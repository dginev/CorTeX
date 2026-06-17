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
    None,
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
    None,
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
    None,
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

/// Stages one `<base>/<yymm>/<paper>/<paper>.zip` task with a result archive carrying
/// `<paper>.html`.
fn stage_paper(
  db: &mut backend::Backend,
  corpus: &Corpus,
  service: &Service,
  base: &std::path::Path,
  yymm: &str,
  paper: &str,
  status: TaskStatus,
) {
  let entry_dir = base.join(yymm).join(paper);
  write_result_zip(&entry_dir, paper, &format!("HTML {paper}"));
  db.add(&NewTask {
    service_id: service.id,
    corpus_id: corpus.id,
    status: status.raw(),
    entry: entry_dir
      .join(format!("{paper}.zip"))
      .to_string_lossy()
      .into_owned(),
  })
  .expect("insert task");
}

/// The streaming rewrite (KNOWN_ISSUES E-3) must handle the cases the single-paper test can't: many
/// papers across **several `yymm`s** (the per-archive rotation as the key changes mid-stream) and a
/// month archive that **aggregates papers from more than one severity** (the single cross-severity
/// `ORDER BY entry` pass must keep each `yymm` contiguous). Layout:
///   1801 → 1801.001 (no_problem) + 1801.002 (warning)   [two severities, one month]
///   1802 → 1802.001 (no_problem)
#[test]
fn streams_multiple_months_and_severities() {
  let corpus_name = "export_streaming_test";
  let service_name = "tex_to_html";
  let mut db = backend::testdb();
  if let Ok(prior) = Corpus::find_by_name(corpus_name, &mut db.connection) {
    prior.destroy(&mut db.connection).expect("clear prior");
  }

  let base = std::env::temp_dir().join("cortex_export_streaming_base");
  let _ = std::fs::remove_dir_all(&base);

  db.add(&NewCorpus {
    name: corpus_name.to_string(),
    path: base.to_string_lossy().into_owned(),
    complex: true,
    description: "streaming export test".to_string(),
  })
  .expect("insert corpus");
  let corpus = Corpus::find_by_name(corpus_name, &mut db.connection).unwrap();
  let service = match Service::find_by_name(service_name, &mut db.connection) {
    Ok(s) => s,
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

  stage_paper(
    &mut db,
    &corpus,
    &service,
    &base,
    "1801",
    "1801.001",
    TaskStatus::NoProblem,
  );
  stage_paper(
    &mut db,
    &corpus,
    &service,
    &base,
    "1801",
    "1801.002",
    TaskStatus::Warning,
  );
  stage_paper(
    &mut db,
    &corpus,
    &service,
    &base,
    "1802",
    "1802.001",
    TaskStatus::NoProblem,
  );

  // --- month: one archive per yymm; 1801 aggregates BOTH severities ---
  let out_month = std::env::temp_dir().join("cortex_export_streaming_month");
  let _ = std::fs::remove_dir_all(&out_month);
  let outcome = export_html_dataset(
    &mut db.connection,
    &corpus,
    &service,
    &[TaskStatus::NoProblem, TaskStatus::Warning],
    GroupBy::Month,
    None,
    &out_month,
    |_| {},
  )
  .expect("month export");
  assert_eq!(outcome.total_entries, 3, "all three papers bundled");
  assert_eq!(outcome.skipped, 0);
  assert_eq!(
    outcome.archives.len(),
    2,
    "one archive per yymm (1801, 1802)"
  );
  let a1801 = out_month.join(format!("{corpus_name}-1801.zip"));
  assert_eq!(
    read_zip_entry(&a1801, "1801/1801.001.html").as_deref(),
    Some("HTML 1801.001"),
    "1801 archive holds the no_problem paper"
  );
  assert_eq!(
    read_zip_entry(&a1801, "1801/1801.002.html").as_deref(),
    Some("HTML 1801.002"),
    "1801 archive ALSO holds the warning paper — cross-severity month aggregation"
  );
  assert_eq!(
    read_zip_entry(
      &out_month.join(format!("{corpus_name}-1802.zip")),
      "1802/1802.001.html"
    )
    .as_deref(),
    Some("HTML 1802.001"),
    "1802 archive holds its own paper after the streamer rotated off 1801"
  );

  // --- severity: one archive per severity, spanning yymms ---
  let out_sev = std::env::temp_dir().join("cortex_export_streaming_sev");
  let _ = std::fs::remove_dir_all(&out_sev);
  let sev = export_html_dataset(
    &mut db.connection,
    &corpus,
    &service,
    &[TaskStatus::NoProblem, TaskStatus::Warning],
    GroupBy::Severity,
    None,
    &out_sev,
    |_| {},
  )
  .expect("severity export");
  assert_eq!(sev.total_entries, 3);
  let np = out_sev.join(format!("{corpus_name}-no_problem.zip"));
  assert_eq!(
    read_zip_entry(&np, "no_problem/1801/1801.001.html").as_deref(),
    Some("HTML 1801.001")
  );
  assert_eq!(
    read_zip_entry(&np, "no_problem/1802/1802.001.html").as_deref(),
    Some("HTML 1802.001"),
    "the no_problem archive spans both months"
  );
  assert_eq!(
    read_zip_entry(
      &out_sev.join(format!("{corpus_name}-warning.zip")),
      "warning/1801/1801.002.html"
    )
    .as_deref(),
    Some("HTML 1801.002")
  );

  corpus.destroy(&mut db.connection).expect("clean up");
  let _ = std::fs::remove_dir_all(&base);
  let _ = std::fs::remove_dir_all(&out_month);
  let _ = std::fs::remove_dir_all(&out_sev);
}

/// Configurable chunking: a per-archive MB cap splits one bucket into numbered chunks
/// `<corpus>-<key>-NNN.zip`; with no cap the bucket stays a single archive. Three ~600 KB papers in
/// one month + a 1 MB cap ⇒ 3 chunks (each pair exceeds the cap, so each paper rolls a new chunk).
#[test]
fn chunks_a_large_bucket_by_the_configured_mb_cap() {
  let corpus_name = "export_chunk_test";
  let service_name = "tex_to_html";
  let mut db = backend::testdb();
  if let Ok(prior) = Corpus::find_by_name(corpus_name, &mut db.connection) {
    prior.destroy(&mut db.connection).expect("clear prior");
  }
  let base = std::env::temp_dir().join("cortex_export_chunk_base");
  let _ = std::fs::remove_dir_all(&base);
  db.add(&NewCorpus {
    name: corpus_name.to_string(),
    path: base.to_string_lossy().into_owned(),
    complex: true,
    description: "chunk test".to_string(),
  })
  .expect("insert corpus");
  let corpus = Corpus::find_by_name(corpus_name, &mut db.connection).unwrap();
  let service = match Service::find_by_name(service_name, &mut db.connection) {
    Ok(s) => s,
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

  // Three ~600 KB papers in one month: 600 KB + 600 KB > 1 MB, so each paper opens its own chunk.
  let big = "x".repeat(600_000);
  for paper in ["2401.001", "2401.002", "2401.003"] {
    let entry_dir = base.join("2401").join(paper);
    write_result_zip(&entry_dir, paper, &big);
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
  }

  let out = std::env::temp_dir().join("cortex_export_chunk_out");
  let _ = std::fs::remove_dir_all(&out);
  let outcome = export_html_dataset(
    &mut db.connection,
    &corpus,
    &service,
    &[TaskStatus::NoProblem],
    GroupBy::Month,
    Some(1),
    &out,
    |_| {},
  )
  .expect("chunked export");
  assert_eq!(outcome.total_entries, 3, "all three papers bundled");
  assert_eq!(
    outcome.max_archive_mb,
    Some(1),
    "the limit is recorded for resume parity"
  );
  assert_eq!(
    outcome.archives.len(),
    3,
    "1 MB cap split the ~1.8 MB month into 3 chunks"
  );
  for n in 1..=3 {
    let chunk = out.join(format!("{corpus_name}-2401-{n:03}.zip"));
    assert!(chunk.exists(), "chunk {n:03} written: {}", chunk.display());
  }

  // No cap → the same month is a single, un-suffixed archive (backward-compatible naming).
  let out_nocap = std::env::temp_dir().join("cortex_export_chunk_nocap");
  let _ = std::fs::remove_dir_all(&out_nocap);
  let plain = export_html_dataset(
    &mut db.connection,
    &corpus,
    &service,
    &[TaskStatus::NoProblem],
    GroupBy::Month,
    None,
    &out_nocap,
    |_| {},
  )
  .expect("uncapped export");
  assert_eq!(
    plain.archives.len(),
    1,
    "no cap → one archive for the bucket"
  );
  assert!(
    out_nocap.join(format!("{corpus_name}-2401.zip")).exists(),
    "unchunked archive keeps the original <corpus>-<key>.zip name"
  );

  corpus.destroy(&mut db.connection).expect("cleanup");
  let _ = std::fs::remove_dir_all(&base);
  let _ = std::fs::remove_dir_all(&out);
  let _ = std::fs::remove_dir_all(&out_nocap);
}
