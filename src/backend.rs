// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! ORM-like capabilities for high- and mid-level operations on the Task store
extern crate postgres;
extern crate rustc_serialize;
extern crate rand;
extern crate dotenv;
extern crate r2d2;

use dotenv::dotenv;
use std::thread;
use std::clone::Clone;
use std::collections::{HashMap, HashSet};
use regex::Regex;
use rand::{thread_rng, Rng};
use diesel::{update, delete, insert_into};
use diesel::prelude::*;
use diesel::pg::PgConnection;
use diesel::pg::upsert::*;
use diesel::result::Error;
use schema::{tasks, log_infos, log_warnings, log_errors, log_fatals, log_invalids};

// use data::{CortexORM, Corpus, Service, Task, TaskReport, TaskStatus};
use concerns::{CortexInsertable, CortexDeletable};
use models;
use models::{Task, NewTask, Service, LogRecord};
use helpers::{TaskStatus, TaskReport};
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
  PgConnection::establish(address).expect(&format!("Error connecting to {}", address))
}

/// Constructs the default Backend struct for testing
pub fn testdb() -> Backend {
  dotenv().ok();
  Backend { connection: connection_at(TEST_DB_ADDRESS) }
}

/// Instance methods
impl Backend {
  /// Insert a vector of new `NewTask` tasks into the Task store
  /// For example, on import, or when a new service is activated on a corpus
  pub fn mark_imported(&self, imported_tasks: &[NewTask]) -> Result<(), Box<Error>> {
    // Insert, but only if the task is new (allow for extension calls with the same method)
    try!(
      insert_into(tasks::table)
        .values(imported_tasks)
        .on_conflict_do_nothing()
        .execute(&self.connection)
    );

    Ok(())
  }

  /// Insert a vector of `TaskReport` reports into the Task store, also marking their tasks as completed with the correct status code.
  pub fn mark_done(&self, reports: &[TaskReport]) -> Result<(), Error> {
    use schema::tasks::dsl::{id, status};

    try!(self.connection.transaction::<(), Error, _>(|| {
      for report in reports.iter() {
        // Update the status
        try!(
          update(tasks::table)
            .filter(id.eq(report.task.id))
            .set(status.eq(report.status.raw()))
            .execute(&self.connection)
        );
        // Next, delete all previous log messages for this task.id
        try!(
          delete(log_infos::table)
            .filter(log_infos::dsl::taskid.eq(report.task.id))
            .execute(&self.connection)
        );
        try!(
          delete(log_warnings::table)
            .filter(log_warnings::dsl::taskid.eq(report.task.id))
            .execute(&self.connection)
        );
        try!(
          delete(log_errors::table)
            .filter(log_errors::dsl::taskid.eq(report.task.id))
            .execute(&self.connection)
        );
        try!(
          delete(log_fatals::table)
            .filter(log_fatals::dsl::taskid.eq(report.task.id))
            .execute(&self.connection)
        );
        try!(
          delete(log_invalids::table)
            .filter(log_invalids::dsl::taskid.eq(report.task.id))
            .execute(&self.connection)
        );
        // Clean slate, so proceed to add the new messages
        for message in &report.messages {
          if (message.severity() != "status") {
            // Warnings, Errors and Fatals will get added:
            // TODO: NEXT UP!
            // message.create();
          }
        }
        // TODO: Update dependenct services, when integrated in DB
      }
      Ok(())
    }));
    Ok(())
  }

  //   /// Given a complex selector, of a `Corpus`, `Service`, and the optional `severity`, `category` and `what`
  //   /// mark all matching tasks to be rerun
  //   pub fn mark_rerun(&self, corpus: &Corpus, service: &Service, severity: Option<String>, category: Option<String>, what: Option<String>) -> Result<(), Error> {

  //     let mut rng = thread_rng();
  //     let mark_rng: u16 = rng.gen();
  //     let mark: i32 = !(mark_rng as i32);

  //     // First, mark as blocked all of the tasks in the chosen scope, using a special mark
  //     match severity {
  //       Some(severity) => {
  //         match category {
  //           Some(category) => {
  //             match what {
  //               Some(what) => {
  //                 // All tasks in a "what" class
  //                 try!(self.connection
  //                          .execute("UPDATE tasks SET status=$1 where corpusid=$2 and serviceid=$3 and taskid in (select distinct(taskid) from logs where severity=$4 and category=$5 and what=$6)",
  //                                   &[&mark, &corpus.id.unwrap(), &service.id.unwrap(), &severity, &category, &what]));
  //               }
  //               None => {
  //                 // All tasks in a category
  //                 try!(self.connection.execute("UPDATE tasks SET status=$1 where corpusid=$2 and serviceid=$3 and taskid in (select distinct(taskid) from logs where severity=$4 and category=$5)",
  //                                              &[&mark, &corpus.id.unwrap(), &service.id.unwrap(), &severity, &category]));
  //               }
  //             };
  //           }
  //           None => {
  //             // All tasks in a certain status
  //             let status: i32 = TaskStatus::from_key(&severity).raw();
  //             try!(self.connection.execute("UPDATE tasks SET status=$1 where corpusid=$2 and serviceid=$3 and status=$4",
  //                                          &[&mark, &corpus.id.unwrap(), &service.id.unwrap(), &status]));
  //           }
  //         }
  //       }
  //       None => {
  //         // Entire corpus
  //         try!(self.connection.execute("UPDATE tasks SET status=$1 where corpusid=$2 and serviceid=$3",
  //                                      &[&mark, &corpus.id.unwrap(), &service.id.unwrap()]));
  //       }
  //     };

  //     // Next, delete all logs for the blocked tasks.
  //     // Note that if we are using a negative blocking status, this query should get sped up via an "Index Scan using log_taskid on logs"
  //     try!(self.connection.execute("DELETE from logs USING tasks WHERE logs.taskid=tasks.taskid and tasks.status=$1 and tasks.corpusid=$2 and tasks.serviceid=$3;",
  //                                  &[&mark, &corpus.id.unwrap(), &service.id.unwrap()]));

  //     // Lastly, switch all blocked tasks to "queued", and complete the rerun mark pass.
  //     try!(self.connection.execute("UPDATE tasks set status=$1 where status=$2 and corpusid=$3 and serviceid=$4;",
  //                                  &[&TaskStatus::TODO.raw(), &mark, &corpus.id.unwrap(), &service.id.unwrap()]));
  //     Ok(())
  //   }

  // /// Generic sync method, attempting to obtain the DB record for a given mock Task store datum
  // /// applicable for any struct implementing the `CortexORM` trait
  // /// (for example `Corpus`, `Service`, `Task`)
  // pub fn sync<D: CortexORM + Clone>(&self, d: &D) -> Result<D, Box<Error>> {
  //   let synced = match d.get_id() {
  //     Some(_) => try!(d.select_by_id(&self.connection)),
  //     None => try!(d.select_by_key(&self.connection)),
  //   };
  //   match synced {
  //     Some(synced_d) => Ok(synced_d),
  //     None => Ok(d.clone()),
  //   }
  // }

  /// Generic delete method, uses primary "id" field
  pub fn delete<Model: CortexDeletable>(&self, object: &Model) -> Result<usize, Error> {
    object.delete_by(&self.connection, "id")
  }

  /// Delete all entries matching the "field" value of a given object
  pub fn delete_by<Model: CortexDeletable>(
    &self,
    object: &Model,
    field: &str,
  ) -> Result<usize, Error> {
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
    models::fetch_tasks(service, limit, &self.connection)
  }

  /// Globally resets any "in progress" tasks back to "queued".
  /// Particularly useful for dispatcher restarts, when all "in progress" tasks need to be invalidated
  pub fn clear_limbo_tasks(&self) -> Result<usize, Error> {
    models::clear_limbo_tasks(&self.connection)
  }

  //   /// Activates an existing service on a given corpus (via NAME)
  //   /// if the service has previously been registered, this has "extend" semantics, without any "overwrite" or "reset"
  //   pub fn register_service(&self, service: Service, corpus_name: String) -> Result<(), Error> {
  //     let corpus_placeholder = Corpus {
  //       id: None,
  //       path: String::new(),
  //       name: corpus_name,
  //       complex: true,
  //     };
  //     let corpus = self.sync(&corpus_placeholder).unwrap();
  //     let corpusid = corpus.id.unwrap();
  //     let serviceid = service.id.unwrap();
  //     let todo_raw = TaskStatus::TODO.raw();

  //     // If we wanted to erase old tasks for this service, we could do as follows, but there is a lot more logic missing
  //     // - also erase log entries
  //     // - update dependencies
  //     // so instead, for now we'll just add new tasks, leaving existing ones as-is.
  //     // try!(self.connection.execute("DELETE from tasks where serviceid=$1 AND corpusid=$2", &[&serviceid, &corpusid]));
  //     let mut prior_set: HashSet<String> = HashSet::new();
  //     let prior_entries_query = try!(self.connection.prepare("SELECT entry from tasks where serviceid=$1 AND corpusid=$2"));
  //     let prior_entries = try!(prior_entries_query.query(&[&serviceid, &corpusid]));
  //     for prior_entry in prior_entries.iter() {
  //       let entry: String = prior_entry.get(0);
  //       prior_set.insert(entry);
  //     }

  //     let task_entries_query = try!(self.connection.prepare("SELECT entry from tasks where serviceid=2 AND corpusid=$1"));
  //     let task_entries = try!(task_entries_query.query(&[&corpusid]));
  //     let trans = try!(self.connection.transaction());
  //     for task_entry in task_entries.iter() {
  //       let entry: String = task_entry.get(0);
  //       if prior_set.contains(&entry) {
  //         continue;
  //       }
  //       trans.execute("INSERT INTO tasks (entry,serviceid,corpusid, status) VALUES($1,$2,$3,$4) ON CONFLICT(entry, serviceid, corpusid) DO NOTHING;",
  //                     &[&entry, &serviceid, &corpusid, &todo_raw])
  //            .unwrap();
  //     }
  //     trans.set_commit();
  //     try!(trans.finish());
  //     Ok(())
  //   }

  //   /// Returns a vector of currently available corpora in the Task store
  //   pub fn corpora(&self) -> Vec<Corpus> {
  //     let mut corpora = Vec::new();
  //     if let Ok(select_query) = self.connection.prepare("SELECT corpusid,name,path,complex FROM corpora order by name") {
  //       if let Ok(rows) = select_query.query(&[]) {
  //         for row in rows.iter() {
  //           corpora.push(Corpus::from_row(row));
  //         }
  //       }
  //     }
  //     corpora
  //   }

  //   /// Returns a vector of tasks for a given Corpus, Service and status
  //   pub fn entries(&self, corpus: &Corpus, service: &Service, status: &TaskStatus) -> Vec<String> {
  //     let raw_status = status.raw();
  //     match self.connection.prepare("select entry from tasks where serviceid=$1 and corpusid=$2 and status=$3") {
  //       Ok(select_query) => {
  //         match select_query.query(&[&service.id.unwrap_or(-1), &corpus.id.unwrap_or(-1), &raw_status]) {
  //           Ok(entry_rows) => {
  //             let entry_name_regex = Regex::new(r"^(.+)/[^/]+$").unwrap();
  //             let mut entries = Vec::new();
  //             for row in entry_rows.iter() {
  //               let entry_fixedwidth: String = row.get(0);
  //               let entry = entry_fixedwidth.trim_right().to_string();
  //               if service.name == "import" {
  //                 entries.push(entry);
  //               } else {
  //                 let entry_service_result = entry_name_regex.replace(&entry, "$1") + "/" + &service.name + ".zip";
  //                 entries.push(entry_service_result);
  //               }
  //             }
  //             entries
  //           }
  //           _ => Vec::new(),
  //         }
  //       }
  //       _ => Vec::new(),
  //     }
  //   }
  //   /// Provides a progress report, grouped by severity, for a given `Corpus` and `Service` pair
  //   pub fn progress_report(&self, c: &Corpus, s: &Service) -> HashMap<String, f64> {
  //     let mut stats_hash: HashMap<String, f64> = HashMap::new();
  //     for status_key in TaskStatus::keys() {
  //       stats_hash.insert(status_key, 0.0);
  //     }
  //     stats_hash.insert("total".to_string(), 0.0);
  //     if let Ok(select_query) = self.connection.prepare("select status,count(*) as status_count from tasks where serviceid=$1 and corpusid=$2 group by status order by status_count desc;") {
  //       if let Ok(rows) = select_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1)]) {
  //         for row in rows.iter() {
  //           let status = TaskStatus::from_raw(row.get(0));
  //           let status_key = status.to_key();
  //           let count: i64 = row.get(1);
  //           {
  //             let status_frequency = stats_hash.entry(status_key).or_insert(0.0);
  //             *status_frequency += count as f64;
  //           }
  //           if status != TaskStatus::Invalid {
  //             // DIScount invalids from the total numbers
  //             let total_frequency = stats_hash.entry("total".to_string()).or_insert(0.0);
  //             *total_frequency += count as f64;
  //           }
  //         }
  //       }
  //     }
  //     Backend::aux_stats_compute_percentages(&mut stats_hash, None);
  //     stats_hash
  //   }

  //   /// Given a complex selector, of a `Corpus`, `Service`, and the optional `severity`, `category` and `what`,
  //   /// Provide a progress report at the chosen granularity
  //   pub fn task_report(&self, c: &Corpus, s: &Service, severity: Option<String>, category: Option<String>, what: Option<String>) -> Vec<HashMap<String, String>> {
  //     match severity {
  //       Some(severity_name) => {
  //         let raw_status = TaskStatus::from_key(&severity_name).raw();
  //         if severity_name == "no_problem" {
  //           match self.connection.prepare("select entry,taskid from tasks where serviceid=$1 and corpusid=$2 and status=$3 limit 100;") {
  //             Ok(select_query) => {
  //               match select_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1), &raw_status]) {
  //                 Ok(entry_rows) => {
  //                   let entry_name_regex = Regex::new(r"^.+/(.+)\..+$").unwrap();
  //                   let mut entries = Vec::new();
  //                   for row in entry_rows.iter() {
  //                     let mut entry_map = HashMap::new();
  //                     let entry_fixedwidth: String = row.get(0);
  //                     let entry_taskid: i64 = row.get(1);
  //                     let entry = entry_fixedwidth.trim_right().to_string();
  //                     let entry_name = entry_name_regex.replace(&entry, "$1");

  //                     entry_map.insert("entry".to_string(), entry);
  //                     entry_map.insert("entry_name".to_string(), entry_name);
  //                     entry_map.insert("entry_taskid".to_string(), entry_taskid.to_string());
  //                     entry_map.insert("details".to_string(), "OK".to_string());
  //                     entries.push(entry_map);
  //                   }
  //                   entries
  //                 }
  //                 _ => Vec::new(),
  //               }
  //             }
  //             _ => Vec::new(),
  //           }
  //         } else {
  //           let total_count_query = self.connection.prepare("select count(*) from tasks WHERE serviceid=$1 and corpusid=$2;").unwrap();
  //           let invalid_count_query = self.connection
  //                                         .prepare("select count(distinct(tasks.taskid)) from tasks,logs WHERE tasks.taskid = logs.taskid and serviceid=$1 and corpusid=$2 and severity='invalid';")
  //                                         .unwrap();
  //           let invalid_tasks: i64 = match invalid_count_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1)]) {
  //             Err(_) => 0, // don't divide by 0
  //             Ok(count) => count.get(0).get(0),
  //           };
  //           // The total tasks are All tasks MINUS Invalid tasks, as we don't want to dilute the service percentage with invalid jobs.
  //           // For now the fastest way to obtain that number is using 2 queries for each and subtracting the numbers in Rust
  //           let total_tasks: i64 = match total_count_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1)]) {
  //             Err(_) => 1, // don't divide by 0
  //             Ok(count) => count.get(0).get(0),
  //           } - invalid_tasks;
  //           match category {
  //             // using ::int4 since the rust postgresql wrapper can't map Numeric into Rust yet, but it is fine with bigint (as i64)
  //             None => {
  //               match self.connection.prepare("select category, count(*) as task_count, sum(total_counts::int4) from (
  //               select logs.category, logs.taskid, count(*) as total_counts from \
  //                                              tasks LEFT OUTER JOIN logs ON (tasks.taskid=logs.taskid) WHERE serviceid=$1 and corpusid=$2 and status=$3 and severity=$4
  //                group by \
  //                                              logs.category, logs.taskid) as tmp GROUP BY category ORDER BY task_count desc;") {
  //                 Ok(select_query) => {
  //                   match select_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1), &raw_status, &severity_name]) {
  //                     Ok(category_rows) => {
  //                       // How many tasks total in this severity?
  //                       let severity_tasks: i64 = match self.connection.prepare("select count(*) from tasks where serviceid=$1 and corpusid=$2 and status=$3;") {
  //                         Ok(total_severity_query) => {
  //                           match total_severity_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1), &raw_status]) {
  //                             Ok(total_severity_rows) => total_severity_rows.get(0).get(0),
  //                             _ => -1,
  //                           }
  //                         }
  //                         _ => -1,
  //                       };
  //                       match self.connection
  //                                 .prepare("select count(*), sum(message_count::int4) from (select tasks.taskid, count(*) as message_count from tasks, logs where tasks.taskid=logs.taskid and \
  //                                           serviceid=$1 and corpusid=$2 and status=$3 and severity=$4 group by tasks.taskid) as tmp;") {
  //                         Ok(total_query) => {
  //                           match total_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1), &raw_status, &severity_name]) {
  //                             Ok(total_rows) => {
  //                               let severity_message_tasks: i64 = total_rows.get(0).get_opt(0).unwrap_or(Ok(0)).unwrap_or(0);
  //                               let severity_messages: i64 = total_rows.get(0).get_opt(1).unwrap_or(Ok(0)).unwrap_or(0);
  //                               let severity_silent_tasks = if severity_message_tasks >= severity_tasks {
  //                                 None
  //                               } else {
  //                                 Some(severity_tasks - severity_message_tasks)
  //                               };
  //                               Backend::aux_task_rows_stats(category_rows,
  //                                                            total_tasks,
  //                                                            severity_tasks,
  //                                                            severity_messages,
  //                                                            severity_silent_tasks)
  //                             }
  //                             _ => Vec::new(),
  //                           }
  //                         }
  //                         _ => Vec::new(),
  //                       }
  //                     }
  //                     _ => Vec::new(),
  //                   }
  //                 }
  //                 _ => Vec::new(),
  //               }
  //             }
  //             Some(category_name) => {
  //               if category_name == "no_messages" {
  //                 match self.connection
  //                           .prepare("select entry,taskid from tasks t where serviceid=$1 and corpusid=$2 and status=$3 and not exists (select null from logs where logs.taskid=t.taskid) limit 100;") {
  //                   Ok(select_query) => {
  //                     match select_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1), &raw_status]) {
  //                       Ok(entry_rows) => {
  //                         let entry_name_regex = Regex::new(r"^.+/(.+)\..+$").unwrap();
  //                         let mut entries = Vec::new();
  //                         for row in entry_rows.iter() {
  //                           let mut entry_map = HashMap::new();
  //                           let entry_fixedwidth: String = row.get(0);
  //                           let entry_taskid: i64 = row.get(1);
  //                           let entry = entry_fixedwidth.trim_right().to_string();
  //                           let entry_name = entry_name_regex.replace(&entry, "$1");

  //                           entry_map.insert("entry".to_string(), entry);
  //                           entry_map.insert("entry_name".to_string(), entry_name);
  //                           entry_map.insert("entry_taskid".to_string(), entry_taskid.to_string());
  //                           entry_map.insert("details".to_string(), "OK".to_string());
  //                           entries.push(entry_map);
  //                         }
  //                         entries
  //                       }
  //                       _ => Vec::new(),
  //                     }
  //                   }
  //                   _ => Vec::new(),
  //                 }
  //               } else {
  //                 match what {
  //                   // using ::int4 since the rust postgresql wrapper can't map Numeric into Rust yet, but it is fine with bigint (as i64)
  //                   None => {
  //                     match self.connection.prepare("select what, count(*) as task_count, sum(total_counts::int4) from (
  //                 select logs.what, logs.taskid, count(*) as total_counts from \
  //                                                    tasks LEFT OUTER JOIN logs ON (tasks.taskid=logs.taskid)
  //                 WHERE serviceid=$1 and corpusid=$2 and status=$3 and severity=$4 and \
  //                                                    category=$5
  //                 GROUP BY logs.what, logs.taskid) as tmp GROUP BY what ORDER BY task_count desc;") {
  //                       Ok(select_query) => {
  //                         match select_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1), &raw_status, &severity_name, &category_name]) {
  //                           Ok(what_rows) => {
  //                             // How many tasks and messages total in this category?
  //                             match self.connection
  //                                       .prepare("select count(*), sum(message_count::int4) from (select tasks.taskid, count(*) as message_count from tasks, logs where tasks.taskid=logs.taskid and \
  //                                                 serviceid=$1 and corpusid=$2 and status=$3 and severity=$4 and category=$5 group by tasks.taskid) as tmp;") {
  //                               Ok(total_query) => {
  //                                 match total_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1), &raw_status, &severity_name, &category_name]) {
  //                                   Ok(total_rows) => {
  //                                     let category_tasks: i64 = total_rows.get(0).get(0);
  //                                     let category_messages: i64 = total_rows.get(0).get(1);
  //                                     Backend::aux_task_rows_stats(what_rows,
  //                                                                  total_tasks,
  //                                                                  category_tasks,
  //                                                                  category_messages,
  //                                                                  None)
  //                                   }
  //                                   _ => Vec::new(),
  //                                 }
  //                               }
  //                               _ => Vec::new(),
  //                             }
  //                           }
  //                           _ => Vec::new(),
  //                         }
  //                       }
  //                       _ => Vec::new(),
  //                     }
  //                   }
  //                   Some(what_name) => {
  //                     match self.connection.prepare("select tasks.taskid, tasks.entry, logs.details from tasks, logs where tasks.taskid=logs.taskid and serviceid=$1 and corpusid=$2 and status=$3 \
  //                                                    and severity=$4 and category=$5 and what=$6 limit 100;") {
  //                       Ok(select_query) => {
  //                         match select_query.query(&[&s.id.unwrap_or(-1), &c.id.unwrap_or(-1), &raw_status, &severity_name, &category_name, &what_name]) {
  //                           Ok(entry_rows) => {
  //                             let entry_name_regex = Regex::new(r"^.+/(.+)\..+$").unwrap();
  //                             let mut entries = Vec::new();
  //                             for row in entry_rows.iter() {
  //                               let mut entry_map = HashMap::new();
  //                               let entry_taskid: i64 = row.get(0);
  //                               let entry_fixedwidth: String = row.get(1);
  //                               let details: String = row.get(2);
  //                               let entry = entry_fixedwidth.trim_right().to_string();
  //                               let entry_name = entry_name_regex.replace(&entry, "$1");
  //                               // TODO: Also use url-escape
  //                               entry_map.insert("entry".to_string(), entry);
  //                               entry_map.insert("entry_name".to_string(), entry_name);
  //                               entry_map.insert("entry_taskid".to_string(), entry_taskid.to_string());
  //                               entry_map.insert("details".to_string(), details);
  //                               entries.push(entry_map);
  //                             }
  //                             entries
  //                           }
  //                           _ => Vec::new(),
  //                         }
  //                       }
  //                       _ => Vec::new(),
  //                     }
  //                   }
  //                 }
  //               }
  //             }
  //           }
  //         }
  //       }
  //       None => Vec::new(),
  //     }
  //   }
  //   fn aux_stats_compute_percentages(stats_hash: &mut HashMap<String, f64>, total_given: Option<f64>) {
  //     // Compute percentages, now that we have a total
  //     let total: f64 = 1.0_f64.max(match total_given {
  //       None => {
  //         let total_entry = stats_hash.get_mut("total").unwrap();
  //         *total_entry
  //       }
  //       Some(total_num) => total_num,
  //     });
  //     let stats_keys = stats_hash.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
  //     for stats_key in stats_keys {
  //       {
  //         let key_percent_value: f64 = 100.0 * (*stats_hash.get_mut(&stats_key).unwrap() as f64 / total as f64);
  //         let key_percent_rounded: f64 = (key_percent_value * 100.0).round() as f64 / 100.0;
  //         let key_percent_name = stats_key + "_percent";
  //         stats_hash.insert(key_percent_name, key_percent_rounded);
  //       }
  //     }
  //   }
  //   fn aux_task_rows_stats(rows: Rows, total_tasks: i64, these_tasks: i64, these_messages: i64, these_silent: Option<i64>) -> Vec<HashMap<String, String>> {
  //     let mut report = Vec::new();

  //     for row in rows.iter() {
  //       let stat_type_fixedwidth: String = row.get(0);
  //       let stat_type: String = stat_type_fixedwidth.trim_right().to_string();
  //       let stat_tasks: i64 = row.get(1);
  //       let stat_messages: i64 = row.get(2);
  //       let mut stats_hash: HashMap<String, String> = HashMap::new();
  //       stats_hash.insert("name".to_string(), stat_type);
  //       stats_hash.insert("tasks".to_string(), stat_tasks.to_string());
  //       stats_hash.insert("messages".to_string(), stat_messages.to_string());

  //       let tasks_percent_value: f64 = 100.0 * (stat_tasks as f64 / total_tasks as f64);
  //       let tasks_percent_rounded: f64 = (tasks_percent_value * 100.0).round() as f64 / 100.0;
  //       stats_hash.insert("tasks_percent".to_string(),
  //                         tasks_percent_rounded.to_string());
  //       let messages_percent_value: f64 = 100.0 * (stat_messages as f64 / these_messages as f64);
  //       let messages_percent_rounded: f64 = (messages_percent_value * 100.0).round() as f64 / 100.0;
  //       stats_hash.insert("messages_percent".to_string(),
  //                         messages_percent_rounded.to_string());

  //       report.push(stats_hash);
  //     }
  //     let these_tasks_percent_value: f64 = 100.0 * (these_tasks as f64 / total_tasks as f64);
  //     let these_tasks_percent_rounded: f64 = (these_tasks_percent_value * 100.0).round() as f64 / 100.0;
  //     // Append the total to the end of the report:
  //     let mut total_hash = HashMap::new();
  //     total_hash.insert("name".to_string(), "total".to_string());
  //     match these_silent {
  //       None => {}
  //       Some(silent_count) => {
  //         let mut no_messages_hash: HashMap<String, String> = HashMap::new();
  //         no_messages_hash.insert("name".to_string(), "no_messages".to_string());
  //         no_messages_hash.insert("tasks".to_string(), silent_count.to_string());
  //         let silent_tasks_percent_value: f64 = 100.0 * (silent_count as f64 / total_tasks as f64);
  //         let silent_tasks_percent_rounded: f64 = (silent_tasks_percent_value * 100.0).round() as f64 / 100.0;
  //         no_messages_hash.insert("tasks_percent".to_string(),
  //                                 silent_tasks_percent_rounded.to_string());
  //         no_messages_hash.insert("messages".to_string(), "0".to_string());
  //         no_messages_hash.insert("messages_percent".to_string(), "0".to_string());
  //         report.push(no_messages_hash);
  //       }
  //     };
  //     total_hash.insert("tasks".to_string(), these_tasks.to_string());
  //     total_hash.insert("tasks_percent".to_string(),
  //                       these_tasks_percent_rounded.to_string());
  //     total_hash.insert("messages".to_string(), these_messages.to_string());
  //     total_hash.insert("messages_percent".to_string(), "100".to_string());
  //     report.push(total_hash);


  //     report
  //   }
}
