// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Backend models and traits for the CorTeX "Task store"

use std::fmt;
use rand::{thread_rng, Rng};
use helpers::TaskStatus;

use diesel::result::Error;
use diesel::{delete, insert_into, update};
use diesel::pg::PgConnection;
use diesel::prelude::*;
use schema::tasks;
use concerns::{CortexInsertable, CortexDeletable};


// Tasks

#[derive(Identifiable, Queryable, AsChangeset, Clone)]
/// A CorTeX task, for a given corpus-service pair
pub struct Task {
  /// optional id (None for mock / yet-to-be-inserted rows)
  pub id: i64,
  /// id of the service owning this task
  pub serviceid: i32,
  /// id of the corpus hosting this task
  pub corpusid: i32,
  /// current processing status of this task
  pub status: i32,
  /// entry path on the file system
  pub entry: String,
}

#[derive(Insertable)]
#[table_name = "tasks"]
/// A new task, to be inserted into CorTeX
pub struct NewTask<'a> {
  /// id of the service owning this task
  pub serviceid: i32,
  /// id of the corpus hosting this task
  pub corpusid: i32,
  /// current processing status of this task
  pub status: i32,
  /// entry path on the file system
  pub entry: &'a str,
}

impl fmt::Display for Task {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    // The `f` value implements the `Write` trait, which is what the
    // write! macro is expecting. Note that this formatting ignores the
    // various flags provided to format strings.
    write!(
      f,
      "(id: {}, entry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n",
      self.id,
      self.entry,
      self.serviceid,
      self.corpusid,
      self.status
    )
  }
}
impl fmt::Debug for Task {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    // The `f` value implements the `Write` trait, which is what the
    // write! macro is expecting. Note that this formatting ignores the
    // various flags provided to format strings.
    write!(
      f,
      "(id: {},\n\tentry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n",
      self.id,
      self.entry,
      self.serviceid,
      self.corpusid,
      self.status
    )
  }
}

impl<'a> CortexInsertable for NewTask<'a> {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(tasks::table).values(self).execute(connection)
  }
}

impl CortexDeletable for Task {
  fn delete_by(&self, connection: &PgConnection, field: &str) -> Result<usize, Error> {
    match field {
      "entry" => self.delete_by_entry(connection),
      "serviceid" => self.delete_by_serviceid(connection),
      "id" => self.delete_by_id(connection),
      _ => Err(Error::QueryBuilderError(
        format!("unknown Task model field: {}", field).into(),
      )),
    }
  }
}
impl Task {
  fn delete_by_entry(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::entry;
    delete(tasks::table.filter(entry.eq(&self.entry))).execute(connection)
  }
  fn delete_by_serviceid(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::serviceid;
    delete(tasks::table.filter(serviceid.eq(&self.serviceid))).execute(connection)
  }
  fn delete_by_id(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::id;
    delete(tasks::table.filter(id.eq(self.id))).execute(connection)
  }
}

impl<'a> CortexDeletable for NewTask<'a> {
  fn delete_by(&self, connection: &PgConnection, field: &str) -> Result<usize, Error> {
    match field {
      "entry" => self.delete_by_entry(connection),
      "serviceid" => self.delete_by_serviceid(connection),
      _ => Err(Error::QueryBuilderError(
        format!("unknown Task model field: {}", field).into(),
      )),
    }
  }
}

impl<'a> NewTask<'a> {
  fn delete_by_entry(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::entry;
    delete(tasks::table.filter(entry.eq(&self.entry))).execute(connection)
  }
  fn delete_by_serviceid(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::serviceid;
    delete(tasks::table.filter(serviceid.eq(&self.serviceid))).execute(connection)
  }
}


// Services
#[derive(Clone)]
/// A `CorTeX` processing service
pub struct Service {
  /// optional id (None for mock / yet-to-be-inserted rows)
  pub id: i32,
  /// a human-readable name for this service
  pub name: String,
  /// a floating-point number to mark the current version (e.g. 0.01)
  pub version: f32,
  /// the expected input format for this service (e.g. tex)
  pub inputformat: String,
  /// the produced output format by this service (e.g. html)
  pub outputformat: String,
  // pub xpath : String,
  // pub resource : String,
  /// prerequisite input conversion service, if any
  pub inputconverter: Option<String>,
  /// is this service requiring more than the main textual content of a document?
  /// mark "true" if unsure
  pub complex: bool,
}

// Aggregate methods, to be used by backend

/// Fetch a batch of `queue_size` TODO tasks for a given `service`.
pub fn fetch_tasks(
  service: &Service,
  queue_size: usize,
  connection: &PgConnection,
) -> Result<Vec<Task>, Error> {
  use schema::tasks::dsl::{serviceid, status};
  let mut rng = thread_rng();
  let mark: u16 = 1 + rng.gen::<u16>();

  let mut marked_tasks: Vec<Task> = Vec::new();
  try!(connection.transaction::<(), Error, _>(|| {
    let tasks_for_update = try!(
      tasks::table
        .for_update()
        .filter(serviceid.eq(service.id))
        .filter(status.eq(TaskStatus::TODO.raw()))
        .limit(queue_size as i64)
        .load(connection)
    );
    marked_tasks = tasks_for_update
      .into_iter()
      .map(|task| {
        Task {
          status: i32::from(mark),
          ..task
        }
      })
      .map(|task| task.save_changes(connection))
      .filter_map(|saved| saved.ok())
      .collect();
    Ok(())
  }));
  Ok(marked_tasks)
}

/// Mark all "limbo" (= "in progress", assumed disconnected) tasks as TODO
pub fn clear_limbo_tasks(connection: &PgConnection) -> Result<usize, Error> {
  use schema::tasks::dsl::status;
  update(tasks::table)
    .filter(status.gt(&TaskStatus::TODO.raw()))
    .set(status.eq(&TaskStatus::TODO.raw()))
    .execute(connection)
}