// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
use cortex::backend::Backend;
use cortex::importer::Importer;
use cortex::models::Corpus;
use std::env;

/// Extends all corpora registered with the `CorTeX` backend, with any new available sources
/// (example usage: arXiv.org releases new source bundles every month, which warrant an update at
/// the same frequency.)
fn main() {
  // Note that we realize the initial import via a real cortex worker, but use a simply utility
  // script for extensions. this is the case since the goal here is to do a simple sysadmin
  // "maintenance update", rather than a full-blown "semantic" union operation
  let backend = Backend::default();

  // If input is provided, only extend the corpus of the given name/path.
  let mut input_args = env::args();
  let _ = input_args.next();
  let corpora = if let Some(path) = input_args.next() {
    if let Ok(corpus) = Corpus::find_by_path(&path, &backend.connection) {
      vec![corpus]
    } else {
      panic!(
        "No corpus could be found at path {:?}. Make sure path matches DB registration.",
        path
      );
    }
  } else {
    backend.corpora()
  };

  for corpus in corpora {
    // First, build an importer, which will perform the extension
    let importer = Importer {
      corpus: corpus.clone(),
      backend: Backend::default(),
      cwd: Importer::cwd(),
    };

    // Extend the already imported corpus. I prefer that method name to "update", as we won't yet
    // implement downsizing on deletion.
    let extend_start = time::get_time();
    println!("-- Extending: {:?}", corpus.name);
    match importer.extend_corpus() {
      Ok(_) => {},
      Err(e) => println!("Corpus extension panicked: {:?}", e),
    };
    let extend_end = time::get_time();
    let extend_duration = (extend_end - extend_start).num_milliseconds();
    println!(
      "-- Extending corpus {:?} took {:?}ms",
      corpus.name, extend_duration
    );

    // Then re-register all services, so that they pick up on the tasks
    let register_start = time::get_time();
    match corpus.select_services(&backend.connection) {
      Ok(services) => {
        for service in services {
          let service_id = service.id;
          if service_id > 2 {
            println!(
              "   Extending service {:?} on corpus {:?}",
              service.name, corpus.name
            );
            backend.extend_service(&service, &corpus.path).unwrap();
          }
        }
      },
      Err(e) => println!("Services could not be fetched: {:?}", e),
    };
    let register_end = time::get_time();
    let register_duration = (register_end - register_start).num_milliseconds();
    println!(
      "-- Service registration on corpus {:?} took {:?}ms",
      corpus.name, register_duration
    );
  }
}
