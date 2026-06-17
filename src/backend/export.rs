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

/// Keyset-pagination page size for the entry scan: at most this many `entry` paths are resident at
/// once (then streamed into archives and dropped). With the per-archive streaming below, that makes
/// the exporter's footprint O(one page + one open zip + one paper's HTML) regardless of corpus size
/// — the fix for the old whole-work-list-in-RAM `BTreeMap` (KNOWN_ISSUES E-3).
const EXPORT_PAGE_SIZE: i64 = 10_000;

/// The shared ZIP entry options for every dataset archive (max-deflate, matching the scripts).
fn zip_options() -> zip::write::FileOptions<'static, ()> {
  zip::write::FileOptions::default()
    .compression_method(zip::CompressionMethod::Deflated)
    .compression_level(Some(9))
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
  fs::create_dir_all(out_dir).map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;

  progress("Streaming HTML dataset export...");
  // Stream the matching papers straight into per-archive ZIPs, holding at most ONE archive open at
  // a time, so the resident footprint is O(page) — not the whole corpus's work-list in a
  // `BTreeMap` (the old KNOWN_ISSUES E-3 gap). Both modes feed the streamer with same-archive-key
  // entries contiguous, which is the streamer's one requirement.
  let mut streamer = ArchiveStreamer::new(out_dir, corpus, &service.name, group_by);

  match group_by {
    // Month: ONE keyset-paginated pass over *all* requested severities, ordered by `entry`. Since
    // `entry` is `<base>/<yymm>/<paper>/…`, that order makes each `yymm` (the archive key) a
    // contiguous run — even when one month's papers span several severities.
    GroupBy::Month => {
      let raws: Vec<i32> = severities.iter().map(|s| s.raw()).collect();
      let mut after: Option<String> = None;
      loop {
        let page = fetch_entry_page(connection, corpus.id, service.id, &raws, after.as_deref())?;
        let full = page.len() as i64 == EXPORT_PAGE_SIZE;
        for entry in &page {
          streamer.feed(entry, None, &mut progress)?;
        }
        after = page.into_iter().next_back();
        if !full {
          break;
        }
      }
    },
    // Severity: one archive per severity, so process a severity at a time — its `entry`-ordered
    // pages stream into that single archive (the per-severity loop *is* the contiguity).
    GroupBy::Severity => {
      for status in severities {
        let key = status.to_key();
        let raws = [status.raw()];
        let mut after: Option<String> = None;
        loop {
          let page = fetch_entry_page(connection, corpus.id, service.id, &raws, after.as_deref())?;
          let full = page.len() as i64 == EXPORT_PAGE_SIZE;
          for entry in &page {
            streamer.feed(entry, Some(&key), &mut progress)?;
          }
          after = page.into_iter().next_back();
          if !full {
            break;
          }
        }
      }
    },
  }

  let (archives, total_entries, skipped) = streamer.finish(&mut progress)?;

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

/// One keyset page of matching `entry` paths, ordered by `entry`, strictly after the `after` cursor
/// (the last entry of the previous page). `entry` is unique within a `(corpus, service)` (the
/// `UNIQUE(entry, service, corpus)` constraint), so it is a safe, gap-free keyset cursor — O(log n)
/// per page, no deep-`OFFSET` scan-and-discard. Bounded to [`EXPORT_PAGE_SIZE`] rows.
fn fetch_entry_page(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  status_raws: &[i32],
  after: Option<&str>,
) -> Result<Vec<String>, String> {
  use crate::schema::tasks::dsl as t;
  let mut query = t::tasks
    .filter(t::corpus_id.eq(corpus_id))
    .filter(t::service_id.eq(service_id))
    .filter(t::status.eq_any(status_raws.to_vec()))
    .select(t::entry)
    .order(t::entry.asc())
    .into_boxed();
  if let Some(cursor) = after {
    query = query.filter(t::entry.gt(cursor.to_string()));
  }
  query
    .limit(EXPORT_PAGE_SIZE)
    .load(connection)
    .map_err(|e| format!("querying export tasks failed: {e}"))
}

/// The one archive currently being written by the [`ArchiveStreamer`].
struct OpenArchive {
  /// archive key (a `yymm` in month mode, a severity in severity mode)
  key: String,
  /// the on-disk file name (`<corpus>-<key>.zip`)
  name: String,
  /// the open ZIP writer
  zip: zip::ZipWriter<File>,
  /// documents written into it so far
  written: usize,
}

/// Streams papers into per-archive-key ZIPs while holding **at most one archive open** at a time,
/// so the exporter's resident memory is O(one open zip + one paper's HTML) regardless of corpus
/// size (the fix for the old whole-work-list `BTreeMap` — KNOWN_ISSUES E-3). Callers must feed
/// entries so that all papers sharing an archive key arrive contiguously; [`export_html_dataset`]
/// guarantees that in both [`GroupBy`] modes. Resume parity with the scripts: a key whose `.zip`
/// already exists is skipped without opening a writer (its papers are never read). A paper whose
/// result archive is missing/unreadable or carries no HTML is skipped (counted), never fatal — one
/// bad paper must not sink a multi-thousand-paper export (DESIGN_PRINCIPLES: isolate blast radius).
struct ArchiveStreamer<'a> {
  out_dir: &'a PathBuf,
  corpus: &'a Corpus,
  service_name: &'a str,
  group_by: GroupBy,
  /// the archive currently open for writing (`None` before the first paper / while skipping)
  open: Option<OpenArchive>,
  /// the key currently being skipped because its `.zip` already exists (resume)
  skipping: Option<String>,
  archives: Vec<DatasetArchive>,
  total: usize,
  skipped: usize,
}

impl<'a> ArchiveStreamer<'a> {
  fn new(
    out_dir: &'a PathBuf,
    corpus: &'a Corpus,
    service_name: &'a str,
    group_by: GroupBy,
  ) -> Self {
    ArchiveStreamer {
      out_dir,
      corpus,
      service_name,
      group_by,
      open: None,
      skipping: None,
      archives: Vec::new(),
      total: 0,
      skipped: 0,
    }
  }

  /// Route one task `entry` to its archive: derive `(result_zip, paper, yymm)`, rotate the open
  /// archive if the key changed, then copy the paper's main HTML in (or count a skip).
  /// `key_override` forces the archive key (severity mode); month mode passes `None` and uses the
  /// `yymm`.
  fn feed(
    &mut self,
    entry: &str,
    key_override: Option<&str>,
    progress: &mut impl FnMut(&str),
  ) -> Result<(), String> {
    // The result archive sits next to the source entry (sandbox-aware, F-6).
    let result_zip = match result_archive_path(entry, self.service_name, self.corpus.sandbox_id()) {
      Some(path) => path,
      None => {
        self.skipped += 1;
        return Ok(());
      },
    };
    // Layout: `<base>/<yymm>/<paper>/<service>.zip` ⇒ paper = parent dir name, yymm = its parent.
    let entry_dir = match result_zip.parent() {
      Some(dir) => dir,
      None => {
        self.skipped += 1;
        return Ok(());
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
        self.skipped += 1;
        return Ok(());
      },
    };
    let archive_key = match key_override {
      Some(k) => k.to_string(),
      None => yymm.clone(),
    };
    self.rotate_to(&archive_key, progress)?;
    // Resume: the archive for this key already existed → its papers are left untouched.
    let internal = match self.group_by {
      GroupBy::Month => format!("{yymm}/{paper}.html"),
      GroupBy::Severity => format!("{archive_key}/{yymm}/{paper}.html"),
    };
    let html = match extract_main_html(&result_zip, &paper) {
      Some(bytes) => bytes,
      None => {
        // Only count an HTML miss when we'd actually be writing (not while resume-skipping a key).
        if self.open.is_some() {
          self.skipped += 1;
        }
        return Ok(());
      },
    };
    let Some(open) = self.open.as_mut() else {
      return Ok(());
    };
    open
      .zip
      .start_file(internal, zip_options())
      .map_err(|e| format!("zip start_file failed: {e}"))?;
    open
      .zip
      .write_all(&html)
      .map_err(|e| format!("zip write failed: {e}"))?;
    open.written += 1;
    self.total += 1;
    Ok(())
  }

  /// Switch the open archive to `key` when the streamed key changes: close the previous archive,
  /// then either open a fresh writer or (resume) mark the key skipped because its `.zip` exists.
  fn rotate_to(&mut self, key: &str, progress: &mut impl FnMut(&str)) -> Result<(), String> {
    let active = self
      .open
      .as_ref()
      .map(|o| o.key.as_str())
      .or(self.skipping.as_deref());
    if active == Some(key) {
      return Ok(());
    }
    self.close_open(progress)?;
    self.skipping = None;
    let name = format!("{}-{key}.zip", self.corpus.name);
    let path = self.out_dir.join(&name);
    if path.exists() {
      // Resume: an already-written archive is kept as-is (the scripts' behaviour).
      progress(&format!("  {name} exists — skipping"));
      self.skipping = Some(key.to_string());
    } else {
      // Atomic publish: write to `<name>.partial` and rename to the final name only after the zip
      // is finalized (`close_open`). A crash mid-write then leaves a `.partial`, **not** a
      // truncated `.zip` — so the resume (which skips by final-name existence) re-writes it
      // instead of treating a corrupt half-archive as done. `File::create` truncates any
      // `.partial` orphaned by a prior crash, so the re-write starts clean.
      let tmp_path = self.out_dir.join(format!("{name}.partial"));
      let file = File::create(&tmp_path)
        .map_err(|e| format!("cannot create {}: {e}", tmp_path.display()))?;
      self.open = Some(OpenArchive {
        key: key.to_string(),
        name,
        zip: zip::ZipWriter::new(file),
        written: 0,
      });
    }
    Ok(())
  }

  /// Finalize the currently-open archive (if any), recording it in the outcome.
  fn close_open(&mut self, progress: &mut impl FnMut(&str)) -> Result<(), String> {
    if let Some(open) = self.open.take() {
      let file = open
        .zip
        .finish()
        .map_err(|e| format!("finalizing {} failed: {e}", open.name))?;
      drop(file); // close the handle before the rename (atomic publish)
                  // Publish atomically: now that the central directory is written, rename `<name>.partial` →
                  // `<name>` — only a complete archive ever gets the final name (the resume's skip key).
      let tmp_path = self.out_dir.join(format!("{}.partial", open.name));
      let final_path = self.out_dir.join(&open.name);
      fs::rename(&tmp_path, &final_path)
        .map_err(|e| format!("publishing {} failed: {e}", open.name))?;
      progress(&format!("  {}: {} document(s)", open.name, open.written));
      self.archives.push(DatasetArchive {
        name: open.name,
        entries: open.written,
      });
    }
    Ok(())
  }

  /// Close the last open archive and return `(archives, total_documents, skipped)`.
  fn finish(
    mut self,
    progress: &mut impl FnMut(&str),
  ) -> Result<(Vec<DatasetArchive>, usize, usize), String> {
    self.close_open(progress)?;
    Ok((self.archives, self.total, self.skipped))
  }
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
