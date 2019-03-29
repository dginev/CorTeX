use diesel::*;
use std::time::SystemTime;

// use super::messages::*;
// use super::tasks::Task;
// use crate::helpers::TaskStatus;
use crate::schema::historical_runs;

#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug, PartialEq, Eq, QueryableByName)]
#[table_name = "historical_runs"]
/// Historical `(Corpus, Service)` run records
pub struct HistoricalRun {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// id of the service owning this task
  pub service_id: i32,
  /// id of the corpus hosting this task
  pub corpus_id: i32,
  /// description of the purpose of this run
  pub description: String,
  /// owner who initiated the run
  pub owner: String,
  /// total tasks in run
  pub total: i32,
  /// fatal results in run
  pub fatal: i32,
  /// error results in run
  pub error: i32,
  /// warning results in run
  pub warning: i32,
  /// invalid tasks in run
  pub invalid: i32,
  /// results with no notable problems in run
  pub no_problem: i32,
  /// info messages count in run
  pub log_info: i32,
  /// warning messages count in run
  pub log_warning: i32,
  /// error messages count in run
  pub log_error: i32,
  /// fatal messages count in run
  pub log_fatal: i32,
  /// start timestamp of run
  pub start_time: SystemTime,
  /// end timestamp of run
  pub end_time: Option<SystemTime>,
}

#[derive(Insertable, Debug, Clone)]
#[table_name = "historical_runs"]
/// A new task, to be inserted into `CorTeX`
pub struct NewHistoricalRun {
  /// id of the service owning this task
  pub service_id: i32,
  /// id of the corpus hosting this task
  pub corpus_id: i32,
  /// description of the purpose of this run
  pub description: String,
  /// owner who initiated the run
  pub owner: String,
}
