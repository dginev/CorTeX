// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Background jobs: one persisted row per long-running administrative operation, run on an
//! in-process thread with progress persisted to the database. The shared mechanism behind corpus
//! import/extend, service activation, runs, and dataset export. See `docs/JOB_MODEL.md`.

use std::thread;

use chrono::NaiveDateTime;
use diesel::dsl::now;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use serde_json::Value;
use uuid::Uuid;

use crate::backend::DbPool;
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

  let worker_pool = pool.clone();
  thread::spawn(move || {
    set_running(&worker_pool, job_id);
    let progress = JobProgress {
      pool: worker_pool.clone(),
      job_id,
    };
    // Catch a panicking body so a job is never stranded `running` forever (e.g. an importer panic,
    // or `from_address`/`connection_at` panicking when the DB is briefly unreachable). A panic
    // becomes a terminal `failed` with a message — the job reaches a real health state.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&progress))) {
      Ok(Ok(result)) => finish(&worker_pool, job_id, "succeeded", "", Some(result)),
      Ok(Err(message)) => finish(&worker_pool, job_id, "failed", &message, None),
      Err(panic) => finish(
        &worker_pool,
        job_id,
        "failed",
        &format!("job panicked: {}", panic_message(&*panic)),
        None,
      ),
    }
  });
  Ok(job_uuid)
}

/// The job `kind` for a `report_summary` rollup refresh.
pub const REFRESH_REPORTS_KIND: &str = "refresh_reports";

/// Spawns a background job that rebuilds the `report_summary` rollup — a multi-minute
/// `REFRESH ... CONCURRENTLY` at production scale (see `docs/REPORT_FRESHNESS.md`) — **off** the
/// request path, so the caller (a force-refresh endpoint, or the rerun path) returns immediately.
///
/// **Debounced:** a single global refresh already updates the data behind *every* report page, so
/// if one is already queued or running its uuid is returned instead of spawning another (concurrent
/// rebuilds would only serialize on the matview lock anyway). Poll `GET /api/jobs/<uuid>` for
/// status/health.
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
    crate::backend::refresh_report_summary(&mut connection).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "status": "refreshed" }))
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

/// Lists recent jobs, most-recent-first, capped at `limit`. With `active_only`, returns just the
/// **pending** (non-terminal: `queued`/`running`) jobs — the fleet-wide observability check for any
/// background-task capability. Best-effort: an error yields an empty list rather than propagating.
pub fn list_recent(connection: &mut PgConnection, active_only: bool, limit: i64) -> Vec<Job> {
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
