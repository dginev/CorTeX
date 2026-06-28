// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
use cortex::backend::Backend;
use cortex::importer::Importer;
use cortex::models::Corpus;
use std::collections::HashSet;
use std::env;
use std::process;

/// Extends corpora registered with the `CorTeX` backend with newly available sources
/// (example usage: arXiv.org releases new source bundles every month, which warrant an update at
/// the same frequency.)
///
/// Usage:
///   extend_corpora [<corpus_path> [<yymm_prefix>]]
///
///   * no args         -> extend every registered corpus (whole tree)
///   * <corpus_path>   -> extend only the corpus registered at that path
///   * <yymm_prefix>   -> additionally scope the import to a single monthly sub-directory (e.g.
///     "2606"), so a monthly release only (re)imports the new month instead of walking every
///     already-processed sub-directory. Recommended for the routine monthly maintenance update.
///
/// Exits NON-ZERO if any corpus extension or service registration fails, so a
/// calling orchestrator can gate on a clean import.
fn main() {
  // Note that we realize the initial import via a real cortex worker, but use a simple utility
  // script for extensions. this is the case since the goal here is to do a simple sysadmin
  // "maintenance update", rather than a full-blown "semantic" union operation
  let mut backend = Backend::default();

  let mut input_args = env::args();
  let _ = input_args.next();
  let path_arg = input_args.next();
  // Optional monthly prefix (e.g. "2606") to restrict the import to the most recent month.
  let only_prefix = input_args.next();

  let corpora = if let Some(ref path) = path_arg {
    match Corpus::find_by_path(path, &mut backend.connection) {
      Ok(corpus) => vec![corpus],
      _ => {
        eprintln!(
          "No corpus could be found at path {path:?}. Make sure path matches DB registration."
        );
        process::exit(1);
      },
    }
  } else {
    backend.corpora()
  };

  let mut failures = 0usize;

  for corpus in corpora {
    // First, build an importer, which will perform the extension
    let mut importer = Importer {
      corpus: corpus.clone(),
      ..Importer::default()
    };

    // Scope the import to a single monthly sub-directory when a prefix is given. Setting
    // `active_prefixes` explicitly (rather than relying on it being a side-effect of unpacking
    // the present *.tar files) guarantees we only import that month, even on a re-run where the
    // tars are already unpacked and the side-effect would otherwise leave the filter empty (=
    // "import everything").
    if let Some(ref prefix) = only_prefix {
      importer.active_prefixes = HashSet::from([prefix.clone()]);
      println!("-- Scoping import to monthly prefix {prefix:?}");
    }

    // Extend the already imported corpus. I prefer that method name to "update", as we won't yet
    // implement downsizing on deletion.
    let extend_start = chrono::Utc::now();
    println!("-- Extending: {:?}", corpus.name);
    if let Err(e) = importer.extend_corpus() {
      eprintln!("-- Corpus extension FAILED for {:?}: {e:?}", corpus.name);
      failures += 1;
      continue; // do not register services on top of a failed import
    }
    let extend_end = chrono::Utc::now();
    let extend_duration = (extend_end - extend_start).num_milliseconds();
    println!(
      "-- Extending corpus {:?} took {:?}ms",
      corpus.name, extend_duration
    );

    // Then re-register all services, so that they pick up on the tasks
    let register_start = chrono::Utc::now();
    match corpus.select_services(&mut backend.connection) {
      Ok(services) => {
        for service in services {
          let service_id = service.id;
          if service_id > 2 {
            println!(
              "   Extending service {:?} on corpus {:?}",
              service.name, corpus.name
            );
            if let Err(e) = backend.extend_service(&service, &corpus.path) {
              eprintln!(
                "-- Service extension FAILED for {:?} on {:?}: {e:?}",
                service.name, corpus.name
              );
              failures += 1;
            }
          }
        }
      },
      Err(e) => {
        eprintln!("Services could not be fetched for {:?}: {e:?}", corpus.name);
        failures += 1;
      },
    };
    let register_end = chrono::Utc::now();
    let register_duration = (register_end - register_start).num_milliseconds();
    println!(
      "-- Service registration on corpus {:?} took {:?}ms",
      corpus.name, register_duration
    );
  }

  if failures > 0 {
    eprintln!("-- extend_corpora finished with {failures} failure(s); exiting non-zero.");
    process::exit(1);
  }
  println!("-- extend_corpora completed successfully.");
}
