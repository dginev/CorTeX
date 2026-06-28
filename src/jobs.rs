// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Background jobs: one persisted row per long-running administrative operation, run on an
//! in-process thread with progress persisted to the database. The shared mechanism behind corpus
//! import/extend, service activation, runs, and dataset export. See `docs/archive/JOB_MODEL.md`.

use std::thread;

use chrono::NaiveDateTime;
use diesel::dsl::now;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

use crate::backend::DbPool;
use crate::config::config;
use crate::schema::jobs;

/// A persisted background job.
#[derive(Queryable, Identifiable, Clone, Debug)]
#[diesel(table_name = jobs)]
pub struct Job {
  /// Internal serial id.
  pub id: i64,
  /// External handle.
  pub uuid: Uuid,
  /// Operation kind (e.g. `corpus_import`).
  pub kind: String,
  /// `queued` | `running` | `succeeded` | `failed` | `interrupted`.
  pub status: String,
  /// Units of work completed.
  pub progress_current: i32,
  /// Total units of work, when known.
  pub progress_total: Option<i32>,
  /// Current step, or the error message on failure.
  pub message: String,
  /// Who started the job (Arm 9 identity).
  pub actor: String,
  /// Job inputs.
  pub params: Value,
  /// Terminal result payload.
  pub result: Option<Value>,
  /// When the job was created.
  pub created_at: NaiveDateTime,
  /// When the job was last updated.
  pub updated_at: NaiveDateTime,
}

#[derive(Insertable)]
#[diesel(table_name = jobs)]
struct NewJob {
  kind: String,
  actor: String,
  params: Value,
}

/// A handle passed to a job body; each call persists progress on the job row.
pub struct JobProgress {
  pool: DbPool,
  job_id: i64,
}
impl JobProgress {
  /// Records progress (`current`/`total` and a human-readable `message`) on the job row.
  pub fn step(&self, current: i32, total: Option<i32>, message: &str) {
    if let Ok(mut connection) = self.pool.get() {
      let _ = diesel::update(jobs::table.filter(jobs::id.eq(self.job_id)))
        .set((
          jobs::progress_current.eq(current),
          jobs::progress_total.eq(total),
          jobs::message.eq(message),
          jobs::updated_at.eq(now),
        ))
        .execute(&mut connection);
    }
  }
}

/// Spawns a background job: inserts a `queued` row, returns its uuid, and runs `body` on a thread.
/// The body reports progress via [`JobProgress`] and returns a terminal result or an error message.
pub fn spawn_job<F>(
  pool: DbPool,
  kind: &str,
  actor: &str,
  params: Value,
  body: F,
) -> Result<Uuid, String>
where
  F: FnOnce(&JobProgress) -> Result<Value, String> + Send + 'static,
{
  let mut connection = pool.get().map_err(|e| e.to_string())?;
  let (job_id, job_uuid): (i64, Uuid) = diesel::insert_into(jobs::table)
    .values(NewJob {
      kind: kind.to_string(),
      actor: actor.to_string(),
      params,
    })
    .returning((jobs::id, jobs::uuid))
    .get_result(&mut connection)
    .map_err(|e| e.to_string())?;
  drop(connection);

  // Structured operational journal for the whole job lifecycle (the jobs table is the durable
  // record; this is the leveled `tracing` stream an operator tails alongside the dispatcher, so
  // background activity — imports, reruns, reindex — is visible and correlatable in one place).
  // Captured (owned) for the worker thread, which can't borrow these `&str`s.
  let log_kind = kind.to_string();
  let log_actor = actor.to_string();
  tracing::info!(kind = %log_kind, actor = %log_actor, job = %job_uuid, "background job spawned");

  let worker_pool = pool.clone();
  thread::spawn(move || {
    let started = std::time::Instant::now();
    set_running(&worker_pool, job_id);
    let progress = JobProgress {
      pool: worker_pool.clone(),
      job_id,
    };
    // Catch a panicking body so a job is never stranded `running` forever (e.g. an importer panic,
    // or `from_address`/`connection_at` panicking when the DB is briefly unreachable). A panic
    // becomes a terminal `failed` with a message — the job reaches a real health state.
    let (status, message) =
      match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&progress))) {
        Ok(Ok(result)) => {
          finish(&worker_pool, job_id, "succeeded", "", Some(result));
          ("succeeded", String::new())
        },
        Ok(Err(message)) => {
          finish(&worker_pool, job_id, "failed", &message, None);
          ("failed", message)
        },
        Err(panic) => {
          let message = format!("job panicked: {}", panic_message(&*panic));
          finish(&worker_pool, job_id, "failed", &message, None);
          ("failed", message)
        },
      };
    let elapsed_ms = started.elapsed().as_millis();
    if status == "succeeded" {
      tracing::info!(kind = %log_kind, actor = %log_actor, job = %job_uuid, elapsed_ms, "background job succeeded");
    } else {
      tracing::warn!(kind = %log_kind, actor = %log_actor, job = %job_uuid, elapsed_ms, error = %message, "background job failed");
    }
  });
  Ok(job_uuid)
}

/// The job `kind` for a `report_summary` rollup refresh.
pub const REFRESH_REPORTS_KIND: &str = "refresh_reports";

/// Spawns a background job for the manual "Force refresh" action. The global `report_summary`
/// matview was retired in favour of a per-(corpus, service, severity) cache (`report_grain_cache`),
/// so this no longer does a multi-minute rebuild: it **invalidates** the whole cache (a cheap
/// `DELETE`, never a scan), and each report slice then repopulates lazily, per scope, on its next
/// view. Run **off** the request path so the caller returns immediately.
///
/// **Debounced:** one global invalidation suffices for every report page, so if a job is already
/// queued or running its uuid is returned instead of spawning another. Poll `GET /api/jobs/<uuid>`
/// for status/health.
pub fn spawn_report_refresh(pool: DbPool, actor: &str) -> Result<Uuid, String> {
  // Debounce against an already-active refresh before inserting a new job row.
  {
    let mut connection = pool.get().map_err(|e| e.to_string())?;
    if let Some(existing) = list_recent(&mut connection, true, 200)
      .into_iter()
      .find(|job| job.kind == REFRESH_REPORTS_KIND)
    {
      return Ok(existing.uuid);
    }
  }
  spawn_job(pool, REFRESH_REPORTS_KIND, actor, Value::Null, |progress| {
    let mut connection = progress.pool.get().map_err(|e| e.to_string())?;
    crate::backend::invalidate_all(&mut connection).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "status": "cache-invalidated" }))
  })
}

/// The job `kind` for a single `(corpus, service, severity)` report-cache populate.
pub const POPULATE_REPORT_KIND: &str = "populate_report";

/// Whether a populate job for exactly this `(corpus, service, severity)` slice is already queued or
/// running — the request path's pre-check, so a burst of viewers of a still-computing report
/// doesn't each re-attempt the (bounded) inline aggregation while the background job is already on
/// it.
pub fn report_populate_active(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
) -> bool {
  list_recent(connection, true, 200)
    .into_iter()
    .any(|job| populate_job_matches(&job, corpus_id, service_id, severity))
}

fn populate_job_matches(job: &Job, corpus_id: i32, service_id: i32, severity: &str) -> bool {
  job.kind == POPULATE_REPORT_KIND
    && job.params.get("corpus_id").and_then(Value::as_i64) == Some(corpus_id as i64)
    && job.params.get("service_id").and_then(Value::as_i64) == Some(service_id as i64)
    && job.params.get("severity").and_then(Value::as_str) == Some(severity)
}

/// Spawns a background job that (re)computes one `(corpus, service, severity)` report slice
/// (`rollup::populate_scope`) **off** the request path — the heavy aggregation behind a cold report
/// view (minutes for the full-arXiv `info` slice) runs here instead of pinning a frontend
/// connection and blocking the viewer, who is shown a "report computing" page that refreshes when
/// the slice is ready. **Debounced** per scope: an already-active populate for the same slice is
/// reused (its uuid returned) rather than spawning a duplicate. Poll `GET /api/jobs/<uuid>` for
/// status/health.
pub fn spawn_report_populate(
  pool: DbPool,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
  actor: &str,
) -> Result<Uuid, String> {
  {
    let mut connection = pool.get().map_err(|e| e.to_string())?;
    if let Some(existing) = list_recent(&mut connection, true, 200)
      .into_iter()
      .find(|job| populate_job_matches(job, corpus_id, service_id, severity))
    {
      return Ok(existing.uuid);
    }
  }
  let severity = severity.to_string();
  let params =
    serde_json::json!({ "corpus_id": corpus_id, "service_id": service_id, "severity": severity });
  spawn_job(pool, POPULATE_REPORT_KIND, actor, params, move |progress| {
    let mut connection = progress.pool.get().map_err(|e| e.to_string())?;
    progress.step(0, Some(1), &format!("aggregating the {severity} report"));
    crate::backend::populate_scope(&mut connection, corpus_id, service_id, &severity)
      .map_err(|e| e.to_string())?;
    progress.step(1, Some(1), "report ready");
    Ok(serde_json::json!({ "populated": severity }))
  })
}

/// The job `kind` for an online index rebuild.
pub const REINDEX_KIND: &str = "reindex";

/// The high-churn / append-heavy tables that benefit from periodic maintenance — index rebuilds
/// (their indexes bloat over time) and planner-statistics refreshes (bulk imports/reruns shift
/// their row distributions). Mirrors the autovacuum-tuned set + `docs/DB_TUNING.md`. Shared by
/// [`spawn_reindex`] and [`spawn_analyze`].
const MAINTENANCE_TABLES: [&str; 7] = [
  "tasks",
  "log_infos",
  "log_warnings",
  "log_errors",
  "log_fatals",
  "log_invalids",
  "historical_tasks",
];

/// Spawns a background job that rebuilds the high-churn tables' indexes **online** with
/// `REINDEX (CONCURRENTLY)` — no exclusive lock, so reads/writes continue (DB ongoing-maintenance;
/// `docs/DB_TUNING.md`). Runs **off** the request path (rebuilds are minutes-to-hours at scale) and
/// reports per-table progress. **Debounced:** a reindex already queued/running is reused.
///
/// `REINDEX ... CONCURRENTLY` forbids running inside a transaction — the job body uses a fresh
/// pooled connection in autocommit, so this holds.
pub fn spawn_reindex(pool: DbPool, actor: &str) -> Result<Uuid, String> {
  {
    let mut connection = pool.get().map_err(|e| e.to_string())?;
    if let Some(existing) = list_recent(&mut connection, true, 200)
      .into_iter()
      .find(|job| job.kind == REINDEX_KIND)
    {
      return Ok(existing.uuid);
    }
  }
  spawn_job(
    pool,
    REINDEX_KIND,
    actor,
    serde_json::json!({ "tables": MAINTENANCE_TABLES }),
    |progress| {
      let mut connection = progress.pool.get().map_err(|e| e.to_string())?;
      let total = MAINTENANCE_TABLES.len() as i32;
      for (index, table) in MAINTENANCE_TABLES.iter().enumerate() {
        progress.step(index as i32, Some(total), &format!("reindexing {table}"));
        // `table` is a fixed identifier (not user input), so the interpolation is injection-safe.
        diesel::sql_query(format!("REINDEX (CONCURRENTLY) TABLE {table}"))
          .execute(&mut connection)
          .map_err(|e| format!("reindex {table} failed: {e}"))?;
      }
      progress.step(total, Some(total), "reindex complete");
      Ok(serde_json::json!({ "reindexed": MAINTENANCE_TABLES }))
    },
  )
}

/// The job `kind` for a planner-statistics refresh.
pub const ANALYZE_KIND: &str = "analyze";

/// Spawns a background job that refreshes the query planner's statistics with `ANALYZE` over the
/// high-churn tables. After a bulk import or a large rerun churns `tasks.status`, stale statistics
/// can make the planner mis-estimate and skip the right index (e.g. the TODO leasing index,
/// `todo_index`), so an operator can refresh them on demand instead of waiting for autovacuum's
/// next pass (DB ongoing-maintenance; `docs/DB_TUNING.md`). `ANALYZE` is online (a brief lock per
/// table, sampling only) and runs **off** the request path, reporting per-table progress.
/// **Debounced:** an analyze already queued/running is reused.
pub fn spawn_analyze(pool: DbPool, actor: &str) -> Result<Uuid, String> {
  {
    let mut connection = pool.get().map_err(|e| e.to_string())?;
    if let Some(existing) = list_recent(&mut connection, true, 200)
      .into_iter()
      .find(|job| job.kind == ANALYZE_KIND)
    {
      return Ok(existing.uuid);
    }
  }
  spawn_job(
    pool,
    ANALYZE_KIND,
    actor,
    serde_json::json!({ "tables": MAINTENANCE_TABLES }),
    |progress| {
      let mut connection = progress.pool.get().map_err(|e| e.to_string())?;
      let total = MAINTENANCE_TABLES.len() as i32;
      for (index, table) in MAINTENANCE_TABLES.iter().enumerate() {
        progress.step(index as i32, Some(total), &format!("analyzing {table}"));
        // `table` is a fixed identifier (not user input), so the interpolation is injection-safe.
        diesel::sql_query(format!("ANALYZE {table}"))
          .execute(&mut connection)
          .map_err(|e| format!("analyze {table} failed: {e}"))?;
      }
      progress.step(total, Some(total), "analyze complete");
      Ok(serde_json::json!({ "analyzed": MAINTENANCE_TABLES }))
    },
  )
}

/// Best-effort extraction of a human-readable message from a caught panic payload.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
  if let Some(s) = panic.downcast_ref::<&str>() {
    (*s).to_string()
  } else if let Some(s) = panic.downcast_ref::<String>() {
    s.clone()
  } else {
    "unknown panic".to_string()
  }
}

fn set_running(pool: &DbPool, job_id: i64) {
  if let Ok(mut connection) = pool.get() {
    let _ = diesel::update(jobs::table.filter(jobs::id.eq(job_id)))
      .set((jobs::status.eq("running"), jobs::updated_at.eq(now)))
      .execute(&mut connection);
  }
}

fn finish(pool: &DbPool, job_id: i64, status: &str, message: &str, result: Option<Value>) {
  if let Ok(mut connection) = pool.get() {
    let _ = diesel::update(jobs::table.filter(jobs::id.eq(job_id)))
      .set((
        jobs::status.eq(status),
        jobs::message.eq(message),
        jobs::result.eq(result),
        jobs::updated_at.eq(now),
      ))
      .execute(&mut connection);
  }
}

/// The database's current wall clock as a tz-naive timestamp **in the session timezone** — i.e. the
/// same clock `created_at`/`updated_at` are written in (Postgres `now()` stored into a `timestamp`
/// column). Differencing a job's `updated_at` against this is therefore skew-free, unlike comparing
/// against the *app process's* `chrono::Utc::now()` (which differs by the session offset when the
/// DB is not on UTC). Used to compute a running job's heartbeat age (W-4 stall observability).
/// Best-effort: returns `None` if the probe fails (a broken connection), so callers degrade to "no
/// age" rather than reporting a bogus one.
pub fn db_now(connection: &mut PgConnection) -> Option<NaiveDateTime> {
  use diesel::dsl::sql;
  use diesel::sql_types::Timestamp;
  diesel::select(sql::<Timestamp>("LOCALTIMESTAMP"))
    .get_result(connection)
    .ok()
}

/// Finds a job by its external uuid handle.
pub fn find_job(connection: &mut PgConnection, job_uuid: Uuid) -> Option<Job> {
  jobs::table
    .filter(jobs::uuid.eq(job_uuid))
    .first(connection)
    .optional()
    .ok()
    .flatten()
}

/// Marks any non-terminal job whose progress heartbeat has been silent for longer than
/// `config().jobs.stale_timeout_seconds` as `interrupted` — the **runtime** complement to
/// [`interrupt_orphans`] (which only runs at startup). It closes the W-4 zombie: a job whose body
/// *hangs* while a long-lived frontend keeps running would otherwise sit `running` forever, leaking
/// a thread and lying to every pending-check + the report-refresh debounce. A job that keeps
/// `step`-ing stays live (fresh `updated_at`); only a silent one is reaped. **Self-correcting:** if
/// a merely-slow job is reaped and later finishes, its `finish()` overwrites the status, so a
/// generous timeout costs at most a transient `interrupted` display. Skew-free (differences against
/// the DB clock, like [`db_now`]). Returns the count reaped; best-effort.
///
/// **Caveat — Rust cannot force-kill a thread:** reaping marks the DB row terminal (so accounting,
/// pending-checks, and the refresh debounce are correct) but the hung OS thread itself runs until
/// its body unblocks. The leak is bounded — its pooled connection is returned between `step`s and
/// the r2d2 pool caps total connections — but the thread/stack is only reclaimed when the body
/// returns; truly aborting a hung *blocking* job would need subprocess isolation (a SIGKILL-able
/// child), a deliberate architecture trade. See the W-4 ledger entry.
pub fn reap_stale(connection: &mut PgConnection) -> usize {
  let timeout_secs = config().jobs.stale_timeout_seconds;
  let Some(clock) = db_now(connection) else {
    return 0; // DB clock unreadable → skip rather than reap against a bogus app clock
  };
  let cutoff = clock - chrono::Duration::seconds(timeout_secs);
  diesel::update(
    jobs::table
      .filter(jobs::status.eq("queued").or(jobs::status.eq("running")))
      .filter(jobs::updated_at.lt(cutoff)),
  )
  .set((
    jobs::status.eq("interrupted"),
    jobs::message.eq(format!(
      "no progress heartbeat for over {} minutes; presumed stalled (W-4)",
      timeout_secs / 60
    )),
  ))
  .execute(connection)
  .unwrap_or(0)
}

/// Counts jobs in terminal `status` created within the last `hours` — a current-state observability
/// signal for "are jobs failing / stalling lately?". A **rolling window** so the gauge
/// auto-resolves (unlike an ever-growing total-failures counter, which would alert forever after
/// one failure). Skew-free (windowed against the DB clock). Best-effort: `0` on error.
pub fn count_recent_with_status(connection: &mut PgConnection, status: &str, hours: i64) -> usize {
  let Some(clock) = db_now(connection) else {
    return 0;
  };
  let cutoff = clock - chrono::Duration::hours(hours);
  jobs::table
    .filter(jobs::status.eq(status))
    .filter(jobs::created_at.gt(cutoff))
    .count()
    .get_result::<i64>(connection)
    .map(|count| count as usize)
    .unwrap_or(0)
}

/// Lists recent jobs, most-recent-first, capped at `limit`. With `active_only`, returns just the
/// **pending** (non-terminal: `queued`/`running`) jobs — the fleet-wide observability check for any
/// background-task capability. Best-effort: an error yields an empty list rather than propagating.
pub fn list_recent(connection: &mut PgConnection, active_only: bool, limit: i64) -> Vec<Job> {
  // Freshen first: reap heartbeat-dead jobs so neither this listing, nor the active/pending check,
  // nor the report-refresh debounce ever counts a hung zombie as live (W-4).
  reap_stale(connection);
  let mut query = jobs::table
    .order(jobs::created_at.desc())
    .limit(limit)
    .into_boxed();
  if active_only {
    query = query.filter(jobs::status.eq("queued").or(jobs::status.eq("running")));
  }
  query.load(connection).unwrap_or_default()
}

/// Marks any non-terminal jobs as `interrupted`; call once on frontend startup so jobs that died
/// with a previous process are not left looking live.
pub fn interrupt_orphans(connection: &mut PgConnection) -> usize {
  diesel::update(jobs::table.filter(jobs::status.eq("queued").or(jobs::status.eq("running"))))
    .set((
      jobs::status.eq("interrupted"),
      jobs::message.eq("interrupted by a restart"),
    ))
    .execute(connection)
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn job(kind: &str, params: Value) -> Job {
    let t = chrono::NaiveDate::from_ymd_opt(2026, 6, 28)
      .unwrap()
      .and_hms_opt(0, 0, 0)
      .unwrap();
    Job {
      id: 1,
      uuid: Uuid::nil(),
      kind: kind.to_string(),
      status: "running".to_string(),
      progress_current: 0,
      progress_total: None,
      message: String::new(),
      actor: "test".to_string(),
      params,
      result: None,
      created_at: t,
      updated_at: t,
    }
  }

  // The populate-job debounce keys on the (corpus_id, service_id, severity) it stored in `params` —
  // and the ids are JSON numbers (i64) compared against the i32 args, the easy-to-break part.
  #[test]
  fn populate_job_matches_only_its_own_scope() {
    let p = serde_json::json!({ "corpus_id": 7, "service_id": 3, "severity": "info" });
    let j = job(POPULATE_REPORT_KIND, p);
    assert!(populate_job_matches(&j, 7, 3, "info"));
    assert!(!populate_job_matches(&j, 7, 3, "warning")); // other severity of the same scope
    assert!(!populate_job_matches(&j, 8, 3, "info")); // other corpus
    assert!(!populate_job_matches(&j, 7, 4, "info")); // other service
    // A different kind with identical params must never be mistaken for a populate job.
    let other = job(
      REFRESH_REPORTS_KIND,
      serde_json::json!({ "corpus_id": 7, "service_id": 3, "severity": "info" }),
    );
    assert!(!populate_job_matches(&other, 7, 3, "info"));
  }
}
