// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Telemetry-dashboard capability: a read-only, retroactive rollup of the per-job `telemetry.json`
//! the latexml-oxide `cortex_worker` writes into every result archive, across a completed
//! `(corpus, service)` run.
//!
//! Follows the symmetry contract — one shared [`crate::telemetry::TelemetrySummary`] is the read
//! model for both the agent API (`GET /api/telemetry/<corpus>/<service>`) and the server-rendered
//! human screen ([`telemetry_report_page`], `GET /telemetry/<corpus>/<service>`). Both live at
//! top-level `/telemetry` / `/api/telemetry` prefixes (like the `/document/...` report screens) so
//! neither collides with the same-shape `/corpus/<c>/<s>/<severity>` report routes. Because a
//! summary aggregates thousands of result
//! archives off disk (a multi-second scan), it is memoized in an in-memory [`TelemetryCache`] with
//! a short TTL rather than recomputed per request. The heavy read happens **off** both the cache
//! lock and the pooled DB connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::{Template, context};

use crate::backend::DbPool;
use crate::models::{Corpus, Service, Task};
use crate::telemetry::TelemetrySummary;

/// How long a computed telemetry summary stays fresh before the next view recomputes it (5
/// minutes).
const TELEMETRY_TTL: Duration = Duration::from_secs(300);

/// The cache's inner map: `(corpus_id, service_id)` → (when it was computed, the shared summary).
type TelemetryCacheMap = HashMap<(i32, i32), (Instant, Arc<TelemetrySummary>)>;

/// In-memory TTL cache of computed telemetry summaries, keyed by `(corpus_id, service_id)`. A
/// summary aggregates every result archive of a completed run (a multi-second disk scan over
/// thousands of ZIPs), so it is memoized for [`TELEMETRY_TTL`] rather than recomputed per request.
/// A plain `Mutex<HashMap>` suffices — the entry set is tiny (one per viewed scope) and writes are
/// rare; the heavy aggregation runs outside the lock.
#[derive(Default)]
pub struct TelemetryCache(Mutex<TelemetryCacheMap>);

/// Resolves a `(corpus, service)` name pair to its records, mapping each miss to `404`.
fn resolve(
  corpus: &str,
  service: &str,
  connection: &mut diesel::PgConnection,
) -> Result<(Corpus, Service), Status> {
  let corpus = Corpus::find_by_name(corpus, connection).map_err(|_| Status::NotFound)?;
  let service = Service::find_by_name(service, connection).map_err(|_| Status::NotFound)?;
  Ok((corpus, service))
}

/// Returns the cached telemetry summary for a `(corpus, service)`, recomputing it on a cold/stale
/// miss. On a miss it resolves the pair, enumerates the completed run's task entries, releases the
/// pooled connection, and aggregates the result-archive telemetry off disk before caching. `404` on
/// an unknown corpus/service, `503` if the pool is exhausted.
fn cached_summary(
  corpus: &str,
  service: &str,
  pool: &State<DbPool>,
  cache: &State<TelemetryCache>,
) -> Result<Arc<TelemetrySummary>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let key = (corpus.id, service.id);
  // Fast path: a fresh cache hit serves without touching disk. A poisoned lock is a `500`, never a
  // panic on the request path (DESIGN_PRINCIPLES).
  {
    let cached = cache.0.lock().map_err(|_| Status::InternalServerError)?;
    if let Some((computed_at, summary)) = cached.get(&key)
      && computed_at.elapsed() < TELEMETRY_TTL
    {
      return Ok(summary.clone());
    }
  }
  // Cold/stale: enumerate the entries, then release the pooled connection BEFORE the multi-second
  // ZIP scan so the aggregation never pins a pooled connection (P-2/P-4 discipline).
  let entries = Task::completed_entries(corpus.id, service.id, &mut connection)
    .map_err(|_| Status::InternalServerError)?;
  drop(connection);
  let summary = Arc::new(crate::telemetry::aggregate(
    &corpus.name,
    &service.name,
    corpus.sandbox_id(),
    entries,
  ));
  // Store under the lock (last writer wins; a concurrent miss recomputing the same scope is
  // acceptable and rare).
  cache
    .0
    .lock()
    .map_err(|_| Status::InternalServerError)?
    .insert(key, (Instant::now(), summary.clone()));
  Ok(summary)
}

/// The telemetry rollup as an agent API (the JSON twin of [`telemetry_report_page`]): wall/RSS
/// percentiles, per-phase P99, outcome mix, and witness papers for a completed `(corpus, service)`
/// run. Served from the shared [`TelemetryCache`]. `404` on an unknown corpus/service, `503` if the
/// pool is exhausted.
#[get("/api/telemetry/<corpus>/<service>")]
pub fn api_telemetry(
  corpus: &str,
  service: &str,
  pool: &State<DbPool>,
  cache: &State<TelemetryCache>,
) -> Result<Json<TelemetrySummary>, Status> {
  // The cache hands back a shared `Arc`; clone the (small) inner summary to hand serde an owned
  // value — `serde` isn't built with the `rc` feature, so `Arc<T>` itself isn't `Serialize`.
  let summary = cached_summary(corpus, service, pool, cache)?;
  Ok(Json((*summary).clone()))
}

/// The telemetry dashboard **screen** (HTML twin of [`api_telemetry`]): the same rolled-up run
/// telemetry, rendered as an outcome mix, wall/RSS percentile tables, a per-phase P99 bar list, and
/// the slowest / highest-RSS witness papers (each linking into its per-article forensics). `404` on
/// an unknown corpus/service, `503` if the pool is exhausted.
#[get("/telemetry/<corpus>/<service>")]
pub fn telemetry_report_page(
  corpus: &str,
  service: &str,
  pool: &State<DbPool>,
  cache: &State<TelemetryCache>,
) -> Result<Template, Status> {
  let summary = cached_summary(corpus, service, pool, cache)?;
  let global = serde_json::json!({
    "title": format!("Telemetry · {}/{}", summary.corpus, summary.service),
    "description": "Per-run conversion telemetry: latency, memory, and per-phase profile",
  });
  // Hand the template an owned clone of the summary (serde has no `rc` feature, so the shared `Arc`
  // isn't `Serialize`); the summary is small, and telemetry views are cache-served and infrequent.
  let summary = (*summary).clone();
  Ok(Template::render(
    "telemetry-report",
    context! { global, summary },
  ))
}

/// The route set for the telemetry-dashboard capability (the agent API + the human screen).
pub fn routes() -> Vec<Route> { routes![api_telemetry, telemetry_report_page] }

#[cfg(test)]
mod tests {
  use crate::telemetry::{
    MathStats, OutcomeWall, PHASES, Percentiles, RssBuckets, TailStats, TelemetrySummary,
  };
  use rocket_dyn_templates::tera::{Context, Tera, Value};
  use std::collections::HashMap;

  /// Render the real `layout` + `telemetry-report` templates against a representative summary, so a
  /// Tera syntax error or a context-shape mismatch in the hand-written template is caught by the
  /// suite rather than only at first request. Runs from the repo root (CWD-coupled, like the rest
  /// of the frontend).
  #[test]
  fn telemetry_report_template_renders() {
    let layout = std::fs::read_to_string("templates/layout.html.tera")
      .expect("layout template present (run tests from the repo root)");
    let page = std::fs::read_to_string("templates/telemetry-report.html.tera")
      .expect("telemetry-report template present");
    let mut tera = Tera::default();
    tera
      .add_raw_templates(vec![
        ("layout", layout.as_str()),
        ("telemetry-report", page.as_str()),
      ])
      .expect("templates parse and their inheritance resolves");
    // The layout/report use the app's custom `group_thousands` filter; a passthrough stand-in keeps
    // this a pure template-shape test (the filter itself is covered in `server.rs`).
    tera.register_filter(
      "group_thousands",
      |value: &Value, _: &HashMap<String, Value>| Ok(value.clone()),
    );

    let summary = TelemetrySummary {
      corpus: "arxiv".to_string(),
      service: "tex_to_html".to_string(),
      sample_count: 3,
      skipped: 1,
      outcome_counts: vec![
        ("no_problem".to_string(), 2),
        ("warning".to_string(), 1),
        ("error".to_string(), 0),
        ("fatal".to_string(), 0),
      ],
      wall_ms: Percentiles {
        p50: 100,
        p90: 200,
        p99: 300,
        max: 400,
      },
      rss_mib: Percentiles {
        p50: 10,
        p90: 20,
        p99: 30,
        max: 40,
      },
      phase_p99_ms: PHASES.iter().map(|phase| (phase.to_string(), 5)).collect(),
      phase_wall_pct: PHASES
        .iter()
        .map(|phase| (phase.to_string(), 100.0 / 17.0))
        .collect(),
      tail: TailStats {
        top1pct_wall_share: 10.0,
        top5pct_wall_share: 27.0,
        over_30s: 5,
        over_60s: 1,
        over_120s: 0,
        over_180s: 0,
      },
      rss_buckets: RssBuckets {
        over_2gib: 3,
        over_3gib: 1,
        over_4gib: 0,
      },
      math: MathStats {
        formulae: 100,
        parse_invocations: 90,
        parse_count: 120,
        parses_per_formula: 1.33,
      },
      slow_tail_dominant: vec![("math_parse".to_string(), 22), ("digest".to_string(), 17)],
      fatal_profile: OutcomeWall {
        n: 1,
        median_ms: 3000,
        mean_ms: 13000,
        p99_ms: 98000,
      },
      no_problem_profile: OutcomeWall {
        n: 2,
        median_ms: 100,
        mean_ms: 150,
        p99_ms: 300,
      },
      slowest: Some(("1234.5678".to_string(), 400)),
      highest_rss: Some(("2345.6789".to_string(), 40)),
      total_formulae: 100,
      total_graphics_assets: 5,
      total_output_bytes: 999,
      generated_unix: 0,
    };
    let mut context = Context::new();
    context.insert(
      "global",
      &serde_json::json!({ "title": "t", "description": "d" }),
    );
    context.insert("summary", &summary);
    // Optional layout-only vars, inserted so the shared layout never trips on an undefined lookup.
    context.insert("is_admin", &false);
    context.insert("message", &"");
    context.insert("history", &false);

    let html = tera
      .render("telemetry-report", &context)
      .expect("the telemetry-report template renders with a representative summary");
    assert!(html.contains("Outcome mix"), "renders the outcome section");
    assert!(
      html.contains("Per-phase profile"),
      "renders the phase profile"
    );
    assert!(
      html.contains("/document/arxiv/tex_to_html/1234.5678"),
      "the slowest witness links to its per-article forensics"
    );
    assert!(
      html.contains("2345.6789"),
      "renders the highest-RSS witness"
    );
    assert!(
      html.contains("Where wall time goes"),
      "renders the phase budget"
    );
    assert!(
      html.contains("Wall by outcome"),
      "renders the outcome wall profiles"
    );
    assert!(
      html.contains("candidate parses/formula"),
      "renders the math over-parse multiplier"
    );
    assert!(
      html.contains("Slowest-50 dominated by"),
      "renders the slow-tail driver"
    );
    assert!(
      html.contains("alloc wall"),
      "renders the RSS pressure buckets"
    );
  }
}
