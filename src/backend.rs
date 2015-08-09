extern crate postgres;
extern crate rustc_serialize;

use postgres::{Connection, SslMode};
use postgres::error::Error;
use rustc_serialize::json::{Json, ToJson};
use std::collections::BTreeMap;
use std::clone::Clone;
// Some useful data structures:

// Tasks
use std::fmt;
pub struct Task {
  pub id : Option<i64>,
  pub entry: String,
  pub serviceid: i32,
  pub corpusid: i32,
  pub status: i32
}
impl fmt::Display for Task {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // The `f` value implements the `Write` trait, which is what the
        // write! macro is expecting. Note that this formatting ignores the
        // various flags provided to format strings.
        write!(f, "(entry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n", self.entry, self.serviceid, self.corpusid, self.status)
    }
}
impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // The `f` value implements the `Write` trait, which is what the
        // write! macro is expecting. Note that this formatting ignores the
        // various flags provided to format strings.
        write!(f, "(entry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n", self.entry, self.serviceid, self.corpusid, self.status)
    }
}

// Corpora
#[derive(RustcDecodable, RustcEncodable)]
pub struct Corpus {
  pub id : Option<i32>,
  pub name : String,
  pub path : String,
  pub complex : bool
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
impl Clone for Corpus {
  fn clone(&self) -> Self {
    Corpus {
      id : self.id.clone(),
      name : self.name.clone(),
      path : self.path.clone(),
      complex : self.complex.clone()
    }
  }
}

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
      path varchar(200),
      name varchar(200),
      complex boolean
    );", &[]).unwrap();
    trans.execute("create index corpusnameidx on corpora(name);", &[]).unwrap();
    // Services
    trans.execute("DROP TABLE IF EXISTS services;", &[]).unwrap();
    trans.execute("CREATE TABLE services (
      serviceid BIGSERIAL PRIMARY KEY,
      name varchar(200),
      version varchar(50) NOT NULL,
      iid varchar(250) NOT NULL,
      url varchar(2000),
      inputformat varchar(20) NOT NULL,
      outputformat varchar(20) NOT NULL,
      xpath varchar(2000),
      resource varchar(50),
      inputconverter varchar(200),
      type integer NOT NULL,
      entrysetup integer NOT NULL,
      UNIQUE(iid,name)
    );", &[]).unwrap();
    trans.execute("create index servicenameidx on services(name);", &[]).unwrap();
    trans.execute("create index serviceiididx on services(iid);", &[]).unwrap();
    trans.execute("INSERT INTO services (name,version,iid,type,inputformat,outputformat,entrysetup)
               values('import',0.1,'import_v0_1',2,'tex','tex',1);", &[]).unwrap();
    trans.execute("INSERT INTO services (name,version,iid,type,inputformat,outputformat,entrysetup)
           values('init',0.1,'init_v0_1',2,'tex','tex',1);", &[]).unwrap();

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

  pub fn mark_done(&self, tasks: &Vec<Task>) -> Result<(),Error> {
    // self.connection.execute("UPDATE tasks (name, data) VALUES ($1, $2)",
    //   &[&me.name, &me.data]).unwrap();
    let trans = try!(self.connection.transaction());
    trans.set_commit();
    try!(trans.finish());
    Ok(())
  }

  pub fn sync_corpus(&self, c: &Corpus) -> Result<Corpus, Error> {
    return match c.id {
      Some(id) => {
        let stmt = try!(self.connection.prepare("SELECT corpusid,name,path,complex FROM corpora WHERE corpusid = $1"));
        let rows = stmt.query(&[&id]).unwrap();
        if rows.len() > 0 {
          let row = rows.get(0);
          return Ok(Corpus {
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
    };
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
    }
    // Add Corpus to the DB:
    try!(self.connection.execute("INSERT INTO corpora (name, path, complex) values($1, $2, $3)", &[&c_checked.name, &c_checked.path, &c_checked.complex]));
    let c_final = try!(self.sync_corpus(&c));
    Ok(c_final)
  }
}