// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! HTML dataset export — one parameterized exporter that collapses the two out-of-band admin
//! scripts (`scripts/bundle-html-dataset.sh`, grouped per year-month, and
//! `scripts/bundle-html-dataset-by-severity.sh`, grouped per severity) into a single,
//! dependency-free implementation (no `psql`/`unzip`/`zip`/`egrep` — the `zip` crate is already
//! vendored, so a fresh box needs nothing extra).
//!
//! Both scripts do the same thing: select a corpus/service's tasks of a given severity, pull the
//! main `*.html` out of each task's result archive (`<entry-dir>/<service>.zip`), and bundle the
//! per-paper HTML into ZIP archives. They differ only in **how the papers are bucketed into
//! archives** — by month or by severity — which is the single [`GroupBy`] knob here. The scripts'
//! one naming inconsistency (`no_problem` vs `no-problem` for the same severity) is resolved in
//! favour of the canonical [`TaskStatus::to_key`] spelling (`no_problem`).

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;

use diesel::prelude::*;
use diesel::PgConnection;

use crate::helpers::{result_archive_path, TaskStatus};
use crate::models::{Corpus, Service};

/// How the per-paper HTML files are bucketed into output archives — the sole difference between the
/// two scripts this replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupBy {
  /// One archive per year-month (`<corpus>-<yymm>.zip`, papers at `<yymm>/<paper>.html`) — the
  /// `bundle-html-dataset.sh` layout.
  Month,
  /// One archive per severity (`<corpus>-<severity>.zip`, papers at
  /// `<severity>/<yymm>/<paper>.html`) — the `bundle-html-dataset-by-severity.sh` layout.
  Severity,
}

impl GroupBy {
  /// Parse the CLI/string form (`month` / `severity`).
  pub fn from_key(key: &str) -> Option<Self> {
    match key {
      "month" => Some(GroupBy::Month),
      "severity" => Some(GroupBy::Severity),
      _ => None,
    }
  }
}

/// One produced dataset archive and how many papers it holds.
#[derive(Debug, serde::Serialize)]
pub struct DatasetArchive {
  /// archive file name, e.g. `arxmliv-1808.zip`
  pub name: String,
  /// number of HTML documents bundled into it
  pub entries: usize,
}

/// The result of an [`export_html_dataset`] run — the archives written plus tallies, also the body
/// of the `*-manifest.json` provenance file written alongside them.
#[derive(Debug, serde::Serialize)]
pub struct DatasetExportOutcome {
  /// corpus the dataset was carved from
  pub corpus: String,
  /// service whose HTML output was bundled
  pub service: String,
  /// `month` or `severity`
  pub group_by: String,
  /// the severity keys included (canonical spelling)
  pub severities: Vec<String>,
  /// RFC-3339 UTC time the export finished
  pub generated_at: String,
  /// the `cortex` version that produced it
  pub cortex_version: String,
  /// archives written, in name order
  pub archives: Vec<DatasetArchive>,
  /// total HTML documents bundled across all archives
  pub total_entries: usize,
  /// tasks whose result archive was missing/unreadable or had no HTML — counted, not bundled
  pub skipped: usize,
}

/// One paper's contribution to the dataset: where its HTML comes from and where it goes.
struct Paper {
  /// the task's result archive (`<entry-dir>/<service>.zip`)
  result_zip: PathBuf,
  /// the paper id (the entry directory's name) — the output HTML is named `<paper>.html`
  paper: String,
  /// the year-month bucket (the entry directory's parent name)
  yymm: String,
}

/// Export a corpus/service's converted HTML into ZIP archives bucketed by [`GroupBy`].
///
/// Reads existing result archives off the shared filesystem (no conversion is run); the DB is only
/// queried for the matching task `entry` paths. `progress` is called with human-readable milestone
/// lines (the CLI prints them; a future web/job caller can stream them). Resumable: an archive
/// whose `.zip` already exists in `out_dir` is left untouched (matching the scripts' resume
/// behaviour).
///
/// Severities are taken in the given order; the canonical [`TaskStatus::to_key`] spelling is used
/// throughout (no `no-problem`/`no_problem` ambiguity).
pub fn export_html_dataset(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  severities: &[TaskStatus],
  group_by: GroupBy,
  out_dir: &PathBuf,
  mut progress: impl FnMut(&str),
) -> Result<DatasetExportOutcome, String> {
  use crate::schema::tasks::dsl as t;

  fs::create_dir_all(out_dir).map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;

  // Bucket papers by their archive key. `BTreeMap` keeps a deterministic (sorted) archive order.
  // ponytail: the whole work-list lives in RAM here — bounded by one corpus's size (the lightweight
  // {result_zip, paper, yymm} per task), not the html bytes (those are streamed one at a time
  // during the write phase). The scripts materialised the same list to a text file; for the largest
  // corpus (~1.5M papers ≈ a few hundred MB transient) this is fine. Recorded as KNOWN_ISSUES E-3;
  // upgrade path = a server-side cursor + per-yymm streaming if a corpus outgrows RAM.
  let mut buckets: BTreeMap<String, Vec<Paper>> = BTreeMap::new();
  let mut skipped = 0_usize;

  for status in severities {
    let key = status.to_key();
    let entries: Vec<String> = t::tasks
      .filter(t::corpus_id.eq(corpus.id))
      .filter(t::service_id.eq(service.id))
      .filter(t::status.eq(status.raw()))
      .select(t::entry)
      .order(t::entry.asc())
      .load(connection)
      .map_err(|e| format!("querying {key} tasks failed: {e}"))?;
    progress(&format!("  {key}: {} task(s)", entries.len()));

    for entry in entries {
      // The result archive sits next to the source entry (sandbox-aware, F-6).
      let result_zip = match result_archive_path(&entry, &service.name, corpus.sandbox_id()) {
        Some(path) => path,
        None => {
          skipped += 1;
          continue;
        },
      };
      // Layout: `<base>/<yymm>/<paper>/<service>.zip` ⇒ paper = parent dir name, yymm = its parent.
      let entry_dir = match result_zip.parent() {
        Some(dir) => dir,
        None => {
          skipped += 1;
          continue;
        },
      };
      let paper = entry_dir.file_name().and_then(|s| s.to_str());
      let yymm = entry_dir
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str());
      let (paper, yymm) = match (paper, yymm) {
        (Some(p), Some(y)) => (p.to_string(), y.to_string()),
        _ => {
          skipped += 1;
          continue;
        },
      };
      let archive_key = match group_by {
        GroupBy::Month => yymm.clone(),
        GroupBy::Severity => key.clone(),
      };
      buckets.entry(archive_key).or_default().push(Paper {
        result_zip,
        paper,
        yymm,
      });
    }
  }

  progress(&format!("Bundling {} archive(s)...", buckets.len()));
  let mut archives = Vec::new();
  let mut total_entries = 0_usize;
  for (archive_key, papers) in &buckets {
    let archive_name = format!("{}-{archive_key}.zip", corpus.name);
    let archive_path = out_dir.join(&archive_name);
    if archive_path.exists() {
      // Resume: an already-written archive is kept as-is (the scripts' behaviour).
      progress(&format!("  {archive_name} exists — skipping"));
      continue;
    }
    let (written, html_skipped) = write_archive(&archive_path, papers, group_by, &service.name)?;
    skipped += html_skipped;
    total_entries += written;
    progress(&format!("  {archive_name}: {written} document(s)"));
    archives.push(DatasetArchive {
      name: archive_name,
      entries: written,
    });
  }

  let outcome = DatasetExportOutcome {
    corpus: corpus.name.clone(),
    service: service.name.clone(),
    group_by: match group_by {
      GroupBy::Month => "month",
      GroupBy::Severity => "severity",
    }
    .to_string(),
    severities: severities.iter().map(|s| s.to_key()).collect(),
    generated_at: chrono::Utc::now().to_rfc3339(),
    cortex_version: env!("CARGO_PKG_VERSION").to_string(),
    archives,
    total_entries,
    skipped,
  };

  // Provenance: a manifest recording what this dataset is and how it was built (the owner's
  // "provenance model" — kept simple: a sidecar JSON, not new DB state).
  let manifest_path = out_dir.join(format!("{}-manifest.json", corpus.name));
  let manifest = serde_json::to_string_pretty(&outcome)
    .map_err(|e| format!("serializing manifest failed: {e}"))?;
  fs::write(&manifest_path, manifest)
    .map_err(|e| format!("writing {} failed: {e}", manifest_path.display()))?;
  progress(&format!("Wrote {}", manifest_path.display()));

  Ok(outcome)
}

/// Write one dataset archive: copy each paper's main HTML out of its result archive into a fresh
/// `<archive>.zip`. Returns `(documents_written, skipped)`. A paper whose result archive is
/// missing/unreadable or carries no HTML is skipped (counted), never fatal — one bad paper must not
/// sink a multi-thousand-paper export (DESIGN_PRINCIPLES: isolate blast radius).
fn write_archive(
  archive_path: &PathBuf,
  papers: &[Paper],
  group_by: GroupBy,
  service_name: &str,
) -> Result<(usize, usize), String> {
  let file = File::create(archive_path)
    .map_err(|e| format!("cannot create {}: {e}", archive_path.display()))?;
  let mut zip = zip::ZipWriter::new(file);
  let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
    .compression_method(zip::CompressionMethod::Deflated)
    .compression_level(Some(9));

  let mut written = 0_usize;
  let mut skipped = 0_usize;
  for paper in papers {
    let html = match extract_main_html(&paper.result_zip, &paper.paper) {
      Some(bytes) => bytes,
      None => {
        skipped += 1;
        continue;
      },
    };
    // Internal layout mirrors the scripts: month → `<yymm>/<paper>.html`, severity zips keep the
    // `<severity>/<yymm>/…` nesting (the severity is the archive key for that mode).
    let internal = match group_by {
      GroupBy::Month => format!("{}/{}.html", paper.yymm, paper.paper),
      GroupBy::Severity => {
        // archive key (severity) is the file stem; the path keeps the yymm subdir under it
        let severity = archive_path
          .file_stem()
          .and_then(|s| s.to_str())
          .and_then(|s| s.rsplit('-').next())
          .unwrap_or(service_name);
        format!("{severity}/{}/{}.html", paper.yymm, paper.paper)
      },
    };
    zip
      .start_file(internal, options)
      .map_err(|e| format!("zip start_file failed: {e}"))?;
    zip
      .write_all(&html)
      .map_err(|e| format!("zip write failed: {e}"))?;
    written += 1;
  }
  zip
    .finish()
    .map_err(|e| format!("finalizing {} failed: {e}", archive_path.display()))?;
  Ok((written, skipped))
}

/// Pull the main HTML document out of a result archive. Prefers a root-level `<paper>.html`, then
/// any other root-level `*.html`; returns `None` if the archive is unreadable or HTML-free (the
/// caller counts it as skipped). HTML-only by design — the arXMLiv datasets bundle the document,
/// not its assets, exactly as the scripts did (`unzip *.html`).
fn extract_main_html(result_zip: &PathBuf, paper: &str) -> Option<Vec<u8>> {
  let file = File::open(result_zip).ok()?;
  let mut archive = zip::ZipArchive::new(file).ok()?;
  // Find the best HTML index first (an immutable scan), then read it — `by_index` needs `&mut`.
  let preferred = format!("{paper}.html");
  let mut best: Option<usize> = None;
  for i in 0..archive.len() {
    let entry = archive.by_index(i).ok()?;
    let name = entry.name();
    // Root-level html only (no `/`): the main document, not a nested asset.
    if name.ends_with(".html") && !name.contains('/') {
      if name == preferred {
        best = Some(i);
        break;
      }
      best.get_or_insert(i);
    }
  }
  let index = best?;
  let mut entry = archive.by_index(index).ok()?;
  let mut bytes = Vec::new();
  entry.read_to_end(&mut bytes).ok()?;
  Some(bytes)
}

#[cfg(test)]
mod tests {
  use super::{extract_main_html, GroupBy};
  use std::io::Write;

  fn write_zip(path: &std::path::Path, files: &[(&str, &str)]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
    for (name, body) in files {
      zip.start_file(*name, opts).unwrap();
      zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
  }

  #[test]
  fn group_by_parses_the_two_modes() {
    assert_eq!(GroupBy::from_key("month"), Some(GroupBy::Month));
    assert_eq!(GroupBy::from_key("severity"), Some(GroupBy::Severity));
    assert!(GroupBy::from_key("yearly").is_none());
  }

  #[test]
  fn extract_main_html_prefers_paper_then_root_then_none() {
    let dir = std::env::temp_dir().join("cortex_export_unit_test");
    std::fs::create_dir_all(&dir).unwrap();

    // Prefers the `<paper>.html` root entry over other root html and over nested assets.
    let z1 = dir.join("preferred.zip");
    write_zip(
      &z1,
      &[
        ("assets/nested.html", "NESTED"),
        ("other.html", "OTHER"),
        ("0801.1234.html", "MAIN"),
        ("cortex.log", "Status:conversion:1"),
      ],
    );
    assert_eq!(
      extract_main_html(&z1, "0801.1234").as_deref(),
      Some(b"MAIN".as_slice()),
      "the paper-named root html wins"
    );

    // No `<paper>.html`: falls back to any root-level html (not the nested one).
    let z2 = dir.join("fallback.zip");
    write_zip(&z2, &[("index.html", "ROOT"), ("sub/deep.html", "DEEP")]);
    assert_eq!(
      extract_main_html(&z2, "9999.0001").as_deref(),
      Some(b"ROOT".as_slice()),
      "a root-level html is used when the paper-named one is absent"
    );

    // No root-level html at all ⇒ skipped (None), never a panic.
    let z3 = dir.join("htmlless.zip");
    write_zip(&z3, &[("sub/only.html", "DEEP"), ("cortex.log", "x")]);
    assert!(extract_main_html(&z3, "p").is_none());

    // A non-existent / unreadable archive ⇒ None, not an error.
    assert!(extract_main_html(&dir.join("nope.zip"), "p").is_none());

    std::fs::remove_dir_all(&dir).ok();
  }
}
