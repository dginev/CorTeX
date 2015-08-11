// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate postgres;
extern crate rustc_serialize;

use postgres::{Connection, SslMode};
use postgres::error::Error;
use std::clone::Clone;
use data::*;

// Only initialize auxiliary resources once and keep them in a Backend struct
pub struct Backend {
  pub connection : Connection
}

impl Default for Backend {
  fn default() -> Backend {
    Backend {
      connection: Connection::connect("postgres://cortex:cortex@localhost/cortex", &SslMode::None).unwrap()
    }
  }
}

impl Backend {
  // Globals
  pub fn testdb() -> Backend {
   Backend {
      connection: Connection::connect("postgres://cortex_tester:cortex_tester@localhost/cortex_tester", &SslMode::None).unwrap()
    }
  }
  // Instance methods
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
               values('import',0.1, 'tex','tex', true);", &[]).unwrap();
    trans.execute("INSERT INTO services (name, version, inputformat,outputformat,complex)
           values('init',0.1, 'tex','tex', true);", &[]).unwrap();

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
      taskid INTEGER NOT NULL,
      category char(50),
      what char(50)
    );", &[]).unwrap();
    trans.execute("DROP TABLE if EXISTS logdetails", &[]).unwrap();
    trans.execute("CREATE TABLE logdetails (
      messageid BIGSERIAL PRIMARY KEY,
      details varchar(2000)
    );", &[]).unwrap();
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
    for report in reports {
      trans.execute("UPDATE tasks SET status=$1 WHERE taskid=$2",
        &[&report.task.id, &report.status.raw()]).unwrap();
      for message in &report.messages {
        println!("{:?}", message.text);
      }
    }
    trans.set_commit();
    try!(trans.finish());
    Ok(())
  }

  pub fn sync_corpus(&self, c: &Corpus) -> Result<Corpus, Error> {
    match c.id {
      Some(id) => {
        let stmt = try!(self.connection.prepare("SELECT corpusid,name,path,complex FROM corpora WHERE corpusid = $1"));
        let rows = stmt.query(&[&id]).unwrap();
        if rows.len() > 0 {
          let row = rows.get(0);
          Ok(Corpus {
            id : Some(row.get(0)),
            name : row.get(1),
            path : row.get(2),
            complex : row.get(3)
          })
        } else {
          Ok(c.clone())
        }
      },
      None => {
        let stmt = try!(self.connection.prepare("SELECT corpusid,name,path,complex FROM corpora WHERE name = $1"));
        let rows = stmt.query(&[&c.name]).unwrap();
        if rows.len() > 0 {
          let row = rows.get(0);
          Ok(Corpus {
            id : Some(row.get(0)),
            name : row.get(1),
            path : row.get(2),
            complex : row.get(3)
          })
        } else {
          Ok(c.clone())
        }
      }
    }
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
  pub fn add<D: CortexORM + Clone>(&self, d: &D) -> Result<D, Error> {
    let d_checked = try!(self.sync(d));
    match d_checked.get_id() {
      Some(_) => {
        // If this data item existed - delete any remnants of it
        try!(self.delete(&d_checked));
      },
      None => {} // New, we can add it safely
    };
    // Add data item to the DB:
    try!(d.insert(&self.connection));
    let d_final = try!(self.sync(d));
    Ok(d_final)
  }

  pub fn delete_corpus(&self, c: &Corpus) -> Result<(),Error> {
    let c_checked = try!(self.sync_corpus(&c));
    match c_checked.id {
      Some(id) => {
        try!(self.connection.execute("DELETE FROM tasks WHERE corpusid = $1", &[&id])); 
        try!(self.connection.execute("DELETE FROM corpora WHERE corpusid = $1", &[&id])); 
      },
      None => {}
    }
    Ok(())
  }

  pub fn add_corpus(&self, c: Corpus) -> Result<Corpus, Error> {
    let c_checked = try!(self.sync_corpus(&c));
    match c_checked.id {
      Some(_) => {
        // If this corpus existed - delete any remnants of it
        try!(self.delete_corpus(&c_checked));
      },
      None => {} // New, we can add it safely
    };
    // Add Corpus to the DB:
    try!(self.connection.execute("INSERT INTO corpora (name, path, complex) values($1, $2, $3)", &[&c_checked.name, &c_checked.path, &c_checked.complex]));
    let c_final = try!(self.sync_corpus(&c));
    Ok(c_final)
  }

  pub fn add_service(&self, s: Service) -> Result<Service, Error> {
    let s_checked = try!(self.sync(&s));
    match s_checked.id {
      Some(_) => {
        // If this service existed - delete any remnants of it
        // try!(self.delete_service(&s_checked));
      },
      None => {} // New, we can add it safely
    };
    // Add Service to the DB:
    try!(self.connection.execute("INSERT INTO services (name, version, inputformat, outputformat, inputconverter, complex) values($1, $2, $3, $4, $5, $6)",
         &[&s_checked.name, &s_checked.version, &s_checked.inputformat, &s_checked.outputformat, &s_checked.inputconverter, &s_checked.complex]));
    let s_final = try!(self.sync(&s));
    Ok(s_final)
  }

  pub fn add_task(&self, t: Task) -> Result<Task, Error> {
    let t_checked = try!(self.sync(&t));
    match t_checked.id {
      Some(_) => {
        // If this service existed - delete any remnants of it
        // try!(self.delete_service(&t_checked));
      },
      None => {} // New, we can add it safely
    }
    // Add Service to the DB:
    try!(self.connection.execute("INSERT INTO tasks (entry, serviceid, corpusid, status) values($1, $2, $3, $4)",
         &[&t_checked.entry, &t_checked.serviceid, &t_checked.corpusid, &t_checked.status]));
    let t_final = try!(self.sync(&t));
    Ok(t_final)
  }
}