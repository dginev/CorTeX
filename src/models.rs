// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Backend models and traits for the `CorTeX` "Task store"

use std::fmt;
use std::collections::{BTreeMap, HashMap};
use rand::{thread_rng, Rng};
use helpers::TaskStatus;
use rustc_serialize::json::{Json, ToJson};

use diesel::result::Error;
use diesel::{delete, insert_into, update};
use diesel::pg::PgConnection;
use diesel::prelude::*;
use schema::tasks;
use schema::services;
use schema::corpora;
use schema::log_infos;
use schema::log_warnings;
use schema::log_errors;
use schema::log_fatals;
use schema::log_invalids;
use concerns::{CortexDeletable, CortexInsertable};

// Tasks

#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug, PartialEq, Eq, QueryableByName)]
#[table_name = "tasks"]
/// A `CorTeX` task, for a given corpus-service pair
pub struct Task {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// id of the service owning this task
  pub service_id: i32,
  /// id of the corpus hosting this task
  pub corpus_id: i32,
  /// current processing status of this task
  pub status: i32,
  /// entry path on the file system
  pub entry: String,
}

#[derive(Insertable, Debug, Clone)]
#[table_name = "tasks"]
/// A new task, to be inserted into `CorTeX`
pub struct NewTask {
  /// id of the service owning this task
  pub service_id: i32,
  /// id of the corpus hosting this task
  pub corpus_id: i32,
  /// current processing status of this task
  pub status: i32,
  /// entry path on the file system
  pub entry: String,
}

impl CortexInsertable for NewTask {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(tasks::table).values(self).execute(connection)
  }
}

impl CortexDeletable for Task {
  fn delete_by(&self, connection: &PgConnection, field: &str) -> Result<usize, Error> {
    match field {
      "entry" => self.delete_by_entry(connection),
      "service_id" => self.delete_by_service_id(connection),
      "id" => self.delete_by_id(connection),
      _ => Err(Error::QueryBuilderError(
        format!("unknown Task model field: {}", field).into(),
      )),
    }
  }
}
impl Task {
  /// Delete task by entry
  pub fn delete_by_entry(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::entry;
    delete(tasks::table.filter(entry.eq(&self.entry))).execute(connection)
  }

  /// Delete all tasks matching this task's service id
  pub fn delete_by_service_id(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::service_id;
    delete(tasks::table.filter(service_id.eq(&self.service_id))).execute(connection)
  }

  /// Delete task by id
  pub fn delete_by_id(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::id;
    delete(tasks::table.filter(id.eq(self.id))).execute(connection)
  }

  /// Find task by id, error if none
  pub fn find(taskid: i64, connection: &PgConnection) -> Result<Task, Error> {
    tasks::table.find(taskid).first(connection)
  }

  /// Find task by entry, error if none
  pub fn find_by_entry(entry: &str, connection: &PgConnection) -> Result<Task, Error> {
    tasks::table
      .filter(tasks::entry.eq(entry))
      .first(connection)
  }
}

impl CortexDeletable for NewTask {
  fn delete_by(&self, connection: &PgConnection, field: &str) -> Result<usize, Error> {
    match field {
      "entry" => self.delete_by_entry(connection),
      "service_id" => self.delete_by_service_id(connection),
      _ => Err(Error::QueryBuilderError(
        format!("unknown Task model field: {}", field).into(),
      )),
    }
  }
}

impl NewTask {
  fn delete_by_entry(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::entry;
    delete(tasks::table.filter(entry.eq(&self.entry))).execute(connection)
  }
  fn delete_by_service_id(&self, connection: &PgConnection) -> Result<usize, Error> {
    use schema::tasks::dsl::service_id;
    delete(tasks::table.filter(service_id.eq(&self.service_id))).execute(connection)
  }
  /// Creates the task unless already present in the DB (entry conflict)
  pub fn create_if_new(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(tasks::table)
      .values(self)
      .on_conflict_do_nothing()
      .execute(connection)
  }
}

// Log Messages

#[derive(Identifiable, Queryable, AsChangeset, Associations, Clone, Debug)]
#[belongs_to(Task)]
/// An info/debug message, as per the `LaTeXML` convention
pub struct LogInfo {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// owner task's id
  pub task_id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone, Debug)]
#[table_name = "log_infos"]
/// A new, insertable, info/debug message
pub struct NewLogInfo {
  /// owner task's id
  pub task_id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}

#[derive(Identifiable, Queryable, AsChangeset, Associations, Clone, Debug)]
#[belongs_to(Task)]
/// A warning message, as per the `LaTeXML` convention
pub struct LogWarning {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// owner task's id
  pub task_id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone, Debug)]
#[table_name = "log_warnings"]
/// A new, insertable, warning message
pub struct NewLogWarning {
  /// owner task's id
  pub task_id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}

#[derive(Identifiable, Queryable, AsChangeset, Associations, Clone, Debug)]
#[belongs_to(Task)]
/// An error message, as per the `LaTeXML` convention
pub struct LogError {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// owner task's id
  pub task_id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone, Debug)]
#[table_name = "log_errors"]
/// A new, insertable, error message
pub struct NewLogError {
  /// owner task's id
  pub task_id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}

#[derive(Identifiable, Queryable, AsChangeset, Associations, Clone, Debug)]
#[belongs_to(Task)]
/// A fatal message, as per the `LaTeXML` convention
pub struct LogFatal {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// owner task's id
  pub task_id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone, Debug)]
#[table_name = "log_fatals"]
/// A new, insertable, fatal message
pub struct NewLogFatal {
  /// mid-level description (open set)
  pub category: String,
  /// owner task's id
  pub task_id: i64,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}

#[derive(Identifiable, Queryable, AsChangeset, Clone, Associations, Debug)]
#[belongs_to(Task)]
/// An invalid message, as per the `LaTeXML` convention
pub struct LogInvalid {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// owner task's id
  pub task_id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}
#[derive(Insertable, Clone, Debug)]
#[table_name = "log_invalids"]
/// A new, insertable, invalid message
pub struct NewLogInvalid {
  /// owner task's id
  pub task_id: i64,
  /// mid-level description (open set)
  pub category: String,
  /// low-level description (open set)
  pub what: String,
  /// technical details of the message (e.g. localization info)
  pub details: String,
}

/// Log actor trait, assumes already Identifiable (for id())
pub trait LogRecord {
  /// Owner Task's id accessor
  fn task_id(&self) -> i64;
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
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { self.debug(f) }
}
impl fmt::Display for LogRecord {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { self.debug(f) }
}

impl LogRecord for LogInfo {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "info" }
}
impl LogRecord for NewLogInfo {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "info" }
}
impl CortexInsertable for NewLogInfo {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_infos::table)
      .values(self)
      .execute(connection)
  }
}
impl LogRecord for LogWarning {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "warning" }
}
impl LogRecord for NewLogWarning {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "warning" }
}
impl CortexInsertable for NewLogWarning {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_warnings::table)
      .values(self)
      .execute(connection)
  }
}
impl LogRecord for LogError {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "error" }
}
impl LogRecord for NewLogError {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "error" }
}
impl CortexInsertable for NewLogError {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_errors::table)
      .values(self)
      .execute(connection)
  }
}
impl LogRecord for LogFatal {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "fatal" }
}
impl LogRecord for NewLogFatal {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "fatal" }
}
impl CortexInsertable for NewLogFatal {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_fatals::table)
      .values(self)
      .execute(connection)
  }
}
impl LogRecord for LogInvalid {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "invalid" }
}
impl LogRecord for NewLogInvalid {
  fn task_id(&self) -> i64 { self.task_id }
  fn category(&self) -> &str { &self.category }
  fn what(&self) -> &str { &self.what }
  fn details(&self) -> &str { &self.details }
  fn set_details(&mut self, new_details: String) { self.details = new_details; }
  fn severity(&self) -> &str { "invalid" }
}
impl CortexInsertable for NewLogInvalid {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(log_invalids::table)
      .values(self)
      .execute(connection)
  }
}
// Services
#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug)]
/// A `CorTeX` processing service
pub struct Service {
  /// auto-incremented postgres id
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
/// Insertable struct for `Service`
#[derive(Insertable, Clone, Debug)]
#[table_name = "services"]
pub struct NewService {
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
impl CortexInsertable for NewService {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(services::table)
      .values(self)
      .execute(connection)
  }
}

impl Service {
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_name(name_query: &str, connection: &PgConnection) -> Result<Service, Error> {
    use schema::services::name;
    services::table
      .filter(name.eq(name_query))
      .get_result(connection)
  }

  /// Returns a hash representation of the `Service`, usually for frontend reports
  pub fn to_hash(&self) -> HashMap<String, String> {
    let mut hm = HashMap::new();
    hm.insert("id".to_string(), self.id.to_string());
    hm.insert("name".to_string(), self.name.clone());
    hm.insert("version".to_string(), self.version.to_string());
    hm.insert("inputformat".to_string(), self.inputformat.clone());
    hm.insert("outputformat".to_string(), self.outputformat.clone());
    hm.insert(
      "inputconverter".to_string(),
      match self.inputconverter.clone() {
        Some(ic) => ic,
        None => "None".to_string(),
      },
    );
    hm.insert("complex".to_string(), self.complex.to_string());
    hm
  }
}

// Corpora

#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug)]
#[table_name = "corpora"]
/// A minimal description of a document collection. Defined by a name, path and simple/complex file
/// system setup.
pub struct Corpus {
  /// auto-incremented postgres id
  pub id: i32,
  /// file system path to corpus root
  /// (a corpus is held in a single top-level directory)
  pub path: String,
  /// a human-readable name for this corpus
  pub name: String,
  /// are we using multiple files to represent a document entry?
  /// (if unsure, always use "true")
  pub complex: bool,
}
impl ToJson for Corpus {
  fn to_json(&self) -> Json {
    let mut map = BTreeMap::new();
    map.insert("id".to_string(), self.id.to_json());
    map.insert("path".to_string(), self.path.to_json());
    map.insert("name".to_string(), self.name.to_json());
    map.insert("complex".to_string(), self.complex.to_json());
    Json::Object(map)
  }
}
impl Corpus {
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_name(name_query: &str, connection: &PgConnection) -> Result<Self, Error> {
    use schema::corpora::name;
    corpora::table.filter(name.eq(name_query)).first(connection)
  }
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_path(path_query: &str, connection: &PgConnection) -> Result<Self, Error> {
    use schema::corpora::path;
    corpora::table.filter(path.eq(path_query)).first(connection)
  }
  /// Return a hash representation of the corpus, usually for frontend reports
  pub fn to_hash(&self) -> HashMap<String, String> {
    let mut hm = HashMap::new();
    hm.insert("name".to_string(), self.name.clone());
    hm
  }

  /// Return a vector of services currently activated on this corpus
  pub fn select_services(&self, connection: &PgConnection) -> Result<Vec<Service>, Error> {
    use schema::tasks::dsl::{corpus_id, service_id};
    let corpus_service_ids_query = tasks::table
      .select(service_id)
      .distinct()
      .filter(corpus_id.eq(self.id));
    let services_query = services::table.filter(services::id.eq_any(corpus_service_ids_query));
    let services: Vec<Service> = services_query.get_results(connection).unwrap_or_default();
    Ok(services)
  }

  /// Deletes a corpus and its dependent tasks from the DB, consuming the object
  pub fn destroy(self, connection: &PgConnection) -> Result<usize, Error> {
    try!(
      delete(tasks::table)
        .filter(tasks::corpus_id.eq(self.id))
        .execute(connection)
    );
    try!(
      delete(tasks::table)
        .filter(tasks::entry.eq(self.path))
        .execute(connection)
    );
    delete(corpora::table)
      .filter(corpora::id.eq(self.id))
      .execute(connection)
  }
}

/// Insertable `Corpus` struct
#[derive(Insertable)]
#[table_name = "corpora"]
pub struct NewCorpus {
  /// a human-readable name for this corpus
  pub name: String,
  /// file system path to corpus root
  /// (a corpus is held in a single top-level directory)
  pub path: String,
  /// are we using multiple files to represent a document entry?
  /// (if unsure, always use "true")
  pub complex: bool,
}
impl Default for NewCorpus {
  fn default() -> Self {
    NewCorpus {
      name: "mock corpus".to_string(),
      path: ".".to_string(),
      complex: true,
    }
  }
}
impl CortexInsertable for NewCorpus {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(corpora::table).values(self).execute(connection)
  }
}

// Aggregate methods, to be used by backend

/// Fetch a batch of `queue_size` TODO tasks for a given `service`.
pub fn fetch_tasks(
  service: &Service,
  queue_size: usize,
  connection: &PgConnection,
) -> Result<Vec<Task>, Error>
{
  use schema::tasks::dsl::{service_id, status};
  let mut rng = thread_rng();
  let mark: u16 = 1 + rng.gen::<u16>();

  let mut marked_tasks: Vec<Task> = Vec::new();
  try!(connection.transaction::<(), Error, _>(|| {
    let tasks_for_update = try!(
      tasks::table
        .for_update()
        .filter(service_id.eq(service.id))
        .filter(status.eq(TaskStatus::TODO.raw()))
        .limit(queue_size as i64)
        .load(connection)
    );
    marked_tasks = tasks_for_update
      .into_iter()
      .map(|task| Task {
        status: i32::from(mark),
        ..task
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

/// Task reruns by a variety of selector granularity
pub trait MarkRerun {
  /// Most-specific rerun query, via both category and what filter
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>;
  /// Mid-specificity `category`-filtered reruns
  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>;
}

/// Info level reruns
impl MarkRerun for LogInfo {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_infos::dsl::{category, log_infos, task_id, what};
    let task_ids_to_rerun = log_infos
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_infos::dsl::{category, log_infos, task_id};
    let task_ids_to_rerun = log_infos
      .filter(category.eq(rerun_category))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }
}

/// Warning level reruns
impl MarkRerun for LogWarning {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_warnings::dsl::{category, log_warnings, task_id, what};
    let task_ids_to_rerun = log_warnings
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_warnings::dsl::{category, log_warnings, task_id};
    let task_ids_to_rerun = log_warnings
      .filter(category.eq(rerun_category))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }
}

/// Error level reruns
impl MarkRerun for LogError {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_errors::dsl::{category, log_errors, task_id, what};
    let task_ids_to_rerun = log_errors
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_errors::dsl::{category, log_errors, task_id};
    let task_ids_to_rerun = log_errors
      .filter(category.eq(rerun_category))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }
}
/// Fatal level reruns
impl MarkRerun for LogFatal {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_fatals::dsl::{category, log_fatals, task_id, what};
    let task_ids_to_rerun = log_fatals
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_fatals::dsl::{category, log_fatals, task_id};
    let task_ids_to_rerun = log_fatals
      .filter(category.eq(rerun_category))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }
}

/// Invalid level reruns
impl MarkRerun for LogInvalid {
  fn mark_rerun_by_what(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    rerun_what: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_invalids::dsl::{category, log_invalids, task_id, what};
    let task_ids_to_rerun = log_invalids
      .filter(category.eq(rerun_category))
      .filter(what.eq(rerun_what))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }

  fn mark_rerun_by_category(
    mark: i32,
    corpus_id: i32,
    service_id: i32,
    rerun_category: &str,
    connection: &PgConnection,
  ) -> Result<usize, Error>
  {
    use schema::log_invalids::dsl::{category, log_invalids, task_id};
    let task_ids_to_rerun = log_invalids
      .filter(category.eq(rerun_category))
      .select(task_id)
      .distinct();

    update(tasks::table)
      .filter(tasks::corpus_id.eq(&corpus_id))
      .filter(tasks::service_id.eq(&service_id))
      .filter(tasks::id.eq_any(task_ids_to_rerun))
      .set(tasks::status.eq(mark))
      .execute(connection)
  }
}
