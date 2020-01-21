// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! All aggregate operations over the CorTeX PostgresQL store are accessed through the connection of
//! a `Backend` object.

mod cached;
mod mark;
mod reports;
mod services_aggregate;
mod tasks_aggregate;
mod users_aggregate;
pub use cached::cache_worker;
pub use cached::task_report as cached_task_report;
pub(crate) use reports::progress_report;
pub use reports::TaskReportOptions;

use diesel::pg::PgConnection;
use diesel::result::Error;
use diesel::*;
use dotenv::dotenv;
use std::collections::HashMap;
use std::process::{Child, Command};
use std::time::SystemTime;
use sysinfo::{System, SystemExt};

use crate::concerns::{CortexDeletable, CortexInsertable};
use crate::helpers::{TaskReport, TaskStatus};
use crate::models::{Corpus, DaemonProcess, NewDaemonProcess, NewTask, Service, Task, User};

/// The production database postgresql address, set from the .env configuration file
pub const DEFAULT_DB_ADDRESS: &str = dotenv!("DATABASE_URL");
/// The test database postgresql address, set from the .env configuration file
pub const TEST_DB_ADDRESS: &str = dotenv!("TEST_DATABASE_URL");

/// Provides an interface to the Postgres task store
pub struct Backend {
  /// The Diesel PgConnection object
  pub connection: PgConnection,
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
  PgConnection::establish(address).unwrap_or_else(|_| panic!("Error connecting to {}", address))
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
  pub fn mark_imported(&self, imported_tasks: &[NewTask]) -> Result<usize, Error> {
    mark::mark_imported(&self.connection, imported_tasks)
  }
  /// Insert a vector of `TaskReport` reports into the Task store, also marking their tasks as
  /// completed with the correct status code.
  pub fn mark_done(&self, reports: &[TaskReport]) -> Result<(), Error> {
    mark::mark_done(&self.connection, reports)
  }
  /// Given a complex selector, of a `Corpus`, `Service`, and the optional `severity`, `category`
  /// and `what` mark all matching tasks to be rerun
  pub fn mark_rerun(&self, options: RerunOptions) -> Result<(), Error> {
    mark::mark_rerun(&self.connection, options)
  }

  /// While not changing any status information for Tasks, add a new historical run bookmark
  pub fn mark_new_run(
    &self,
    corpus: &Corpus,
    service: &Service,
    owner: String,
    description: String,
  ) -> Result<(), Error>
  {
    mark::mark_new_run(&self.connection, corpus, service, owner, description)
  }

  /// Generic delete method, uses primary "id" field
  pub fn delete<Model: CortexDeletable>(&self, object: &Model) -> Result<usize, Error> {
    object.delete_by(&self.connection, "id")
  }
  /// Delete all entries matching the "field" value of a given object
  pub fn delete_by<Model: CortexDeletable>(
    &self,
    object: &Model,
    field: &str,
  ) -> Result<usize, Error>
  {
    object.delete_by(&self.connection, field)
  }
  /// Generic addition method, attempting to insert in the DB a Task store datum
  /// applicable for any struct implementing the `CortexORM` trait
  /// (for example `Corpus`, `Service`, `Task`)
  pub fn add<Model: CortexInsertable>(&self, object: &Model) -> Result<usize, Error> {
    object.create(&self.connection)
  }

  /// Fetches no more than `limit` queued tasks for a given `Service`
  pub fn fetch_tasks(&self, service: &Service, limit: usize) -> Result<Vec<Task>, Error> {
    tasks_aggregate::fetch_tasks(&self.connection, service, limit)
  }
  /// Globally resets any "in progress" tasks back to "queued".
  /// Particularly useful for dispatcher restarts, when all "in progress" tasks need to be
  /// invalidated
  pub fn clear_limbo_tasks(&self) -> Result<usize, Error> {
    tasks_aggregate::clear_limbo_tasks(&self.connection)
  }

  /// Activates an existing service on a given corpus (via PATH)
  /// if the service has previously been registered, this call will `RESET` the service into a mint
  /// state also removing any related log messages.
  pub fn register_service(&self, service: &Service, corpus_path: &str) -> Result<(), Error> {
    services_aggregate::register_service(&self.connection, service, corpus_path)
  }

  /// Extends an existing service on a given corpus (via PATH)
  /// if the service has previously been registered, this call will ignore existing entries and
  /// simply add newly encountered ones
  pub fn extend_service(&self, service: &Service, corpus_path: &str) -> Result<(), Error> {
    services_aggregate::extend_service(&self.connection, service, corpus_path)
  }

  /// Deletes a service by name
  pub fn delete_service_by_name(&self, name: &str) -> Result<usize, Error> {
    services_aggregate::delete_service_by_name(&self.connection, name)
  }

  /// Returns a vector of currently available corpora in the DB
  pub fn corpora(&self) -> Vec<Corpus> { Corpus::all(&self.connection).unwrap_or_default() }

  /// Returns a vector of currently available services in the DB
  pub fn services(&self) -> Vec<Service> { Service::all(&self.connection).unwrap_or_default() }

  /// Returns a vector of currently registered users
  /// (currently we expect only few admin/dev users, so this should be a very fast query)
  pub fn users(&self) -> Vec<User> { users_aggregate::list_users(&self.connection) }

  /// Returns a vector of tasks for a given Corpus, Service and status
  pub fn tasks(&self, corpus: &Corpus, service: &Service, task_status: &TaskStatus) -> Vec<Task> {
    reports::list_tasks(&self.connection, corpus, service, task_status)
  }
  /// Returns a vector of task entry paths for a given Corpus, Service and status
  pub fn entries(
    &self,
    corpus: &Corpus,
    service: &Service,
    task_status: &TaskStatus,
  ) -> Vec<String>
  {
    reports::list_entries(&self.connection, corpus, service, task_status)
  }

  /// Given a complex selector, of a `Corpus`, `Service`, and the optional `severity`, `category`
  /// and `what`, Provide a progress report at the chosen granularity
  pub fn task_report(&self, options: TaskReportOptions) -> Vec<HashMap<String, String>> {
    reports::task_report(&self.connection, options)
  }
  /// Provides a progress report, grouped by severity, for a given `Corpus` and `Service` pair
  pub fn progress_report(&self, corpus: &Corpus, service: &Service) -> HashMap<String, f64> {
    reports::progress_report(&self.connection, corpus.id, service.id)
  }

  /// Ensure a named daemon is running, or spin it up if not
  pub fn ensure_daemon(&self, name: &str) -> Result<Option<Child>, Box<dyn std::error::Error>> {
    // whitelist available daemons, not meant for general purpose calls..
    if name != "cache_worker" && name != "dispatcher" {
      Err("only supported cortex binaries can be executed as daemons".into())
    } else {
      let mut is_running = false;
      if let Ok(process_record) = DaemonProcess::find_by_name(name, &self.connection) {
        // println!("Found record for {:?}: {:?}", name, process_record);
        // we have a record, check if it is running with the OS
        if let Some(_process) = System::new().get_process(process_record.pid) {
          is_running = true;
          process_record.touch(&self.connection)?;
        // println!("Record pid {:?} is still alive!", process_record.pid);
        } else {
          // in the case the record is stale, remove it
          // println!(
          //   "Record pid {:?} is stale - removing from DB.",
          //   process_record.pid
          // );
          process_record.destroy(&self.connection)?;
        }
      }
      if is_running {
        Ok(None) // already running, nothing to do
      } else {
        match Command::new("cargo").args(&["run", "--bin", name]).spawn() {
          Ok(child) => {
            let pid = child.id() as i32;
            println!(
              "Registering new {:?} record at freshly created process id {:?}",
              name, pid
            );
            NewDaemonProcess {
              name: name.to_owned(),
              pid,
              first_seen: SystemTime::now(),
              last_seen: SystemTime::now(),
            }
            .create(&self.connection)?;
            Ok(Some(child))
          }, // register pid with lookup table
          Err(e) => Err(e.into()),
        }
      }
    }
  }

  /// When we start the cortex daemons ourselves, outside of the frontend logic,
  /// one has to register them with the DB, so that the frontend is aware of them
  pub fn override_daemon_record(&self, name: String, pid: u32) -> Result<usize, Error> {
    // delete any existing record, then add the new one.
    if let Ok(process_record) = DaemonProcess::find_by_name(&name, &self.connection) {
      // we have a record, check if it is running with the OS
      process_record.destroy(&self.connection)?;
    }

    NewDaemonProcess {
      name,
      pid: pid as i32,
      first_seen: SystemTime::now(),
      last_seen: SystemTime::now(),
    }
    .create(&self.connection)
  }
}
