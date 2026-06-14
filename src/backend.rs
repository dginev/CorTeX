// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! All aggregate operations over the CorTeX PostgresQL store are accessed through the connection of
//! a `Backend` object.

mod corpora_aggregate;
mod mark;
mod reports;
mod rollup;
mod services_aggregate;
mod tasks_aggregate;
pub(crate) use reports::list_task_diffs;
pub(crate) use reports::progress_report;
pub(crate) use reports::summary_task_diffs;
pub(crate) use reports::task_report;
pub use reports::TaskReportOptions;
pub use rollup::ReportSummaryRow;
pub(crate) use rollup::{category_rollup, category_total, severity_total, what_rollup};

use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use diesel::result::Error;
use diesel::*;
use std::collections::HashMap;
use std::fmt;

use crate::concerns::{CortexDeletable, CortexInsertable};
use crate::config::config;
use crate::helpers::{TaskReport, TaskStatus};
use crate::models::{Corpus, NewTask, Service, Task};

/// The production database postgresql address, from the runtime [`crate::config`] configuration
pub fn default_db_address() -> &'static str { &config().database.url }
/// The test database postgresql address, from the runtime [`crate::config`] configuration
pub fn test_db_address() -> &'static str { &config().database.test_url }

/// Provides an interface to the Postgres task store
pub struct Backend {
  /// The Diesel PgConnection object
  pub connection: PgConnection,
}
impl fmt::Debug for Backend {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("<Backend omitted>") }
}
impl Default for Backend {
  fn default() -> Self {
    let connection = connection_at(default_db_address());
    Backend { connection }
  }
}

/// Constructs a new Task store representation from a Postgres DB address
pub fn connection_at(address: &str) -> PgConnection {
  PgConnection::establish(address).unwrap_or_else(|e| panic!("Error connecting to {address}: {e}"))
}

/// A pool of PostgreSQL connections (Diesel + r2d2).
pub type DbPool = Pool<ConnectionManager<PgConnection>>;
/// A connection checked out from a [`DbPool`]; dereferences to a `PgConnection`.
pub type PooledConn = PooledConnection<ConnectionManager<PgConnection>>;

/// Builds a lazily-initialized connection pool: connections are established on first checkout, so
/// this never blocks or fails at startup even if the database is momentarily unavailable.
pub fn build_pool(database_url: &str, max_size: u32) -> DbPool {
  let manager = ConnectionManager::<PgConnection>::new(database_url);
  Pool::builder().max_size(max_size).build_unchecked(manager)
}

/// The configured database URL, managed as Rocket state so background jobs open their own
/// connection against the same database as the request pool (notably the test database in
/// integration tests).
pub struct DatabaseUrl(pub String);
/// Constructs the default Backend struct for testing
pub fn testdb() -> Backend {
  Backend {
    connection: connection_at(test_db_address()),
  }
}
/// Constructs a Backend at a given address
pub fn from_address(address: &str) -> Backend {
  Backend {
    connection: connection_at(address),
  }
}

/// Options container for relevant fields in requesting a `(corpus, service)` rerun
pub struct RerunOptions<'a> {
  /// corpus to rerun
  pub corpus: &'a Corpus,
  /// service to rerun
  pub service: &'a Service,
  /// optionally, severity level filter
  pub severity_opt: Option<String>,
  /// optionally, category level filter
  pub category_opt: Option<String>,
  /// optionally, what level filter
  pub what_opt: Option<String>,
  /// optionally, owner of the rerun (default is "admin")
  pub owner_opt: Option<String>,
  /// optionally, description of the rerun (default is "rerun")
  pub description_opt: Option<String>,
}

/// Instance methods
impl Backend {
  /// Insert a vector of new `NewTask` tasks into the Task store
  /// For example, on import, or when a new service is activated on a corpus
  pub fn mark_imported(&mut self, imported_tasks: &[NewTask]) -> Result<usize, Error> {
    mark::mark_imported(&mut self.connection, imported_tasks)
  }
  /// Insert a vector of `TaskReport` reports into the Task store, also marking their tasks as
  /// completed with the correct status code.
  pub fn mark_done(&mut self, reports: &[TaskReport]) -> Result<(), Error> {
    mark::mark_done(&mut self.connection, reports)
  }
  /// Given a complex selector, of a `Corpus`, `Service`, and the optional `severity`, `category`
  /// and `what` mark all matching tasks to be rerun
  pub fn mark_rerun(&mut self, options: RerunOptions) -> Result<(), Error> {
    mark::mark_rerun(&mut self.connection, options)
  }

  /// While not changing any status information for Tasks, add a new historical run bookmark
  pub fn mark_new_run(
    &mut self,
    corpus: &Corpus,
    service: &Service,
    owner: String,
    description: String,
  ) -> Result<(), Error> {
    mark::mark_new_run(&mut self.connection, corpus, service, owner, description)
  }

  /// Save the current historical tasks for reference
  pub fn save_historical_tasks(
    &mut self,
    corpus: &Corpus,
    service: &Service,
  ) -> Result<usize, Error> {
    mark::save_historical_tasks(&mut self.connection, corpus, service)
  }

  /// Generic delete method, uses primary "id" field
  pub fn delete<Model: CortexDeletable>(&mut self, object: &Model) -> Result<usize, Error> {
    object.delete_by(&mut self.connection, "id")
  }
  /// Delete all entries matching the "field" value of a given object
  pub fn delete_by<Model: CortexDeletable>(
    &mut self,
    object: &Model,
    field: &str,
  ) -> Result<usize, Error> {
    object.delete_by(&mut self.connection, field)
  }
  /// Generic addition method, attempting to insert in the DB a Task store datum
  /// applicable for any struct implementing the `CortexORM` trait
  /// (for example `Corpus`, `Service`, `Task`)
  pub fn add<Model: CortexInsertable>(&mut self, object: &Model) -> Result<usize, Error> {
    object.create(&mut self.connection)
  }

  /// Fetches no more than `limit` queued tasks for a given `Service`
  pub fn fetch_tasks(&mut self, service: &Service, limit: usize) -> Result<Vec<Task>, Error> {
    tasks_aggregate::fetch_tasks(&mut self.connection, service, limit)
  }
  /// Globally resets any "in progress" tasks back to "queued".
  /// Particularly useful for dispatcher restarts, when all "in progress" tasks need to be
  /// invalidated
  pub fn clear_limbo_tasks(&mut self) -> Result<usize, Error> {
    tasks_aggregate::clear_limbo_tasks(&mut self.connection)
  }

  /// Activates an existing service on a given corpus (via PATH)
  /// if the service has previously been registered, this call will `RESET` the service into a mint
  /// state also removing any related log messages. The new run is attributed to `owner` with
  /// `description` (the UI/API thread the actor; the CLI passes a default).
  pub fn register_service(
    &mut self,
    service: &Service,
    corpus_path: &str,
    owner: String,
    description: String,
  ) -> Result<(), Error> {
    services_aggregate::register_service(
      &mut self.connection,
      service,
      corpus_path,
      owner,
      description,
    )
  }

  /// Extends an existing service on a given corpus (via PATH)
  /// if the service has previously been registered, this call will ignore existing entries and
  /// simply add newly encountered ones
  pub fn extend_service(&mut self, service: &Service, corpus_path: &str) -> Result<(), Error> {
    services_aggregate::extend_service(&mut self.connection, service, corpus_path)
  }

  /// Deletes a service by name
  pub fn delete_service_by_name(&mut self, name: &str) -> Result<usize, Error> {
    services_aggregate::delete_service_by_name(&mut self.connection, name)
  }

  /// Returns a vector of currently available corpora in the Task store
  pub fn corpora(&mut self) -> Vec<Corpus> { corpora_aggregate::list_corpora(&mut self.connection) }

  /// Returns a vector of tasks for a given Corpus, Service and status
  pub fn tasks(
    &mut self,
    corpus: &Corpus,
    service: &Service,
    task_status: &TaskStatus,
  ) -> Vec<Task> {
    reports::list_tasks(&mut self.connection, corpus, service, task_status)
  }
  /// Returns a vector of task entry paths for a given Corpus, Service and status
  pub fn entries(
    &mut self,
    corpus: &Corpus,
    service: &Service,
    task_status: &TaskStatus,
  ) -> Vec<String> {
    reports::list_entries(&mut self.connection, corpus, service, task_status)
  }

  /// Given a complex selector, of a `Corpus`, `Service`, and the optional `severity`, `category`
  /// and `what`, Provide a progress report at the chosen granularity
  pub fn task_report(&mut self, options: TaskReportOptions) -> Vec<HashMap<String, String>> {
    reports::task_report(&mut self.connection, options)
  }
  /// Provides a progress report, grouped by severity, for a given `Corpus` and `Service` pair
  pub fn progress_report(&mut self, corpus: &Corpus, service: &Service) -> HashMap<String, f64> {
    reports::progress_report(&mut self.connection, corpus.id, service.id)
  }

  /// Recomputes the `report_summary` rollup (Arm 14 #6); call on the run-completion path so the
  /// cheap category/what report reads stay fresh.
  pub fn refresh_report_summary(&mut self) -> Result<(), Error> {
    rollup::refresh_report_summary(&mut self.connection)
  }
  /// Category-grain report for `(corpus, service, severity)`, read from the `report_summary`
  /// rollup, windowed to `[offset, offset + limit)` (ordered by descending task count).
  pub fn category_rollup(
    &mut self,
    corpus: &Corpus,
    service: &Service,
    severity: &str,
    limit: i64,
    offset: i64,
  ) -> Vec<ReportSummaryRow> {
    rollup::category_rollup(
      &mut self.connection,
      corpus.id,
      service.id,
      severity,
      limit,
      offset,
    )
    .unwrap_or_default()
  }
  /// `what`-grain drill-down for `(corpus, service, severity, category)`, read from the rollup,
  /// windowed to `[offset, offset + limit)` (ordered by descending task count).
  pub fn what_rollup(
    &mut self,
    corpus: &Corpus,
    service: &Service,
    severity: &str,
    category: &str,
    limit: i64,
    offset: i64,
  ) -> Vec<ReportSummaryRow> {
    rollup::what_rollup(
      &mut self.connection,
      corpus.id,
      service.id,
      severity,
      category,
      limit,
      offset,
    )
    .unwrap_or_default()
  }
}
