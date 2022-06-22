#![allow(clippy::implicit_hasher,clippy::extra_unused_lifetimes)]
use std::fmt;

use diesel::pg::PgConnection;
use diesel::result::Error;
use diesel::*;
use diesel::{insert_into};

use crate::concerns::CortexInsertable;
use crate::schema::log_errors;
use crate::schema::log_fatals;
use crate::schema::log_infos;
use crate::schema::log_invalids;
use crate::schema::log_warnings;

use super::tasks::Task;

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
    writeln!(
      f,
      "{}(category: {},\n\twhat: {},\n\tdetails: {})",
      self.severity(),
      self.category(),
      self.what(),
      self.details()
    )
  }
}
impl fmt::Debug for dyn LogRecord {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { self.debug(f) }
}
impl fmt::Display for dyn LogRecord {
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
