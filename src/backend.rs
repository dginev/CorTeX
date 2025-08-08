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
mod services_aggregate;
mod tasks_aggregate;
pub(crate) use reports::progress_report;
pub use reports::TaskReportOptions;

use diesel::result::Error;
use diesel::*;
use dotenv::dotenv;
use std::collections::HashMap;
use std::fmt;

use crate::concerns::{CortexDeletable, CortexInsertable};
use crate::helpers::{TaskReport, TaskStatus};
use crate::models::{
  Corpus, DiffStatusFilter, DiffStatusRow, NewTask, Service, Task, TaskRunMetadata,
};
use chrono::NaiveDateTime;

/// The production database postgresql address, set from the .env configuration file
pub const DEFAULT_DB_ADDRESS: &str = dotenv!("DATABASE_URL");
/// The test database postgresql address, set from the .env configuration file
pub const TEST_DB_ADDRESS: &str = dotenv!("TEST_DATABASE_URL");

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
    dotenv().ok();
    let connection = connection_at(DEFAULT_DB_ADDRESS);

    Backend { connection }
  }
}

/// Constructs a new Task store representation from a Postgres DB address
pub fn connection_at(address: &str) -> PgConnection {
  PgConnection::establish(address).expect("Error connecting to {address}")
}
/// Constructs the default Backend struct for testing
pub fn testdb() -> Backend {
  dotenv().ok();
  Backend {
    connection: connection_at(TEST_DB_ADDRESS),
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
  /// state also removing any related log messages.
  pub fn register_service(&mut self, service: &Service, corpus_path: &str) -> Result<(), Error> {
    services_aggregate::register_service(&mut self.connection, service, corpus_path)
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

  /// Prepares a template-friendly report of task differences
  pub fn list_task_diffs(
    &mut self,
    corpus: &Corpus,
    service: &Service,
    filters: DiffStatusFilter,
  ) -> Vec<TaskRunMetadata> {
    reports::list_task_diffs(&mut self.connection, corpus, service, filters)
  }

  /// Prepares a template-friendly summary of task differences
  pub fn summary_task_diffs(
    &mut self,
    corpus: &Corpus,
    service: &Service,
    previous_date: Option<NaiveDateTime>,
    current_date: Option<NaiveDateTime>,
  ) -> (Vec<String>, Vec<DiffStatusRow>) {
    reports::summary_task_diffs(
      &mut self.connection,
      corpus,
      service,
      previous_date,
      current_date,
    )
  }
}
