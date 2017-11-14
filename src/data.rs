// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Data structures and traits for each framework component in the Task store

use rustc_serialize::json::{Json, ToJson};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::Path;
use std::str;

use postgres::Connection;
use postgres::rows::Row;
use postgres::error::Error;

use Archive::*;

/// A minimalistic ORM trait for `CorTeX` data items
pub trait CortexORM {
  /// Select from Task store via the primary id
  fn select_by_id<'a>(&'a self, connection: &'a Connection) -> Result<Option<Self>, Error>
  where
    Self: Sized;
  /// Select from Task store via a struct-specific uniquely identifying key
  fn select_by_key<'a>(&'a self, connection: &'a Connection) -> Result<Option<Self>, Error>
  where
    Self: Sized;
  /// Inser the row identified by this struct into the Task store (overwrite if present)
  fn insert(&self, connection: &Connection) -> Result<(), Error>;
  /// Delete the row identified by this struct from the Task store
  fn delete(&self, connection: &Connection) -> Result<(), Error>;
  /// Construct a struct from a given Task store row
  fn from_row(row: Row) -> Self;
  /// Obtain the id of the struct, if any
  fn get_id(&self) -> Option<i32>;
}


// Tasks
use std::fmt;
use super::schema::tasks;

#[derive(Insertable, Clone)]
#[table_name = "tasks"]
/// Struct representation of a task store row
pub struct Task {
  /// optional id (None for mock / yet-to-be-inserted rows)
  pub id: Option<i64>,
  /// id of the service owning this task
  pub serviceid: i32,
  /// id of the corpus hosting this task
  pub corpusid: i32,
  /// current processing status of this task
  pub status: i32,
  /// entry path on the file system
  pub entry: String,
}
impl fmt::Display for Task {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    // The `f` value implements the `Write` trait, which is what the
    // write! macro is expecting. Note that this formatting ignores the
    // various flags provided to format strings.
    let taskid = match self.id {
      Some(taskid) => taskid.to_string(),
      None => "None".to_string(),
    };
    write!(
      f,
      "(taskid: {}, entry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n",
      taskid,
      self.entry,
      self.serviceid,
      self.corpusid,
      self.status
    )
  }
}
impl fmt::Debug for Task {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    // The `f` value implements the `Write` trait, which is what the
    // write! macro is expecting. Note that this formatting ignores the
    // various flags provided to format strings.
    let taskid = match self.id {
      Some(taskid) => taskid.to_string(),
      None => "None".to_string(),
    };
    write!(
      f,
      "(taskid: {},\n\tentry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n",
      taskid,
      self.entry,
      self.serviceid,
      self.corpusid,
      self.status
    )
  }
}
impl CortexORM for Task {
  fn get_id(&self) -> Option<i32> {
    // TODO: Best way to deal with this?
    match self.id {
      Some(value) => Some(value as i32),
      None => None,
    }
  }
  fn select_by_id<'a>(&'a self, connection: &'a Connection) -> Result<Option<Task>, Error> {
    let stmt = try!(connection.prepare(
      "SELECT taskid,entry,serviceid,corpusid,status FROM tasks WHERE taskid = $1",
    ));
    let rows = try!(stmt.query(&[&self.id]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Task::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn select_by_key<'a>(&'a self, connection: &'a Connection) -> Result<Option<Task>, Error> {
    let stmt = try!(connection.prepare("SELECT taskid,entry,serviceid,corpusid,status FROM tasks WHERE entry = $1 and serviceid = $2 and corpusid = $3"));
    let rows = try!(stmt.query(&[&self.entry, &self.serviceid, &self.corpusid]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Task::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn insert(&self, connection: &Connection) -> Result<(), Error> {
    try!(connection.execute("INSERT INTO tasks (serviceid, corpusid, status, entry) values($1, $2, $3, $4) ON CONFLICT(entry, serviceid, corpusid) DO UPDATE SET status=excluded.status;",
                            &[&self.serviceid, &self.corpusid, &self.status, &self.entry]));
    Ok(())
  }
  fn delete(&self, connection: &Connection) -> Result<(), Error> {
    try!(connection.execute(
      "DELETE FROM tasks WHERE taskid = $1",
      &[&self.id],
    ));
    Ok(())
  }
  fn from_row(row: Row) -> Self {
    let fix_width_entry: String = row.get(1);
    Task {
      id: Some(row.get(0)),
      entry: fix_width_entry.trim_right().to_string(),
      serviceid: row.get(2),
      corpusid: row.get(3),
      status: row.get(4),
    }
  }
}

impl Task {
  /// Generates a `TaskReport`, given the path to a result archive from a `CorTeX` processing job
  /// Expects a "cortex.log" file in the archive, following the LaTeXML messaging conventions
  pub fn generate_report(&self, result: &Path) -> TaskReport {
    // println!("Preparing report for {:?}, result at {:?}",self.entry, result);
    let mut messages = Vec::new();
    let mut status = TaskStatus::Fatal; // Fatal by default

    {
      // -- Archive::Reader, trying to localize (to .drop asap)
      // Let's open the archive file and find the cortex.log file:
      let log_name = "cortex.log";
      match Reader::new()
        .unwrap()
        .support_filter_all()
        .support_format_all()
        .open_filename(result.to_str().unwrap(), 10240) {

        Err(e) => {
          println!("Error TODO: Couldn't open archive_reader: {:?}", e);
        }
        Ok(archive_reader) => {
          while let Ok(e) = archive_reader.next_header() {
            let current_name = e.pathname();
            if current_name != log_name {
              continue;
            } else {
              // In a "raw" read, we don't know the data size in advance. So we bite the bullet and
              // read the usually manageable log file in memory
              let mut raw_log_data = Vec::new();
              while let Ok(chunk) = archive_reader.read_data(10240) {
                raw_log_data.extend(chunk.into_iter());
              }
              let log_string: String = match str::from_utf8(&raw_log_data) {
                Ok(some_utf_string) => some_utf_string.to_string(),
                Err(e) => {
                  "Fatal:cortex:unicode_parse_error ".to_string() + &e.to_string() +
                    "\nStatus:conversion:3"
                }
              };
              messages = self.parse_log(log_string);
              // Look for the special status message - Fatal otherwise!
              for m in &messages {
                // Invalids are a bit of a workaround for now, they're fatal messages in latexml, but we want them separated out in cortex
                if m.severity == "invalid" {
                  status = TaskStatus::Invalid;
                  break;
                } else if (m.severity == "status") && (m.category == "conversion") &&
                         !(m.what.is_empty())
                {
                  // Adapt status to the CorTeX scheme: cortex_status = -(latexml_status+1)
                  let latexml_scheme_status = match m.what.parse::<i32>() {
                    Ok(num) => num,
                    Err(e) => {
                      println!(
                        "Error TODO: Failed to parse conversion status {:?}: {:?}",
                        m.what,
                        e
                      );
                      TaskStatus::Fatal.raw()
                    }
                  };
                  let cortex_scheme_status = -(latexml_scheme_status + 1);
                  status = TaskStatus::from_raw(cortex_scheme_status);
                  break;
                }
              }
            }
          }
          drop(archive_reader);
        }
      }
    } // -- END: Archive::Reader, trying to localize (to .drop asap)

    TaskReport {
      task: self.clone(),
      status: status,
      messages: messages,
    }
  }

  /// Returns an open file handle to the task's entry
  pub fn prepare_input_stream(&self) -> Result<File, Error> {
    let entry_path = Path::new(&self.entry);
    let file = try!(File::open(entry_path));
    Ok(file)
  }
}

#[derive(RustcDecodable, RustcEncodable, Clone, Debug)]
/// A minimal description of a document collection. Defined by a name, path and simple/complex file system setup.
pub struct Corpus {
  /// optional id (None for mock / yet-to-be-inserted rows)
  pub id: Option<i32>,
  /// a human-readable name for this corpus
  pub name: String,
  /// file system path to corpus root
  /// (a corpus is held in a single top-level directory)
  pub path: String,
  /// are we using multiple files to represent a document entry?
  /// (if unsure, always use "true")
  pub complex: bool,
}
impl Default for Corpus {
  fn default() -> Self {
    Corpus {
      id: None,
      name: "mock corpus".to_string(),
      path: ".".to_string(),
      complex: true,
    }
  }
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

impl CortexORM for Corpus {
  fn get_id(&self) -> Option<i32> {
    self.id
  }
  fn select_by_id<'a>(&'a self, connection: &'a Connection) -> Result<Option<Corpus>, Error> {
    let stmt = try!(connection.prepare(
      "SELECT corpusid,name,path,complex FROM corpora WHERE corpusid = $1",
    ));
    let rows = try!(stmt.query(&[&self.id]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Corpus::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn select_by_key<'a>(&'a self, connection: &'a Connection) -> Result<Option<Corpus>, Error> {
    let stmt = try!(connection.prepare(
      "SELECT corpusid,name,path,complex FROM corpora WHERE name = $1",
    ));
    let rows = try!(stmt.query(&[&self.name]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Corpus::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn insert(&self, connection: &Connection) -> Result<(), Error> {
    try!(connection.execute(
      "INSERT INTO corpora (name, path, complex) values($1, $2, $3)",
      &[&self.name, &self.path, &self.complex],
    ));
    Ok(())
  }
  fn delete(&self, connection: &Connection) -> Result<(), Error> {
    try!(connection.execute(
      "DELETE FROM tasks WHERE corpusid = $1",
      &[&self.id],
    ));
    try!(connection.execute(
      "DELETE FROM corpora WHERE corpusid = $1",
      &[&self.id],
    ));
    Ok(())
  }
  fn from_row(row: Row) -> Self {
    Corpus {
      id: Some(row.get(0)),
      name: row.get(1),
      path: row.get(2),
      complex: row.get(3),
    }
  }
}
impl Corpus {
  /// Return a vector of services currently activated on this corpus
  pub fn select_services<'a>(&'a self, connection: &'a Connection) -> Result<Vec<Service>, Error> {
    let stmt = try!(connection.prepare(
      "SELECT distinct(serviceid) FROM tasks WHERE corpusid = $1",
    ));
    let rows = try!(stmt.query(&[&self.id]));
    let mut services = Vec::new();
    for row in rows.iter() {
      let service_result = Service {
        id: row.get(0),
        outputformat: String::new(),
        complex: true,
        inputconverter: None,
        name: String::new(),
        version: 0.1,
        inputformat: String::new(),
      }.select_by_id(connection);
      if let Ok(service_select) = service_result {
        if let Some(service) = service_select {
          services.push(service);
        }
      }
    }
    Ok(services)
  }

  /// Return a hash representation of the corpus, usually for frontend reports
  pub fn to_hash(&self) -> HashMap<String, String> {
    let mut hm = HashMap::new();
    hm.insert("name".to_string(), self.name.clone());
    hm
  }
}

#[derive(Clone)]
/// A `CorTeX` processing service
pub struct Service {
  /// optional id (None for mock / yet-to-be-inserted rows)
  pub id: Option<i32>,
  /// a human-readable name for this service
  pub name: String,
  /// a floating-point number to mark the current version (e.g. 0.01)
  pub version: f32,
  /// the expected input format for this service (e.g. tex)
  pub inputformat: String,
  /// the produced output format by this service (e.g. html)
  pub outputformat: String,
  // pub xpath : String,
  // pub resource : String,
  /// prerequisite input conversion service, if any
  pub inputconverter: Option<String>,
  /// is this service requiring more than the main textual content of a document?
  /// mark "true" if unsure
  pub complex: bool,
}
impl fmt::Debug for Service {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    // The `f` value implements the `Write` trait, which is what the
    // write! macro is expecting. Note that this formatting ignores the
    // various flags provided to format strings.
    let serviceid = match self.id {
      Some(serviceid) => serviceid.to_string(),
      None => "None".to_string(),
    };
    let mut ic = "None".to_string();
    if self.inputconverter.is_some() {
      ic = self.inputconverter.clone().unwrap();
    };
    write!(
      f,
      "(serviceid: {},\n\tname: {},\n\tversion: {},\n\tinputformat: {},\n\toutputformat: {},\n\tinputconverter: {},\n\tcomplex: {})\n",
      serviceid,
      self.name,
      self.version,
      self.inputformat,
      self.outputformat,
      ic,
      self.complex
    )
  }
}

impl CortexORM for Service {
  fn get_id(&self) -> Option<i32> {
    self.id
  }
  fn select_by_id<'a>(&'a self, connection: &'a Connection) -> Result<Option<Service>, Error> {
    let stmt = try!(connection.prepare("SELECT serviceid,name,version,inputformat,outputformat,inputconverter,complex FROM services WHERE serviceid = $1"));
    let rows = try!(stmt.query(&[&self.id]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Service::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn select_by_key<'a>(&'a self, connection: &'a Connection) -> Result<Option<Service>, Error> {
    let stmt = try!(connection.prepare("SELECT serviceid,name,version,inputformat,outputformat,inputconverter,complex FROM services WHERE name = $1 and version = $2"));
    let rows = try!(stmt.query(&[&self.name, &self.version]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Service::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn insert(&self, connection: &Connection) -> Result<(), Error> {
    try!(connection.execute("INSERT INTO services (name, version, inputformat, outputformat, inputconverter, complex) values($1, $2, $3, $4, $5, $6)",
                            &[&self.name, &self.version, &self.inputformat, &self.outputformat, &self.inputconverter, &self.complex]));
    Ok(())
  }
  fn delete(&self, connection: &Connection) -> Result<(), Error> {
    try!(connection.execute(
      "DELETE FROM tasks WHERE serviceid = $1",
      &[&self.id],
    ));
    try!(connection.execute(
      "DELETE FROM services WHERE serviceid = $1",
      &[&self.id],
    ));
    Ok(())
  }
  fn from_row(row: Row) -> Self {
    Service {
      id: Some(row.get(0)),
      name: row.get(1),
      version: row.get(2),
      inputformat: row.get(3),
      outputformat: row.get(4),
      inputconverter: row.get(5),
      complex: row.get(6),
    }
  }
}
impl Service {
  /// Select a service from the Task store via its human-readable name. Requires a postgres `Connection`.
  pub fn from_name(connection: &Connection, name: String) -> Result<Option<Self>, Error> {
    let stmt = try!(connection.prepare("SELECT serviceid,name,version,inputformat,outputformat,inputconverter,complex FROM services WHERE name = $1"));
    let rows = try!(stmt.query(&[&name]));
    if rows.len() == 1 {
      let row = rows.get(0);
      Ok(Some(Service::from_row(row)))
    } else {
      Ok(None)
    }
  }
  /// Returns a hash representation of the `Service`, usually for frontend reports
  pub fn to_hash(&self) -> HashMap<String, String> {
    let mut hm = HashMap::new();
    hm.insert(
      "id".to_string(),
      match self.id {
        Some(id) => id.to_string(),
        None => "None".to_string(),
      },
    );
    hm.insert("name".to_string(), self.name.clone());
    hm.insert("version".to_string(), self.version.to_string());
    hm.insert("inputformat".to_string(), self.inputformat.clone());
    hm.insert("outputformat".to_string(), self.outputformat.clone());
    hm.insert(
      "inputconverter".to_string(),
      match self.inputconverter.clone() {
        Some(ic) => ic,
        None => "None".to_string(),
      },
    );
    hm.insert("complex".to_string(), self.complex.to_string());
    hm
  }
}
