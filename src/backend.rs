// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate postgres;
extern crate rustc_serialize;
extern crate rand;

use postgres::{Connection, SslMode};
use postgres::error::Error;
use std::clone::Clone;
use std::collections::HashMap;
use data::{CortexORM, Corpus, Service, Task, TaskReport, TaskStatus};

use rand::{thread_rng, Rng};

// Only initialize auxiliary resources once and keep them in a Backend struct
pub struct Backend {
  pub connection : Connection
}

pub static DEFAULT_DB_ADDRESS : &'static str = "postgres://cortex:cortex@localhost/cortex";
pub static TEST_DB_ADDRESS : &'static str = "postgres://cortex_tester:cortex_tester@localhost/cortex_tester";
impl Default for Backend {
  fn default() -> Backend {
    Backend {
      connection: Connection::connect(DEFAULT_DB_ADDRESS.clone(), &SslMode::None).unwrap()
    }
  }
}

impl Backend {
  // Globals
  pub fn from_address(address : &str) -> Backend {
   Backend {
      connection: Connection::connect(address, &SslMode::None).unwrap()
    } 
  }
  pub fn testdb() -> Backend {
   Backend {
      connection: Connection::connect(TEST_DB_ADDRESS.clone(), &SslMode::None).unwrap()
    }
  }

  // Instance methods
  pub fn needs_init(&self) -> bool {
    match self.connection.prepare("SELECT * FROM services where name='init'") {
      Ok(init_check_query) => {
        match init_check_query.query(&[]) {
          Ok(rows) => {
            rows.len() == 0
          },
          _ => true
        }
      },
      _ => true
    }
  }
  pub fn setup_task_tables(&self) -> postgres::Result<()> {
    let trans = try!(self.connection.transaction());
    // Tasks
    trans.execute("DROP TABLE IF EXISTS tasks;", &[]).unwrap();
    trans.execute("CREATE TABLE tasks (
      taskid BIGSERIAL PRIMARY KEY,
      serviceid INTEGER NOT NULL,
      corpusid INTEGER NOT NULL,
      entry char(200) NOT NULL,
      status INTEGER NOT NULL
    );", &[]).unwrap();
    trans.execute("create index entryidx on tasks(entry);", &[]).unwrap();
    trans.execute("create index serviceidx on tasks(serviceid);", &[]).unwrap();
    trans.execute("create index scst_index on tasks(status,serviceid,corpusid,taskid);", &[]).unwrap();
    // Corpora
    trans.execute("DROP TABLE IF EXISTS corpora;", &[]).unwrap();
    trans.execute("CREATE TABLE corpora (
      corpusid SERIAL PRIMARY KEY,
      path varchar(200) NOT NULL,
      name varchar(200) NOT NULL,
      complex boolean NOT NULL
    );", &[]).unwrap();
    trans.execute("create index corpusnameidx on corpora(name);", &[]).unwrap();
    // Services
    trans.execute("DROP TABLE IF EXISTS services;", &[]).unwrap();
    trans.execute("CREATE TABLE services (
      serviceid SERIAL PRIMARY KEY,
      name varchar(200) NOT NULL,
      version real NOT NULL,
      inputformat varchar(20) NOT NULL,
      outputformat varchar(20) NOT NULL,
      inputconverter varchar(200),
      complex boolean NOT NULL,
      UNIQUE(name,version)
    );", &[]).unwrap();
    trans.execute("create index servicenameidx on services(name);", &[]).unwrap();
    // trans.execute("create index serviceiididx on services(iid);", &[]).unwrap();
    trans.execute("INSERT INTO services (name, version, inputformat,outputformat,complex)
           values('init',0.1, 'tex','tex', true);", &[]).unwrap();
    trans.execute("INSERT INTO services (name, version, inputformat,outputformat,complex)
           values('import',0.1, 'tex','tex', true);", &[]).unwrap();

    // Dependency Tables
    trans.execute("DROP TABLE IF EXISTS dependencies;", &[]).unwrap();
    trans.execute("CREATE TABLE dependencies (
      master INTEGER NOT NULL,
      foundation INTEGER NOT NULL,
      PRIMARY KEY (master, foundation)
    );", &[]).unwrap();
    trans.execute("create index masteridx on dependencies(master);", &[]).unwrap();
    trans.execute("create index foundationidx on dependencies(foundation);", &[]).unwrap();

    // Log Tables
    trans.execute("DROP TABLE if EXISTS logs", &[]).unwrap();
    trans.execute("CREATE TABLE logs (
      messageid BIGSERIAL PRIMARY KEY,
      taskid BIGINT NOT NULL,
      severity char(50),
      category char(50),
      what char(50),
      details varchar(2000)
    );", &[]).unwrap();
    trans.execute("DROP TABLE if EXISTS logdetails", &[]).unwrap();
    // trans.execute("CREATE TABLE logdetails (
    //   messageid BIGSERIAL PRIMARY KEY,
    //   details varchar(2000)
    // );", &[]).unwrap();
    trans.execute("create index logtaskcatwhat on logs(taskid,category,what);", &[]).unwrap();

    trans.set_commit();
    try!(trans.finish());
    Ok(())
  }

  pub fn mark_imported(&self, tasks: &Vec<Task>) -> Result<(),Error> {
    let trans = try!(self.connection.transaction());
    for task in tasks {
      trans.execute("INSERT INTO tasks (entry,serviceid,corpusid,status) VALUES ($1,$2,$3,$4)",
        &[&task.entry, &task.serviceid, &task.corpusid, &task.status]).unwrap();
    }
    trans.set_commit();
    try!(trans.finish());
    Ok(())
  }

  pub fn mark_done(&self, reports: &Vec<TaskReport>) -> Result<(),Error> {
    let trans = try!(self.connection.transaction());
    let insert_log_message = trans.prepare("INSERT INTO logs (taskid, severity, category, what, details) values($1,$2,$3,$4,$5)").unwrap();
    // let insert_log_message_details = trans.prepare("INSERT INTO logdetails (messageid, details) values(?,?)").unwrap();
    for report in reports.iter() {
      let taskid = report.task.id.unwrap();
      trans.execute("UPDATE tasks SET status=$1 WHERE taskid=$2",
        &[&report.status.raw(), &taskid]).unwrap();
      for message in &report.messages {
        if (message.severity == "info") || (message.severity == "status") {
          continue; // Skip info and status information, keep the DB small
        } else {
          // Warnings, Errors and Fatals will get added:
          insert_log_message.query(&[&taskid, 
            &message.severity, &message.category, &message.what, &message.details]).unwrap();
        }
      }
      // TODO: Update dependencies
    }
    trans.set_commit();
    try!(trans.finish());
    Ok(())
  }

  pub fn sync<D: CortexORM + Clone>(&self, d: &D) -> Result<D, Error> {
    let synced = match d.get_id() {
      Some(_) => {
        try!(d.select_by_id(&self.connection))
      },
      None => {
        try!(d.select_by_key(&self.connection))
      }
    };
    match synced {
      Some(synced_d) => Ok(synced_d),
      None => Ok(d.clone())
    }
  }

  pub fn delete<D: CortexORM + Clone>(&self, d: &D) -> Result<(), Error> {
    let d_checked = try!(self.sync(d));
    match d_checked.get_id() {
      Some(_) => d.delete(&self.connection),
      None => Ok(()) // No ID means we don't really know what to delete.
    }
  }
  pub fn add<D: CortexORM + Clone>(&self, d: D) -> Result<D, Error> {
    let d_checked = try!(self.sync(&d));
    match d_checked.get_id() {
      Some(_) => {
        // If this data item existed - delete any remnants of it
        try!(self.delete(&d_checked));
      },
      None => {} // New, we can add it safely
    };
    // Add data item to the DB:
    try!(d.insert(&self.connection));
    let d_final = try!(self.sync(&d));
    Ok(d_final)
  }

  pub fn fetch_tasks(&self, service: &Service, limit : usize) -> Result<Vec<Task>, Error> {
    match service.id { 
      Some(_) => {}
      None => {return Ok(Vec::new())}
    };
    let mut rng = thread_rng();
    let mark: u16 = rng.gen();

    // TODO: Concurrent use needs to add "and pg_try_advisory_xact_lock(taskid)" in the proper fashion
    //       But we need to be careful that the LIMIT takes place before the lock, which is why I removed it for now.
    let stmt = try!(self.connection.prepare(
      "UPDATE tasks t SET status = $1 FROM (
          SELECT * FROM tasks WHERE serviceid = $2 and status = $3
          LIMIT $4
          FOR UPDATE
        ) subt
        WHERE t.taskid = subt.taskid
        RETURNING t.taskid,t.entry,t.serviceid,t.corpusid,t.status;"));
    let rows = try!(stmt.query(&[&(mark as i32), &service.id.unwrap(), &TaskStatus::TODO.raw(), &(limit as i64)]));
    Ok(rows.iter().map(|row| Task::from_row(row)).collect::<Vec<_>>())
  }

  pub fn clear_limbo_tasks(&self) -> Result<(), Error> {
    try!(self.connection.execute("UPDATE tasks SET status=$1 WHERE status > $2", &[&TaskStatus::TODO.raw(), &TaskStatus::NoProblem.raw(),]));
    Ok(())
  }

  pub fn register_service(&self, service: Service, corpus_path: String) -> Result<(),Error> {
    let corpus_placeholder = Corpus {
      id : None,
      path : corpus_path.clone(),
      name : corpus_path,
      complex : true
    };
    let corpus = self.sync(&corpus_placeholder).unwrap();
    let corpusid = corpus.id.unwrap();
    let serviceid = service.id.unwrap();
    let todo_raw = TaskStatus::TODO.raw();

    try!(self.connection.execute("DELETE from tasks where serviceid=$1 AND corpusid=$2", &[&serviceid, &corpusid]));
    let task_entries_query = try!(self.connection.prepare("SELECT entry from tasks where serviceid=2 AND corpusid=$1"));
    let task_entries = try!(task_entries_query.query(&[&corpus.id.unwrap()]));
    let trans = try!(self.connection.transaction());   
    for task_entry in task_entries.iter() {
      let entry : String = task_entry.get(0);
      trans.execute("INSERT INTO tasks (entry,serviceid,corpusid, status) VALUES ($1,$2,$3,$4)",
        &[&entry, &serviceid, &corpusid, &todo_raw]).unwrap();
    }
    trans.set_commit();
    try!(trans.finish());
    Ok(())
 }

  pub fn corpora(&self) -> Vec<Corpus> {
    let mut corpora = Vec::new();
    match self.connection.prepare("SELECT corpusid,name,path,complex FROM corpora order by name") {
      Ok(select_query) => {
        match select_query.query(&[]) {
          Ok(rows) => {
            for row in rows.iter() {
              corpora.push(Corpus::from_row(row));
            }
          },
          _ => {}
        }
      }
      _ => {}
    }
    return corpora;
  }

  pub fn progress_report<'report>(&self, c : &Corpus, s : &Service, 
    severity: Option<&str>, category: Option<&str>, what: Option<&str>) -> HashMap<String, f64> {

    let mut stats_hash : HashMap<String, f64> = HashMap::new();
    for status_key in TaskStatus::keys().into_iter() {
      stats_hash.insert(status_key,0.0);
    }
    stats_hash.insert("total".to_string(),0.0);
    match self.connection.prepare("select status,count(*) from tasks where serviceid=$1 and corpusid=$2 group by status") {
      Ok(select_query) => {
        match select_query.query(&[&s.id.unwrap(), &c.id.unwrap()]) {
          Ok(rows) => {
            for row in rows.iter() {
              let status_code = TaskStatus::from_raw(row.get(0)).to_key();
              let count : i64 = row.get(1);
              {
                let status_frequency = stats_hash.entry(status_code).or_insert(0.0);
                *status_frequency += count as f64;
              }
              let total_frequency = stats_hash.entry("total".to_string()).or_insert(0.0);
              *total_frequency += count as f64;
            }
          },
          _ => {}
        }
      }
      _ => {}
    }
    Backend::aux_stats_compute_percentages(&mut stats_hash);
    stats_hash
  }
  fn aux_stats_compute_percentages(stats_hash : &mut HashMap<String, f64>) {
     //Compute percentages, now that we have a total
    let mut total : f64;
    {
      let total_entry = stats_hash.get_mut("total").unwrap();
      total = (*total_entry).clone();
    }
    if total <= 0.0 {total = 1.0;}
    let stats_keys = stats_hash.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
    for stats_key in stats_keys {
      {
        let key_percent_value : f64 = 100.0 * (*stats_hash.get_mut(&stats_key).unwrap() / total);
        let key_percent_rounded : f64 = (key_percent_value * 100.0).round() / 100.0;
        let key_percent_name = stats_key + "_percent";
        stats_hash.insert(key_percent_name, key_percent_rounded);
      }
    }
  }
}