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
use schema::log_infos;
use schema::log_warnings;
use schema::log_errors;
use schema::log_fatals;
use schema::log_invalids;
use concerns::{CortexInsertable, CortexDeletable};


// Tasks

#[derive(Identifiable, Queryable, AsChangeset, Clone)]
/// A CorTeX task, for a given corpus-service pair
pub struct Task {
  /// task primary key, auto-incremented by postgresql
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

// Log Messages

#[derive(Identifiable, Queryable, AsChangeset, Clone)]
/// An info/debug message, as per the `LaTeXML` convention
pub struct LogInfo {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone)]
#[table_name = "log_infos"]
/// A new, insertable, info/debug message
pub struct NewLogInfo {
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}

#[derive(Identifiable, Queryable, AsChangeset, Clone)]
/// A warning message, as per the `LaTeXML` convention
pub struct LogWarning {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone)]
#[table_name = "log_warnings"]
/// A new, insertable, warning message
pub struct NewLogWarning {
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}

#[derive(Identifiable, Queryable, AsChangeset, Clone)]
/// An error message, as per the `LaTeXML` convention
pub struct LogError {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone)]
#[table_name = "log_errors"]
/// A new, insertable, error message
pub struct NewLogError {
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}

#[derive(Identifiable, Queryable, AsChangeset, Clone)]
/// A fatal message, as per the `LaTeXML` convention
pub struct LogFatal {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone)]
#[table_name = "log_fatals"]
/// A new, insertable, fatal message
pub struct NewLogFatal {
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}


#[derive(Identifiable, Queryable, AsChangeset, Clone)]
/// An invalid message, as per the `LaTeXML` convention
pub struct LogInvalid {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone)]
#[table_name = "log_invalids"]
/// A new, insertable, invalid message
pub struct NewLogInvalid {
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}

/// Log actor trait, assumes already Identifiable (for id())
pub trait LogRecord {
  /// Category accessor
  fn category(&self) -> &str;
  /// What accessor
  fn what(&self) -> &str;
  /// Details accessor
  fn details(&self) -> &str;
  /// Details setter
  fn set_details(&mut self, new_details: String);
  /// Severity accessor
  fn severity(&self) -> &str;
  /// Implements the fmt::Debug fmt
  fn debug(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(
      f,
      "{}(category: {},\n\twhat: {},\n\tdetails: {})\n",
      self.severity(),
      self.category(),
      self.what(),
      self.details()
    )
  }
}
impl fmt::Debug for LogRecord {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    self.debug(f)
  }
}
impl fmt::Display for LogRecord {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    self.debug(f)
  }
}

impl LogRecord for LogInfo {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "info"
  }
}
impl LogRecord for NewLogInfo {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "info"
  }
}
impl CortexInsertable for NewLogInfo {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_infos::table).values(self).execute(
      connection,
    )
  }
}
impl LogRecord for LogWarning {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "warning"
  }
}
impl LogRecord for NewLogWarning {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "warning"
  }
}
impl CortexInsertable for NewLogWarning {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_warnings::table).values(self).execute(
      connection,
    )
  }
}
impl LogRecord for LogError {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "error"
  }
}
impl LogRecord for NewLogError {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "error"
  }
}
impl CortexInsertable for NewLogError {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_errors::table).values(self).execute(
      connection,
    )
  }
}
impl LogRecord for LogFatal {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "fatal"
  }
}
impl LogRecord for NewLogFatal {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "fatal"
  }
}
impl CortexInsertable for NewLogFatal {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_fatals::table).values(self).execute(
      connection,
    )
  }
}
impl LogRecord for LogInvalid {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "invalid"
  }
}
impl LogRecord for NewLogInvalid {
  fn category(&self) -> &str {
    &self.category
  }
  fn what(&self) -> &str {
    &self.what
  }
  fn details(&self) -> &str {
    &self.details
  }
  fn set_details(&mut self, new_details: String) {
    self.details = new_details;
  }
  fn severity(&self) -> &str {
    "invalid"
  }
}
impl CortexInsertable for NewLogInvalid {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_invalids::table).values(self).execute(
      connection,
    )
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