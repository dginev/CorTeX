// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! End-to-end contract test for the HTML dataset exporter (`cortex export-dataset`,
//! `backend::export_html_dataset`) — the replacement for the `bundle-html-dataset*.sh` scripts.
//! DB-only (no Rocket Client), so it runs under the default libtest harness.

use std::io::{Read, Write};

use cortex::backend::{self, export_html_dataset, GroupBy};
use cortex::helpers::TaskStatus;
use cortex::models::{Corpus, NewCorpus, NewService, NewTask, Service};

/// Build a result archive `<dir>/tex_to_html.zip` carrying a single `<paper>.html`.
fn write_result_zip(dir: &std::path::Path, paper: &str, body: &str) {
  std::fs::create_dir_all(dir).unwrap();
  let mut zip = zip::ZipWriter::new(std::fs::File::create(dir.join("tex_to_html.zip")).unwrap());
  let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
  zip.start_file(format!("{paper}.html"), opts).unwrap();
  zip.write_all(body.as_bytes()).unwrap();
  zip.start_file("cortex.log", opts).unwrap();
  zip.write_all(b"Status:conversion:1\n").unwrap();
  zip.finish().unwrap();
}

/// Read one entry out of a produced dataset archive (None if absent).
fn read_zip_entry(archive: &std::path::Path, name: &str) -> Option<String> {
  let file = std::fs::File::open(archive).ok()?;
  let mut zip = zip::ZipArchive::new(file).ok()?;
  let mut entry = zip.by_name(name).ok()?;
  let mut s = String::new();
  entry.read_to_string(&mut s).ok()?;
  Some(s)
}

#[test]
fn exports_html_into_month_and_severity_archives() {
  let corpus_name = "export_capability_test";
  let service_name = "tex_to_html";
  let mut db = backend::testdb();

  // Clean any prior run (corpus + its tasks, orphan-free).
  if let Ok(prior) = Corpus::find_by_name(corpus_name, &mut db.connection) {
    prior
      .destroy(&mut db.connection)
      .expect("clear prior corpus");
  }

  // A corpus rooted at a temp base, laid out as `<base>/<yymm>/<paper>/…` (the arXiv topology the
  // scripts assume). The source entry need not exist on disk — the exporter only reads the result
  // archive that sits beside it.
  let base = std::env::temp_dir().join("cortex_export_test_base");
  let _ = std::fs::remove_dir_all(&base);
  let paper = "0801.1234";
  let yymm = "1808";
  let entry_dir = base.join(yymm).join(paper);
  write_result_zip(&entry_dir, paper, "MAIN HTML");

  db.add(&NewCorpus {
    name: corpus_name.to_string(),
    path: base.to_string_lossy().into_owned(),
    complex: true,
    description: "export test".to_string(),
  })
  .expect("insert corpus");
  let corpus = Corpus::find_by_name(corpus_name, &mut db.connection).unwrap();
  // `tex_to_html` is a real service name that may already be seeded — find-or-insert.
  let service = match Service::find_by_name(service_name, &mut db.connection) {
    Ok(service) => service,
    Err(_) => {
      db.add(&NewService {
        name: service_name.to_string(),
        version: 0.1,
        inputformat: "tex".to_string(),
        outputformat: "html".to_string(),
        inputconverter: None,
        complex: true,
        description: "d".to_string(),
      })
      .expect("insert service");
      Service::find_by_name(service_name, &mut db.connection).unwrap()
    },
  };
  db.add(&NewTask {
    service_id: service.id,
    corpus_id: corpus.id,
    status: TaskStatus::NoProblem.raw(),
    entry: entry_dir
      .join(format!("{paper}.zip"))
      .to_string_lossy()
      .into_owned(),
  })
  .expect("insert task");

  // --- group by month ---
  let out_month = std::env::temp_dir().join("cortex_export_test_out_month");
  let _ = std::fs::remove_dir_all(&out_month);
  let outcome = export_html_dataset(
    &mut db.connection,
    &corpus,
    &service,
    &[TaskStatus::NoProblem],
    GroupBy::Month,
    &out_month,
    |_| {},
  )
  .expect("month export succeeds");
  assert_eq!(outcome.total_entries, 1, "one document bundled");
  assert_eq!(outcome.skipped, 0);
  let month_archive = out_month.join(format!("{corpus_name}-{yymm}.zip"));
  assert!(month_archive.exists(), "per-month archive written");
  assert_eq!(
    read_zip_entry(&month_archive, &format!("{yymm}/{paper}.html")).as_deref(),
    Some("MAIN HTML"),
    "month archive holds <yymm>/<paper>.html with the converted body"
  );
  assert!(
    out_month
      .join(format!("{corpus_name}-manifest.json"))
      .exists(),
    "a provenance manifest is written"
  );

  // --- group by severity (note: canonical `no_problem`, never `no-problem`) ---
  let out_sev = std::env::temp_dir().join("cortex_export_test_out_sev");
  let _ = std::fs::remove_dir_all(&out_sev);
  export_html_dataset(
    &mut db.connection,
    &corpus,
    &service,
    &[TaskStatus::NoProblem],
    GroupBy::Severity,
    &out_sev,
    |_| {},
  )
  .expect("severity export succeeds");
  let sev_archive = out_sev.join(format!("{corpus_name}-no_problem.zip"));
  assert!(sev_archive.exists(), "per-severity archive written");
  assert_eq!(
    read_zip_entry(&sev_archive, &format!("no_problem/{yymm}/{paper}.html")).as_deref(),
    Some("MAIN HTML"),
    "severity archive nests <severity>/<yymm>/<paper>.html"
  );

  // --- resume: a second run leaves an existing archive untouched ---
  let again = export_html_dataset(
    &mut db.connection,
    &corpus,
    &service,
    &[TaskStatus::NoProblem],
    GroupBy::Month,
    &out_month,
    |_| {},
  )
  .expect("re-run succeeds");
  assert_eq!(
    again.total_entries, 0,
    "the already-present archive is skipped on re-run (resumable)"
  );

  // cleanup — `destroy` removes the corpus + its tasks + log rows in one transaction (the shared
  // `tex_to_html` service row is intentionally left alone).
  corpus
    .destroy(&mut db.connection)
    .expect("clean up test corpus");
  let _ = std::fs::remove_dir_all(&base);
  let _ = std::fs::remove_dir_all(&out_month);
  let _ = std::fs::remove_dir_all(&out_sev);
}
