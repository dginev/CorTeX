extern crate postgres;
extern crate rustc_serialize;

use postgres::{Connection, SslMode};
use rustc_serialize::json::{Json, ToJson};
use std::collections::BTreeMap;
// Some useful data structures:

// Tasks
use std::fmt;
use std::f64;

pub struct Task {
  pub entry: String,
  pub serviceid: usize,
  pub corpusid: usize,
  pub status: i64
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
  pub id : usize,
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
      corpusid BIGSERIAL PRIMARY KEY,
      path varchar(200),
      name varchar(200)
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

  pub fn mark_imported(&self, tasks: Vec<Task>) {
        
  }

  pub fn mark_done(&self, tasks: Vec<Task>) {
    // self.connection.execute("UPDATE tasks (name, data) VALUES ($1, $2)",
    //   &[&me.name, &me.data]).unwrap();
  }

  pub fn add_corpus(&self, path: String, complex: bool) -> Corpus {
    Corpus {
      id: 0,
      path : path.clone(),
      name : path,
      complex : complex
    }
  }
}