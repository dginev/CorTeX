// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Retroactive telemetry aggregation over a completed `(corpus, service)` run.
//!
//! The latexml-oxide `cortex_worker` writes a per-job `telemetry.json` into every result archive
//! (wall/RSS, per-phase microseconds, message + asset counts). This module reads those records back
//! off disk — one random-access `by_name("telemetry.json")` seek per result ZIP, mirroring
//! [`crate::helpers`]'s `read_cortex_log` — and rolls them up into a [`TelemetrySummary`]:
//! nearest-rank wall/RSS percentiles, a per-phase P99 breakdown, an outcome mix, and the slowest /
//! highest-RSS witness papers.
//!
//! It is **read-only and off the dispatch hot path** — the frontend telemetry dashboard
//! ([`crate::frontend::telemetry`]) drives it lazily on a cache miss. Every field of a record is
//! `#[serde(default)]`, because the worker's failure path emits only `paper_id`/`category`/
//! `exit_code`; a missing numeric or array field is simply zero / empty, never a parse error.

use std::path::Path;
use std::thread::available_parallelism;

use serde::{Deserialize, Serialize};

use crate::helpers::result_archive_path;

/// The 17 conversion phases the worker times, in emission order — the index into a
/// [`TelemetryRecord::phase_us`] entry and the label of a per-phase P99 row. Kept in lock-step with
/// the latexml-oxide worker's telemetry writer; a shorter/absent `phase_us` (the failure path)
/// simply contributes nothing to the tail phases.
pub const PHASES: [&str; 17] = [
  "bootstrap",
  "digest",
  "build",
  "rewrite",
  "math_parse",
  "post_xml_parse",
  "post_scan",
  "bibliography",
  "crossref",
  "graphics",
  "math_images",
  "mathml_pres",
  "mathml_cont",
  "split",
  "xslt",
  "html5_fixups",
  "serialize",
];

/// One worker `telemetry.json` record. Every field is `#[serde(default)]`: the worker's failure
/// path emits only `paper_id`/`category`/`exit_code`, so a missing numeric field decodes to `0` and
/// a missing `phase_us` to an empty vector rather than failing the whole parse.
#[derive(Debug, Default, Deserialize)]
pub struct TelemetryRecord {
  /// The document/paper id (e.g. `0801.1234`).
  #[serde(default)]
  pub paper_id: String,
  /// The git sha of the worker binary that produced this record.
  #[serde(default)]
  pub git_sha: String,
  /// The host that ran the conversion.
  #[serde(default)]
  pub host: String,
  /// The worker's own outcome category: `ok` | `conversion_error` | `conversion_fatal`.
  #[serde(default)]
  pub category: String,
  /// The process exit code (`>= 3` fatal, `2` error, lower OK/warnings).
  #[serde(default)]
  pub exit_code: i64,
  /// Total conversion wall time, in microseconds.
  #[serde(default)]
  pub wall_us: u64,
  /// Peak resident set size, in KiB.
  #[serde(default)]
  pub max_rss_kb: u64,
  /// Per-phase wall time in microseconds, aligned to [`PHASES`] (17 entries on the success path,
  /// possibly shorter/empty on the failure path).
  #[serde(default)]
  pub phase_us: Vec<u64>,
  /// Warning-severity message count.
  #[serde(default)]
  pub warnings: u64,
  /// Error-severity message count.
  #[serde(default)]
  pub errors: u64,
  /// Fatal-severity message count.
  #[serde(default)]
  pub fatal_errors: u64,
  /// Number of parsed formulae.
  #[serde(default)]
  pub formulae: u64,
  /// Number of graphics assets emitted.
  #[serde(default)]
  pub graphics_assets: u64,
  /// Serialized output size, in bytes.
  #[serde(default)]
  pub output_bytes: u64,
}

/// Reads and decodes the `telemetry.json` member of a result `.zip`. Mirrors
/// [`crate::helpers`]'s `read_cortex_log`: the pure-Rust `zip` crate's random-access `by_name`
/// seeks straight to `telemetry.json` via the central directory, never decompressing the
/// (potentially large) converted output. Returns the decoded record, or an `Err` describing why it
/// couldn't (a non-zip / corrupt archive, a missing `telemetry.json`, or invalid JSON) — the caller
/// counts that as a skipped sample rather than failing the whole aggregation.
pub fn read_telemetry_json(result: &Path) -> Result<TelemetryRecord, String> {
  let file = std::fs::File::open(result).map_err(|e| format!("cannot open result archive: {e}"))?;
  let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("not a readable zip: {e}"))?;
  let mut entry = archive
    .by_name("telemetry.json")
    .map_err(|e| format!("no telemetry.json entry: {e}"))?;
  let mut raw = Vec::new();
  {
    use std::io::Read;
    entry
      .read_to_end(&mut raw)
      .map_err(|e| format!("reading telemetry.json failed: {e}"))?;
  }
  serde_json::from_slice(&raw).map_err(|e| format!("malformed telemetry.json: {e}"))
}

/// Nearest-rank percentiles of a sample (plus its max), in whatever unit the input carries.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Percentiles {
  /// 50th percentile (median).
  pub p50: u64,
  /// 90th percentile.
  pub p90: u64,
  /// 99th percentile.
  pub p99: u64,
  /// Sample maximum.
  pub max: u64,
}

/// Nearest-rank percentiles of `values` (the copy is sorted; the input may be unsorted). The rank
/// for percentile `p` is `ceil(p/100 * n)` clamped to `[1, n]`, taken at index `rank - 1`. An empty
/// sample yields all-zeros.
pub fn percentiles(values: &[u64]) -> Percentiles {
  let n = values.len();
  if n == 0 {
    return Percentiles::default();
  }
  let mut sorted = values.to_vec();
  sorted.sort_unstable();
  // nearest-rank: rank = ceil(p/100 * n), clamped to [1, n]; value at index rank-1.
  let at = |p: usize| -> u64 {
    let rank = (p * n).div_ceil(100).clamp(1, n);
    sorted[rank - 1]
  };
  Percentiles {
    p50: at(50),
    p90: at(90),
    p99: at(99),
    max: sorted[n - 1],
  }
}

/// The rolled-up telemetry of a completed `(corpus, service)` run — the shared read model for both
/// the telemetry dashboard screen and its agent JSON twin.
#[derive(Debug, Clone, Serialize)]
pub struct TelemetrySummary {
  /// Corpus name.
  pub corpus: String,
  /// Service name.
  pub service: String,
  /// Number of result archives that yielded a telemetry record.
  pub sample_count: usize,
  /// Number of tasks whose result archive was missing / unreadable / lacked telemetry.json.
  pub skipped: usize,
  /// Outcome mix, in canonical order (`no_problem`, `warning`, `error`, `fatal`).
  pub outcome_counts: Vec<(String, u64)>,
  /// Wall-time percentiles, in milliseconds.
  pub wall_ms: Percentiles,
  /// Peak-RSS percentiles, in MiB.
  pub rss_mib: Percentiles,
  /// Per-phase P99 wall time in milliseconds, one entry per [`PHASES`] label (17 entries).
  pub phase_p99_ms: Vec<(String, u64)>,
  /// The slowest paper by wall time — `(paper_id, wall_ms)` — or `None` for an empty sample.
  pub slowest: Option<(String, u64)>,
  /// The highest peak-RSS paper — `(paper_id, rss_mib)` — or `None` for an empty sample.
  pub highest_rss: Option<(String, u64)>,
  /// Total formulae parsed across the run.
  pub total_formulae: u64,
  /// Total graphics assets emitted across the run.
  pub total_graphics_assets: u64,
  /// Total serialized output bytes across the run.
  pub total_output_bytes: u64,
  /// Unix timestamp (seconds) at which this summary was computed.
  pub generated_unix: i64,
}

/// Rolls a slice of telemetry records into a [`TelemetrySummary`]. Pure and DB-free (the reading is
/// [`aggregate`]'s job): outcome bucketing, unit conversions (µs→ms, KiB→MiB), nearest-rank
/// percentiles, per-phase P99, witnesses, and totals. `skipped` is left `0` — [`aggregate`] fills
/// it from the read pass.
pub fn summarize(corpus: &str, service: &str, records: &[TelemetryRecord]) -> TelemetrySummary {
  // Outcome buckets — a record lands in exactly one, most-severe-first (a fatal record with
  // warnings is a fatal, not a warning).
  let (mut no_problem, mut warning, mut error, mut fatal) = (0u64, 0u64, 0u64, 0u64);
  for record in records {
    if record.fatal_errors > 0 || record.category.contains("fatal") || record.exit_code >= 3 {
      fatal += 1;
    } else if record.errors > 0 || record.category == "conversion_error" || record.exit_code == 2 {
      error += 1;
    } else if record.warnings > 0 {
      warning += 1;
    } else {
      no_problem += 1;
    }
  }
  let outcome_counts = vec![
    ("no_problem".to_string(), no_problem),
    ("warning".to_string(), warning),
    ("error".to_string(), error),
    ("fatal".to_string(), fatal),
  ];

  // Wall / RSS percentiles, converted to the display units up front so the percentile ranks over
  // the same integers the witnesses report.
  let wall_ms_values: Vec<u64> = records.iter().map(|record| record.wall_us / 1000).collect();
  let rss_mib_values: Vec<u64> = records
    .iter()
    .map(|record| record.max_rss_kb / 1024)
    .collect();
  let wall_ms = percentiles(&wall_ms_values);
  let rss_mib = percentiles(&rss_mib_values);

  // Per-phase P99 (ms). A record with a short/empty `phase_us` simply doesn't contribute to the
  // phases it lacks (filter_map on the missing index), so the failure path never skews a phase.
  let phase_p99_ms = PHASES
    .iter()
    .enumerate()
    .map(|(index, &name)| {
      let phase_ms: Vec<u64> = records
        .iter()
        .filter_map(|record| record.phase_us.get(index).map(|us| us / 1000))
        .collect();
      (name.to_string(), percentiles(&phase_ms).p99)
    })
    .collect();

  // Witnesses: the extreme papers, reported in the same display units as their percentile tables.
  let slowest = records
    .iter()
    .max_by_key(|record| record.wall_us)
    .map(|record| (record.paper_id.clone(), record.wall_us / 1000));
  let highest_rss = records
    .iter()
    .max_by_key(|record| record.max_rss_kb)
    .map(|record| (record.paper_id.clone(), record.max_rss_kb / 1024));

  // Totals — saturating, so a pathological corpus can never overflow-panic the aggregation.
  let total_formulae = records
    .iter()
    .fold(0u64, |acc, record| acc.saturating_add(record.formulae));
  let total_graphics_assets = records.iter().fold(0u64, |acc, record| {
    acc.saturating_add(record.graphics_assets)
  });
  let total_output_bytes = records
    .iter()
    .fold(0u64, |acc, record| acc.saturating_add(record.output_bytes));

  let generated_unix = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|elapsed| elapsed.as_secs() as i64)
    .unwrap_or(0);

  TelemetrySummary {
    corpus: corpus.to_string(),
    service: service.to_string(),
    sample_count: records.len(),
    skipped: 0,
    outcome_counts,
    wall_ms,
    rss_mib,
    phase_p99_ms,
    slowest,
    highest_rss,
    total_formulae,
    total_graphics_assets,
    total_output_bytes,
    generated_unix,
  }
}

/// Reads one slice of task `entry` paths' result archives into telemetry records, returning the
/// records and the count skipped (missing / unreadable / no telemetry.json). The per-thread body of
/// [`aggregate`].
fn read_chunk(
  chunk: &[String],
  service_name: &str,
  sandbox_id: Option<i32>,
) -> (Vec<TelemetryRecord>, usize) {
  let mut records = Vec::new();
  let mut skipped = 0usize;
  for entry in chunk {
    match result_archive_path(entry, service_name, sandbox_id) {
      Some(path) => match read_telemetry_json(&path) {
        Ok(mut record) => {
          // The worker stamps `paper_id` with the numeric cortex task id, but the dashboard's
          // witness links target `/document/<corpus>/<service>/<name>`, which resolves by the
          // document's short name (e.g. `2605.11315`). Re-key on the entry-derived name so the
          // witness links resolve and read as paper ids, not opaque task ids.
          record.paper_id = crate::helpers::entry_document_name(entry);
          records.push(record);
        },
        Err(_) => skipped += 1,
      },
      None => skipped += 1,
    }
  }
  (records, skipped)
}

/// Aggregates the telemetry of a completed run: reads each task `entry`'s result-archive
/// `telemetry.json` **in parallel** (a bounded `std::thread::scope` pool, `N =
/// available_parallelism` capped at 16, the entries chunked evenly across threads), skipping any
/// archive that is missing / unreadable / lacks a telemetry record, then [`summarize`]s.
/// `sandbox_id` is threaded through [`result_archive_path`] so a **sandbox** corpus reads its own
/// name-scoped archives, not the parent's.
pub fn aggregate(
  corpus_name: &str,
  service_name: &str,
  sandbox_id: Option<i32>,
  entries: Vec<String>,
) -> TelemetrySummary {
  let total = entries.len();
  if total == 0 {
    return summarize(corpus_name, service_name, &[]);
  }
  // Bound the pool: never more threads than cores (cap 16), nor than there are entries.
  let workers = available_parallelism()
    .map(|n| n.get())
    .unwrap_or(1)
    .clamp(1, 16)
    .min(total);
  let chunk_size = total.div_ceil(workers);

  let mut records: Vec<TelemetryRecord> = Vec::new();
  let mut skipped = 0usize;
  std::thread::scope(|scope| {
    // Pair each handle with its chunk length so a panicked worker counts its whole chunk as
    // skipped rather than silently vanishing (fail-safe toward flagging, per DESIGN_PRINCIPLES).
    let handles: Vec<_> = entries
      .chunks(chunk_size)
      .map(|chunk| {
        (
          chunk.len(),
          scope.spawn(move || read_chunk(chunk, service_name, sandbox_id)),
        )
      })
      .collect();
    for (chunk_len, handle) in handles {
      match handle.join() {
        Ok((chunk_records, chunk_skipped)) => {
          records.extend(chunk_records);
          skipped += chunk_skipped;
        },
        Err(_) => skipped += chunk_len,
      }
    }
  });

  let mut summary = summarize(corpus_name, service_name, &records);
  summary.skipped = skipped;
  summary
}

#[cfg(test)]
mod tests {
  use super::{PHASES, TelemetryRecord, percentiles, summarize};

  /// A record with everything zeroed and a full 17-entry `phase_us`, for terse test construction.
  fn record(paper_id: &str) -> TelemetryRecord {
    TelemetryRecord {
      paper_id: paper_id.to_string(),
      phase_us: vec![0; PHASES.len()],
      ..Default::default()
    }
  }

  #[test]
  fn nearest_rank_percentiles_on_a_known_vector() {
    // Ten evenly-spaced values (deliberately unsorted on input — `percentiles` sorts a copy).
    let values = [50, 20, 100, 40, 10, 70, 30, 90, 60, 80];
    let p = percentiles(&values);
    // rank = ceil(p/100 * 10): p50→5→idx4→50, p90→9→idx8→90, p99→ceil(9.9)=10→idx9→100.
    assert_eq!(p.p50, 50);
    assert_eq!(p.p90, 90);
    assert_eq!(p.p99, 100);
    assert_eq!(p.max, 100);
    // Empty sample → all zeros, no panic.
    let empty = percentiles(&[]);
    assert_eq!((empty.p50, empty.p90, empty.p99, empty.max), (0, 0, 0, 0));
  }

  #[test]
  fn summarize_buckets_percentiles_and_phase_length() {
    // One record per outcome, with distinct wall times so the percentiles are predictable.
    let mut clean = record("clean"); // no problems
    clean.wall_us = 1_000_000; // 1000 ms
    clean.max_rss_kb = 1_048_576; // 1024 MiB
    let mut warned = record("warned");
    warned.warnings = 3;
    warned.wall_us = 2_000_000; // 2000 ms
    let mut errored = record("errored");
    errored.errors = 1;
    errored.category = "conversion_error".to_string();
    errored.exit_code = 2;
    errored.wall_us = 3_000_000; // 3000 ms
    let mut fataled = record("fataled");
    fataled.fatal_errors = 1;
    fataled.category = "conversion_fatal".to_string();
    fataled.exit_code = 3;
    fataled.wall_us = 4_000_000; // 4000 ms — the slowest

    let records = [clean, warned, errored, fataled];
    let summary = summarize("c", "s", &records);

    // Outcome buckets, in canonical order, one each.
    assert_eq!(
      summary.outcome_counts,
      vec![
        ("no_problem".to_string(), 1),
        ("warning".to_string(), 1),
        ("error".to_string(), 1),
        ("fatal".to_string(), 1),
      ]
    );
    // Wall percentiles over [1000,2000,3000,4000] ms: p50→idx1→2000, p90/p99→idx3→4000, max→4000.
    assert_eq!(summary.wall_ms.p50, 2000);
    assert_eq!(summary.wall_ms.p90, 4000);
    assert_eq!(summary.wall_ms.p99, 4000);
    assert_eq!(summary.wall_ms.max, 4000);
    // Always one P99 row per phase.
    assert_eq!(summary.phase_p99_ms.len(), 17);
    assert_eq!(summary.phase_p99_ms[0].0, "bootstrap");
    // The slowest witness is the 4000 ms fatal paper.
    assert_eq!(summary.slowest, Some(("fataled".to_string(), 4000)));
    assert_eq!(summary.sample_count, 4);
    assert_eq!(summary.skipped, 0);
  }

  #[test]
  fn minimal_failure_record_parses_and_summarizes() {
    // The worker's failure path emits only these three fields — every numeric/array field is
    // `#[serde(default)]`, so this must parse with an empty `phase_us`.
    let json = r#"{"paper_id":"x","category":"conversion_fatal","exit_code":3}"#;
    let record: TelemetryRecord = serde_json::from_str(json).expect("minimal record parses");
    assert_eq!(record.paper_id, "x");
    assert_eq!(record.exit_code, 3);
    assert!(
      record.phase_us.is_empty(),
      "absent phase_us decodes to empty"
    );

    // summarize must handle the empty phase arrays without panicking, still yielding 17 phase rows
    // (all zero) and classifying the record as fatal.
    let summary = summarize("c", "s", std::slice::from_ref(&record));
    assert_eq!(summary.phase_p99_ms.len(), 17);
    assert!(summary.phase_p99_ms.iter().all(|(_, p99)| *p99 == 0));
    assert_eq!(summary.outcome_counts[3], ("fatal".to_string(), 1));
  }
}
