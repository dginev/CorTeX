#![allow(clippy::extra_unused_lifetimes)]
use std::collections::HashMap;
use std::thread;
use std::time::SystemTime;

use diesel::result::Error;
use diesel::*;

use serde::Serialize;

use crate::backend::DbPool;
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

#[derive(Identifiable, Queryable, Clone, Debug, Serialize)]
#[diesel(table_name = worker_metadata)]
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

    let mut fresh = false;
    wh.insert(
      "first_seen".to_string(),
      since_string(worker.first_seen, &mut fresh),
    );

    wh.insert(
      "session_seen".to_string(),
      match worker.session_seen {
        Some(session_seen) => since_string(session_seen, &mut fresh),
        None => String::new(),
      },
    );

    wh.insert(
      "time_last_dispatch".to_string(),
      since_string(worker.time_last_dispatch, &mut fresh),
    );
    wh.insert(
      "time_last_return".to_string(),
      match worker.time_last_return {
        Some(time_last_return) => since_string(time_last_return, &mut fresh),
        None => String::new(),
      },
    );
    wh.insert(
      "fresh".to_string(),
      if fresh { "fresh" } else { "stale" }.to_string(),
    );
    wh.insert("name".to_string(), worker.name);
    wh
  }
}

fn since_string(then: SystemTime, is_fresh: &mut bool) -> String {
  let now = SystemTime::now();
  let since_duration = now.duration_since(then).unwrap();
  let secs = since_duration.as_secs();
  if secs < 60 {
    *is_fresh = true;
    format!("{secs} seconds ago")
  } else if secs < 3_600 {
    format!("{} minutes ago", secs / 60)
  } else if secs < 86_400 {
    format!("{} hours ago", secs / 3_600)
  } else {
    format!("{} days ago", secs / 86_400)
  }
}

impl WorkerMetadata {
  /// Records a dispatch to a worker. Upserts (insert-or-increment) keyed by `(name, service_id)`,
  /// so the write is correct regardless of the order in which this and the matching return
  /// event's metadata writes complete (KNOWN_ISSUES D-2). Runs off-thread so the ventilator's hot
  /// loop never blocks on the DB; a pooled checkout (~11µs) replaces a fresh `PgConnection`
  /// (~4.5ms, the Arm 14 spike). NB: the thread-per-event spawn itself is still unbounded (D-1).
  pub fn record_dispatched(
    name: String,
    service_id: i32,
    last_dispatched_task_id: i64,
    pool: DbPool,
  ) -> Result<(), Error> {
    let now = SystemTime::now();
    let _ = thread::spawn(move || {
      let mut pooled = match pool.get() {
        Ok(connection) => connection,
        Err(_) => return,
      };
      if let Err(error) =
        upsert_dispatched(&mut pooled, &name, service_id, last_dispatched_task_id, now)
      {
        eprintln!(
          "-- worker metadata (dispatched) upsert failed for {name:?}/{service_id}: {error:?}"
        );
      }
    });
    Ok(())
  }

  /// Records a result returned by a worker. Upserts keyed by `(name, service_id)`: unlike the old
  /// find-then-update, this never drops the count when the worker row does not exist yet (the
  /// sink's metadata write can outrun the ventilator's insert) — it inserts a row carrying the
  /// return (KNOWN_ISSUES D-2). Off-thread so the sink never blocks on the DB.
  pub fn record_received(
    identity: String,
    service_id: i32,
    last_returned_task_id: i64,
    pool: DbPool,
  ) -> Result<(), Error> {
    let now = SystemTime::now();
    let _ = thread::spawn(move || {
      let mut pooled = match pool.get() {
        Ok(connection) => connection,
        Err(_) => return,
      };
      if let Err(error) = upsert_received(
        &mut pooled,
        &identity,
        service_id,
        last_returned_task_id,
        now,
      ) {
        eprintln!(
          "-- worker metadata (received) upsert failed for {identity:?}/{service_id}: {error:?}"
        );
      }
    });
    Ok(())
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
