#![allow(clippy::extra_unused_lifetimes)]
use std::collections::HashMap;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, SystemTime};

use diesel::result::Error;
use diesel::*;

use serde::Serialize;

use crate::backend::DbPool;
use crate::models::Service;
use crate::schema::worker_metadata;

#[derive(Insertable, Debug)]
#[diesel(table_name = worker_metadata)]
/// Metadata collection for workers, updated by the dispatcher upon zmq transactions
pub struct NewWorkerMetadata {
  /// associated service for this worker metadata set
  pub service_id: i32,
  /// time of last ventilator dispatch to the service
  pub last_dispatched_task_id: i64,
  /// time of last sink job received from the service
  pub last_returned_task_id: Option<i64>,
  /// dispatch totals
  pub total_dispatched: i32,
  /// return totals
  pub total_returned: i32,
  /// first registered ventilator request for this worker, coincides with insertion in DB
  pub first_seen: SystemTime,
  /// first time seen in the current dispatcher session
  pub session_seen: Option<SystemTime>,
  /// time of last dispatched task
  pub time_last_dispatch: SystemTime,
  /// time of last returned job result
  pub time_last_return: Option<SystemTime>,
  /// identity of this worker, usually hostname:pid
  pub name: String,
}

#[derive(Identifiable, Queryable, Associations, Clone, Debug, Serialize)]
#[diesel(table_name = worker_metadata)]
#[diesel(belongs_to(Service, foreign_key = service_id))]
/// Metadata collection for workers, updated by the dispatcher upon zmq transactions
pub struct WorkerMetadata {
  /// task primary key, auto-incremented by postgresql
  pub id: i32,
  /// associated service for this worker metadata set
  pub service_id: i32,
  /// time of last ventilator dispatch to the service
  pub last_dispatched_task_id: i64,
  /// time of last sink job received from the service
  pub last_returned_task_id: Option<i64>,
  /// dispatch totals
  pub total_dispatched: i32,
  /// return totals
  pub total_returned: i32,
  /// first registered ventilator request for this worker, coincides with insertion in DB
  pub first_seen: SystemTime,
  /// first time seen in the current dispatcher session
  pub session_seen: Option<SystemTime>,
  /// time of last dispatched task
  pub time_last_dispatch: SystemTime,
  /// time of last returned job result
  pub time_last_return: Option<SystemTime>,
  /// identity of this worker, usually hostname:pid
  pub name: String,
}

impl From<WorkerMetadata> for HashMap<String, String> {
  fn from(worker: WorkerMetadata) -> HashMap<String, String> {
    let mut wh = HashMap::new();
    wh.insert("id".to_string(), worker.id.to_string());
    wh.insert("service_id".to_string(), worker.service_id.to_string());
    wh.insert(
      "last_dispatched_task_id".to_string(),
      worker.last_dispatched_task_id.to_string(),
    );
    wh.insert(
      "last_returned_task_id".to_string(),
      match worker.last_returned_task_id {
        Some(id) => id.to_string(),
        _ => String::new(),
      },
    );
    wh.insert(
      "total_dispatched".to_string(),
      worker.total_dispatched.to_string(),
    );
    wh.insert(
      "total_returned".to_string(),
      worker.total_returned.to_string(),
    );
    // Per-worker outstanding = dispatched − returned: tasks this worker took but hasn't returned a
    // result for. The actionable per-worker signal the fleet summary deliberately omits as an
    // aggregate (KNOWN_ISSUES P-3) — a stale row with a large outstanding is a worker that died or
    // stopped returning results. `saturating_sub` guards the impossible returned>dispatched case.
    wh.insert(
      "outstanding".to_string(),
      worker
        .total_dispatched
        .saturating_sub(worker.total_returned)
        .to_string(),
    );

    // Absolute UTC timestamps (RFC 3339); the UI localizes them to the viewer's zone with its code
    // (public/js/localtime.js), and the fresh/stale row coloring still conveys liveness at a
    // glance.
    wh.insert("first_seen".to_string(), iso_utc_system(worker.first_seen));
    wh.insert(
      "session_seen".to_string(),
      worker.session_seen.map(iso_utc_system).unwrap_or_default(),
    );
    wh.insert(
      "time_last_dispatch".to_string(),
      iso_utc_system(worker.time_last_dispatch),
    );
    wh.insert(
      "time_last_return".to_string(),
      worker
        .time_last_return
        .map(iso_utc_system)
        .unwrap_or_default(),
    );
    wh.insert(
      "fresh".to_string(),
      if worker_is_fresh(&worker) {
        "fresh"
      } else {
        "stale"
      }
      .to_string(),
    );
    wh.insert("name".to_string(), worker.name);
    wh
  }
}

/// Formats a `SystemTime` as an RFC 3339 UTC timestamp (seconds precision) — the zone-unambiguous
/// form the UI localizes to the viewer's zone (public/js/localtime.js); empty timestamps render as
/// an empty cell.
pub fn iso_utc_system(then: SystemTime) -> String {
  chrono::DateTime::<chrono::Utc>::from(then).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Whether the worker has dispatched-to or returned-a-result within the last minute — the
/// at-a-glance liveness flag driving the row coloring. Skew-safe: a future timestamp (clock skew
/// across the fleet's hosts, or a DB clock running ahead) counts as fresh rather than
/// `.unwrap()`-panicking on the `/workers/<service>` request path (no-panic-on-request-path
/// mandate; cf. KNOWN_ISSUES F-4).
fn worker_is_fresh(worker: &WorkerMetadata) -> bool { worker.is_fresh() }

impl WorkerMetadata {
  /// Whether this worker dispatched-to or returned-a-result within the last minute — its
  /// at-a-glance liveness (the threshold that drives the fleet-screen row coloring and the
  /// fleet-health summary). Skew-safe: a future timestamp (clock skew across the fleet's hosts, or
  /// a DB clock running ahead) counts as fresh rather than `.unwrap()`-panicking (KNOWN_ISSUES
  /// F-4).
  pub fn is_fresh(&self) -> bool {
    let last_active = match self.time_last_return {
      Some(returned) => returned.max(self.time_last_dispatch),
      None => self.time_last_dispatch,
    };
    SystemTime::now()
      .duration_since(last_active)
      .map(|elapsed| elapsed.as_secs() < 60)
      .unwrap_or(true)
  }

  /// Load worker metadata record by identity and service id
  pub fn find_by_name(
    identity: &str,
    sid: i32,
    connection: &mut PgConnection,
  ) -> Result<WorkerMetadata, Error> {
    use crate::schema::worker_metadata::{name, service_id};
    worker_metadata::table
      .filter(name.eq(identity))
      .filter(service_id.eq(sid))
      .get_result(connection)
  }

  /// The **currently-active** fleet summary (for the dashboard, `/metrics`, and `cortex status`):
  /// the number of workers that dispatched or returned a task within
  /// [`ACTIVE_WORKER_WINDOW_SECS`], and the tasks in-flight (`dispatched − returned`) summed
  /// across **those** workers. Filtering by recent activity is what makes this a truthful "what's
  /// happening now" signal: without it, an idle deployment whose `worker_metadata` rows are
  /// months stale reports a large phantom fleet and a meaningless cumulative-lifetime in-flight
  /// gap (the KNOWN_ISSUES **P-3** confusion). With no dispatcher running this correctly returns
  /// `(0, 0)`. One aggregate query over the small `worker_metadata` table — cheap per scrape.
  pub fn fleet_summary(connection: &mut PgConnection) -> Result<(i64, i64), Error> {
    use crate::schema::worker_metadata::dsl;
    let active_since = SystemTime::now()
      .checked_sub(Duration::from_secs(ACTIVE_WORKER_WINDOW_SECS))
      .unwrap_or(SystemTime::UNIX_EPOCH);
    let (count, in_flight): (i64, Option<i64>) = worker_metadata::table
      // Active = dispatched or returned a task within the window. `time_last_dispatch` is NOT NULL;
      // a NULL `time_last_return` simply doesn't satisfy that arm (a never-returned worker still
      // counts if it dispatched recently).
      .filter(
        dsl::time_last_dispatch
          .ge(active_since)
          .or(dsl::time_last_return.ge(active_since)),
      )
      .select((
        diesel::dsl::count_star(),
        diesel::dsl::sum(dsl::total_dispatched - dsl::total_returned),
      ))
      .first(connection)?;
    Ok((count, in_flight.unwrap_or(0)))
  }

  /// The most-recently-active workers, newest `time_last_dispatch` first — the admin live-activity
  /// feed's "what the fleet is doing now" list. A cheap ordered read over the small
  /// `worker_metadata` table (read-only; the dispatcher is never in the loop).
  pub fn recent(connection: &mut PgConnection, limit: i64) -> Result<Vec<WorkerMetadata>, Error> {
    use crate::schema::worker_metadata::dsl;
    worker_metadata::table
      .order(dsl::time_last_dispatch.desc())
      .limit(limit)
      .load(connection)
  }
}

/// How recently a worker must have dispatched or returned a task to count as **active** in
/// [`WorkerMetadata::fleet_summary`]. Workers process tasks frequently, so ten minutes captures the
/// actively-converting fleet (even mid-conversion on a slow `latexml` document) while excluding
/// rows left stale by a finished — or never-started — run. So a deployment with no dispatcher
/// reports an empty fleet rather than a phantom one (P-3).
const ACTIVE_WORKER_WINDOW_SECS: u64 = 600;

/// Capacity of the worker-metadata event queue. A few seconds of headroom at the deployment's
/// ~100–200 events/s; if the writer falls this far behind (e.g. the DB is stalled), further events
/// are dropped rather than growing memory or blocking dispatch — observability metadata is
/// best-effort under overload (cf. backpressure, KNOWN_ISSUES D-6).
const METADATA_QUEUE_BOUND: usize = 8192;

/// A worker-metadata event enqueued by the ventilator (dispatch) or sink (return) for the single
/// background writer to apply as an upsert.
enum WorkerEvent {
  /// A task was dispatched to `name` for `service_id` (carrying the dispatched task id).
  Dispatched {
    /// Worker identity.
    name: String,
    /// Service the worker is registered against.
    service_id: i32,
    /// The dispatched task id.
    task_id: i64,
  },
  /// A result was returned by `name` for `service_id` (carrying the returned task id).
  Received {
    /// Worker identity.
    name: String,
    /// Service the worker is registered against.
    service_id: i32,
    /// The returned task id.
    task_id: i64,
  },
}

/// A cloneable, non-blocking handle the ventilator and sink use to enqueue worker-metadata events.
/// Sends never block the dispatch hot loop: if the background writer is saturated the event is
/// dropped (see [`METADATA_QUEUE_BOUND`]).
#[derive(Clone)]
pub struct WorkerMetadataSender {
  tx: SyncSender<WorkerEvent>,
}
impl WorkerMetadataSender {
  /// Enqueue a dispatch event (non-blocking, best-effort).
  pub fn dispatched(&self, name: String, service_id: i32, task_id: i64) {
    let _ = self.tx.try_send(WorkerEvent::Dispatched {
      name,
      service_id,
      task_id,
    });
  }
  /// Enqueue a return event (non-blocking, best-effort).
  pub fn received(&self, name: String, service_id: i32, task_id: i64) {
    let _ = self.tx.try_send(WorkerEvent::Received {
      name,
      service_id,
      task_id,
    });
  }
}

/// Spawns the single background worker-metadata writer and returns a cloneable
/// [`WorkerMetadataSender`]. The writer drains events and applies the race-free upserts on pooled
/// connections; it exits cleanly once every sender has dropped. This bounds metadata bookkeeping to
/// **one** thread regardless of dispatch rate — replacing the unbounded thread-per-event spawn
/// (KNOWN_ISSUES D-1) — while keeping the DB work off the dispatch hot path.
pub fn start_metadata_writer(pool: DbPool) -> WorkerMetadataSender {
  let (tx, rx) = sync_channel::<WorkerEvent>(METADATA_QUEUE_BOUND);
  let _ = thread::spawn(move || {
    for event in rx {
      // Pooled checkout (~11µs) vs a fresh PgConnection (~4.5ms, the Arm 14 spike).
      let mut pooled = match pool.get() {
        Ok(connection) => connection,
        Err(_) => continue,
      };
      let now = SystemTime::now();
      let result = match event {
        WorkerEvent::Dispatched {
          name,
          service_id,
          task_id,
        } => upsert_dispatched(&mut pooled, &name, service_id, task_id, now),
        WorkerEvent::Received {
          name,
          service_id,
          task_id,
        } => upsert_received(&mut pooled, &name, service_id, task_id, now),
      };
      if let Err(error) = result {
        eprintln!("-- worker metadata writer: upsert failed: {error:?}");
      }
    }
  });
  WorkerMetadataSender { tx }
}

/// Inserts — or, on `(name, service_id)` conflict, increments — the dispatch tallies for a worker.
/// `session_seen`/`first_seen` are preserved on conflict (the row already carries them).
fn upsert_dispatched(
  connection: &mut PgConnection,
  name: &str,
  service_id: i32,
  last_dispatched_task_id: i64,
  now: SystemTime,
) -> QueryResult<usize> {
  insert_into(worker_metadata::table)
    .values(&NewWorkerMetadata {
      name: name.to_string(),
      service_id,
      last_dispatched_task_id,
      last_returned_task_id: None,
      total_dispatched: 1,
      total_returned: 0,
      first_seen: now,
      session_seen: Some(now),
      time_last_dispatch: now,
      time_last_return: None,
    })
    .on_conflict((worker_metadata::name, worker_metadata::service_id))
    .do_update()
    .set((
      worker_metadata::last_dispatched_task_id.eq(last_dispatched_task_id),
      worker_metadata::total_dispatched.eq(worker_metadata::total_dispatched + 1),
      worker_metadata::time_last_dispatch.eq(now),
    ))
    .execute(connection)
}

/// Inserts — or, on conflict, increments — the return tallies for a worker. The insert branch
/// covers the out-of-order case (a result recorded before the worker's first dispatch): it seeds a
/// row whose dispatch fields are placeholders (`last_dispatched_task_id = 0`, `total_dispatched =
/// 0`) that the eventual dispatch upsert corrects.
fn upsert_received(
  connection: &mut PgConnection,
  name: &str,
  service_id: i32,
  last_returned_task_id: i64,
  now: SystemTime,
) -> QueryResult<usize> {
  insert_into(worker_metadata::table)
    .values(&NewWorkerMetadata {
      name: name.to_string(),
      service_id,
      last_dispatched_task_id: 0,
      last_returned_task_id: Some(last_returned_task_id),
      total_dispatched: 0,
      total_returned: 1,
      first_seen: now,
      session_seen: Some(now),
      time_last_dispatch: now,
      time_last_return: Some(now),
    })
    .on_conflict((worker_metadata::name, worker_metadata::service_id))
    .do_update()
    .set((
      worker_metadata::last_returned_task_id.eq(last_returned_task_id),
      worker_metadata::total_returned.eq(worker_metadata::total_returned + 1),
      worker_metadata::time_last_return.eq(now),
    ))
    .execute(connection)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::backend;
  use crate::schema::worker_metadata as wm;

  const SERVICE_ID: i32 = 1; // worker_metadata has no FK to services, so any id is fine here

  fn clear(connection: &mut PgConnection, worker: &str) {
    diesel::delete(wm::table.filter(wm::name.eq(worker)))
      .execute(connection)
      .ok();
  }

  fn load(connection: &mut PgConnection, worker: &str) -> WorkerMetadata {
    WorkerMetadata::find_by_name(worker, SERVICE_ID, connection).expect("worker row")
  }

  #[test]
  fn received_before_dispatched_does_not_drop_the_count() {
    let worker = "wm-test:out-of-order:1";
    let mut backend = backend::testdb();
    let connection = &mut backend.connection;
    clear(connection, worker);
    let now = SystemTime::now();

    // Out-of-order: a result is recorded before the worker's first dispatch. The old
    // find-then-update silently dropped this; the upsert must seed the row instead.
    upsert_received(connection, worker, SERVICE_ID, 42, now).expect("received upsert");
    let row = load(connection, worker);
    assert_eq!(row.total_returned, 1, "the early return is not dropped");
    assert_eq!(row.total_dispatched, 0, "no dispatch counted yet");
    assert_eq!(row.last_returned_task_id, Some(42));

    // The dispatch then lands and corrects the dispatch fields without losing the return.
    upsert_dispatched(connection, worker, SERVICE_ID, 99, now).expect("dispatched upsert");
    let row = load(connection, worker);
    assert_eq!(row.total_dispatched, 1, "dispatch now counted");
    assert_eq!(
      row.total_returned, 1,
      "return preserved across the dispatch upsert"
    );
    assert_eq!(row.last_dispatched_task_id, 99);

    clear(connection, worker);
  }

  #[test]
  fn repeated_events_accumulate_in_one_row() {
    let worker = "wm-test:accumulate:1";
    let mut backend = backend::testdb();
    let connection = &mut backend.connection;
    clear(connection, worker);
    let now = SystemTime::now();

    for task in 0..3 {
      upsert_dispatched(connection, worker, SERVICE_ID, task, now).expect("dispatched");
    }
    for task in 0..2 {
      upsert_received(connection, worker, SERVICE_ID, task, now).expect("received");
    }
    // The unique constraint holds: exactly one row, with accumulated tallies.
    let rows: i64 = wm::table
      .filter(wm::name.eq(worker))
      .count()
      .get_result(connection)
      .unwrap();
    assert_eq!(rows, 1, "one row per (name, service_id)");
    let row = load(connection, worker);
    assert_eq!(row.total_dispatched, 3);
    assert_eq!(row.total_returned, 2);

    clear(connection, worker);
  }
}
