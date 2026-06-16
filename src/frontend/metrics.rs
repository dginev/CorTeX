// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Prometheus **`/metrics`** — operational gauges for scraping (Arm 8 observability).
//! **Token-gated** via the [`Actor`] guard, so it is not public; Prometheus scrapes it with
//! `?token=<token>` (the guard also accepts the `X-Cortex-Token` header). Deliberately limited to
//! **current-state gauges** read on each scrape — connection-pool saturation, background-job
//! backlog, active admin sessions, registered corpora/services, the dispatcher worker fleet's
//! size + in-flight backlog, and the **pending-conversion backlog** (`cortex_tasks_todo`, the one
//! full-table count — bounded ~tens-to-hundreds of ms even at arXiv scale).
//!
//! It does **not** instrument the hot paths (no dispatcher changes) and does **not** run the
//! `/healthz` ZMQ/filesystem probes (those are slow and that endpoint's job). Real-time
//! **counters** (request rates, per-event tallies via the `metrics` crate) need hot-path
//! instrumentation and are a follow-on — this gives the operationally-critical saturation/backlog
//! signals today, cheaply.

use diesel::prelude::*;
use rocket::http::ContentType;
use rocket::{Route, State};

use crate::backend::DbPool;
use crate::frontend::actor::Actor;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, Service, Session, WorkerMetadata};
use crate::schema::tasks;

/// Appends one Prometheus gauge (HELP + TYPE + value lines) to `out`.
fn gauge(out: &mut String, name: &str, help: &str, value: impl std::fmt::Display) {
  out.push_str(&format!(
    "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
  ));
}

/// `GET /metrics` — Prometheus exposition of current-state gauges. **Token-gated** (the [`Actor`]
/// guard; scrape with `?token=`). Pool gauges are always emitted (in-memory); DB-derived gauges are
/// best-effort — on a pool/db hiccup they are omitted (and `cortex_db_reachable` is `0`) rather
/// than reporting a wrong value.
#[get("/metrics")]
pub fn metrics(_caller: Actor, pool: &State<DbPool>) -> (ContentType, String) {
  let mut out = String::new();

  out.push_str("# HELP cortex_build_info CorTeX build information.\n");
  out.push_str("# TYPE cortex_build_info gauge\n");
  out.push_str(&format!(
    "cortex_build_info{{version=\"{}\"}} 1\n",
    env!("CARGO_PKG_VERSION")
  ));

  // Connection pool — in-memory, always available even if the database is unreachable.
  let state = pool.state();
  gauge(
    &mut out,
    "cortex_pool_max",
    "Maximum size of the frontend connection pool.",
    pool.max_size(),
  );
  gauge(
    &mut out,
    "cortex_pool_connections",
    "Connections currently established (idle + in use).",
    state.connections,
  );
  gauge(
    &mut out,
    "cortex_pool_idle",
    "Idle, immediately-available pooled connections.",
    state.idle_connections,
  );
  gauge(
    &mut out,
    "cortex_pool_in_use",
    "Pooled connections currently checked out (saturation signal).",
    state.connections.saturating_sub(state.idle_connections),
  );

  // DB-derived gauges: best-effort over one checkout. db_reachable doubles as the "trust the gauges
  // below" flag.
  match pool.get() {
    Ok(mut connection) => {
      gauge(
        &mut out,
        "cortex_db_reachable",
        "1 if the database is reachable, else 0.",
        1,
      );
      gauge(
        &mut out,
        "cortex_corpora_total",
        "Registered corpora.",
        Corpus::all(&mut connection).map_or(0, |corpora| corpora.len()),
      );
      gauge(
        &mut out,
        "cortex_services_total",
        "Registered services.",
        Service::all(&mut connection).map_or(0, |services| services.len()),
      );
      gauge(
        &mut out,
        "cortex_jobs_active",
        "Background jobs currently queued or running.",
        crate::jobs::list_recent(&mut connection, true, 1000).len(),
      );
      gauge(
        &mut out,
        "cortex_jobs_failed_recent",
        "Background jobs that ended in `failed` within the last 24h (rolling window).",
        crate::jobs::count_recent_with_status(&mut connection, "failed", 24),
      );
      gauge(
        &mut out,
        "cortex_jobs_interrupted_recent",
        "Background jobs `interrupted` within the last 24h — stale-reaped (W-4) or restart orphans \
         (rolling window).",
        crate::jobs::count_recent_with_status(&mut connection, "interrupted", 24),
      );
      gauge(
        &mut out,
        "cortex_sessions_active",
        "Active (unexpired) admin sessions.",
        Session::active(&mut connection).map_or(0, |sessions| sessions.len()),
      );
      if let Ok((workers, in_flight)) = WorkerMetadata::fleet_summary(&mut connection) {
        gauge(
          &mut out,
          "cortex_workers_total",
          "Worker rows registered with the dispatcher (per name+service).",
          workers,
        );
        gauge(
          &mut out,
          "cortex_workers_in_flight_total",
          "Dispatched-but-not-yet-returned tasks summed across the fleet (backlog signal).",
          in_flight,
        );
      }
      // Pending-conversion backlog: TODO tasks not yet dispatched to any worker — the headline "is
      // the fleet keeping up?" signal (`workers_in_flight` above is the *leased* backlog; this is
      // the *unleased* one, otherwise invisible to a scrape). One count over `tasks` — the
      // costliest gauge here (~tens-to-hundreds of ms at arXiv scale), but operationally
      // critical; degrades to 0 on a query error, like its siblings.
      gauge(
        &mut out,
        "cortex_tasks_todo",
        "Tasks awaiting conversion (status TODO, not yet dispatched) — the pending-work backlog.",
        tasks::table
          .filter(tasks::status.eq(TaskStatus::TODO.raw()))
          .count()
          .get_result::<i64>(&mut connection)
          .unwrap_or(0),
      );
    },
    Err(_) => gauge(
      &mut out,
      "cortex_db_reachable",
      "1 if the database is reachable, else 0.",
      0,
    ),
  }

  (ContentType::Plain, out)
}

/// The `/metrics` route.
pub fn routes() -> Vec<Route> { routes![metrics] }
