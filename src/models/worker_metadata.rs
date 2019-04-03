use std::collections::HashMap;
use std::thread;
use std::time::SystemTime;

use diesel::pg::PgConnection;
use diesel::result::Error;
use diesel::*;
use diesel::{insert_into, update};

use serde::Serialize;

use crate::backend;
use crate::schema::worker_metadata;

#[derive(Insertable, Debug)]
#[table_name = "worker_metadata"]
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
#[table_name = "worker_metadata"]
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
    wh.insert("name".to_string(), worker.name.to_string());
    wh
  }
}

fn since_string(then: SystemTime, is_fresh: &mut bool) -> String {
  let now = SystemTime::now();
  let since_duration = now.duration_since(then).unwrap();
  let secs = since_duration.as_secs();
  if secs < 60 {
    *is_fresh = true;
    format!("{} seconds ago", secs)
  } else if secs < 3_600 {
    format!("{} minutes ago", secs / 60)
  } else if secs < 86_400 {
    format!("{} hours ago", secs / 3_600)
  } else {
    format!("{} days ago", secs / 86_400)
  }
}

impl WorkerMetadata {
  /// Update the metadata for a worker which was just dispatched to
  pub fn record_dispatched(
    name: String,
    service_id: i32,
    last_dispatched_task_id: i64,
    backend_address: String,
  ) -> Result<(), Error>
  {
    let now = SystemTime::now();
    let _ = thread::spawn(move || {
      let backend = backend::from_address(&backend_address);
      match WorkerMetadata::find_by_name(&name, service_id, &backend.connection) {
        Ok(data) => {
          // update with the appropriate fields.
          let session_seen = match data.session_seen {
            Some(time) => time,
            None => now,
          };
          update(&data)
            .set((
              worker_metadata::last_dispatched_task_id.eq(last_dispatched_task_id),
              worker_metadata::total_dispatched.eq(worker_metadata::total_dispatched + 1),
              worker_metadata::time_last_dispatch.eq(now),
              worker_metadata::session_seen.eq(Some(session_seen)),
            ))
            .execute(&backend.connection)
            .unwrap_or(0);
        },
        _ => {
          let data = NewWorkerMetadata {
            name,
            service_id,
            last_dispatched_task_id,
            last_returned_task_id: None,
            total_dispatched: 1,
            total_returned: 0,
            first_seen: now,
            session_seen: Some(now),
            time_last_dispatch: now,
            time_last_return: None,
          };
          insert_into(worker_metadata::table)
            .values(&data)
            .execute(&backend.connection)
            .unwrap_or(0);
        },
      }
    });
    Ok(())
  }
  /// Update the metadata for a worker which was just received from
  pub fn record_received(
    identity: String,
    service_id: i32,
    last_returned_task_id: i64,
    backend_address: String,
  ) -> Result<(), Error>
  {
    let now = SystemTime::now();
    let _ = thread::spawn(move || {
      let backend = backend::from_address(&backend_address);
      if let Ok(data) = WorkerMetadata::find_by_name(&identity, service_id, &backend.connection) {
        let session_seen = match data.session_seen {
          Some(time) => time,
          None => now,
        };
        update(&data)
          .set((
            worker_metadata::last_returned_task_id.eq(last_returned_task_id),
            worker_metadata::total_returned.eq(worker_metadata::total_returned + 1),
            worker_metadata::time_last_return.eq(now),
            worker_metadata::session_seen.eq(Some(session_seen)),
          ))
          .execute(&backend.connection)
          .unwrap_or(0);
      } else {
        println!(
          "-- Can't record worker metadata for unknown worker: {:?} {:?}",
          identity, service_id
        );
      }
    });
    Ok(())
  }

  /// Load worker metadata record by identity and service id
  pub fn find_by_name(
    identity: &str,
    sid: i32,
    connection: &PgConnection,
  ) -> Result<WorkerMetadata, Error>
  {
    use crate::schema::worker_metadata::{name, service_id};
    worker_metadata::table
      .filter(name.eq(identity))
      .filter(service_id.eq(sid))
      .get_result(connection)
  }
}
