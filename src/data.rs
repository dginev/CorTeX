// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

use rustc_serialize::json::{Json, ToJson};
use std::collections::BTreeMap;
use std::fs::File;
// use std::io::Read;
use std::path::Path;
use std::str;

use postgres::Connection;
use postgres::rows::{Row};
use postgres::error::Error;

use Archive::*;

// The CorTeX data structures and traits:

/// A minimalistic ORM trait for CorTeX data items
pub trait CortexORM {
  fn select_by_id<'a>(&'a self, connection: &'a Connection) -> Result<Option<Self>, Error> where Self: Sized;
  fn select_by_key<'a>(&'a self, connection : &'a Connection) -> Result<Option<Self>,Error> where Self: Sized;
  fn insert(&self, connection: &Connection) -> Result<(),Error>;
  fn delete(&self, connection: &Connection) -> Result<(),Error>;
  fn from_row(row : Row) -> Self;
  fn get_id(&self) -> Option<i32>;
}


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
        let taskid = match self.id {
          Some(taskid) => taskid.to_string(),
          None => "None".to_string()
        };
        write!(f, "(taskid: {}, entry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n", taskid, self.entry, self.serviceid, self.corpusid, self.status)
    }
}
impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // The `f` value implements the `Write` trait, which is what the
        // write! macro is expecting. Note that this formatting ignores the
        // various flags provided to format strings.
        let taskid = match self.id {
          Some(taskid) => taskid.to_string(),
          None => "None".to_string()
        };
        write!(f, "(taskid: {},\n\tentry: {},\n\tserviceid: {},\n\tcorpusid: {},\n\t status: {})\n", taskid, self.entry, self.serviceid, self.corpusid, self.status)
    }
}
impl CortexORM for Task {
  fn get_id(&self) -> Option<i32> {
    // TODO: Best way to deal with this?
    match self.id {
      Some(value) => Some(value as i32),
      None => None
    }
  }
  fn select_by_id<'a>(&'a self, connection : &'a Connection) -> Result<Option<Task>,Error> {
    let stmt = try!(connection.prepare("SELECT taskid,entry,serviceid,corpusid,status FROM tasks WHERE taskid = $1"));
    let rows = try!(stmt.query(&[&self.id]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Task::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn select_by_key<'a>(&'a self, connection : &'a Connection) -> Result<Option<Task>,Error> {
    let stmt = try!(connection.prepare("SELECT taskid,entry,serviceid,corpusid,status FROM tasks WHERE entry = $1 and serviceid = $2 and corpusid = $3"));
    let rows = try!(stmt.query(&[&self.entry, &self.serviceid, &self.corpusid]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Task::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn insert(&self, connection : &Connection) -> Result<(), Error> {
    try!(connection.execute("INSERT INTO tasks (serviceid, corpusid, status, entry) values($1, $2, $3, $4)", &[&self.serviceid, &self.corpusid, &self.status, &self.entry]));
    Ok(()) }
  fn delete(&self, connection: &Connection) -> Result<(),Error> {
    try!(connection.execute("DELETE FROM tasks WHERE taskid = $1", &[&self.id])); 
    Ok(()) 
  }
  fn from_row<'a>(row : Row) -> Self {
    let fix_width_entry : String = row.get(1);
    Task {
      id : Some(row.get(0)),
      entry : fix_width_entry.trim_right().to_string(),
      serviceid : row.get(2),
      corpusid : row.get(3),
      status : row.get(4)
    }
  }
}
impl Clone for Task {
  fn clone(&self) -> Self {
    Task {
      id : self.id.clone(),
      entry : self.entry.clone(),
      serviceid : self.serviceid.clone(),
      corpusid : self.corpusid.clone(),
      status : self.status.clone()
    }
  }
}
impl Task {
  pub fn generate_report(&self, result: &Path) -> TaskReport {
    println!("Preparing report for {:?}, result at {:?}",self.entry, result);
    let mut messages = Vec::new();
    // Let's open the archive file and find the name.log file:
    let log_name = self.id.unwrap().to_string() + ".log";
    let archive_reader = Reader::new().unwrap()
      .support_filter_all()
      .support_format_all()
      .open_filename(result.to_str().unwrap(), 10240).unwrap();
    loop {
      match archive_reader.next_header() {
        Ok(e) => {
          let current_name = e.pathname();
          if current_name != log_name {
            continue;
          } else {
            // In a "raw" read, we don't know the data size in advance. So we bite the bullet and
            // read the usually manageable log file in memory
            let mut raw_log_data = Vec::new();
            loop {
              match archive_reader.read_data(10240) {
                Ok(chunk) => raw_log_data.extend(chunk.into_iter()),
                Err(_) => {break}
              };
            }
            let log_string = str::from_utf8(&raw_log_data).unwrap();
            messages = self.parse_log(log_string.to_string());
          }
        },
        Err(_) => { break }
      }
    }

    TaskReport {
      task : self.clone(),
      status : TaskStatus::NoProblem,
      messages : messages
    }
  }

  /// Parses a log string which follows the LaTeXML convention
  /// (described at http://dlmf.nist.gov/LaTeXML/manual/errorcodes/index.html)
  pub fn parse_log(&self, log : String) -> Vec<TaskMessage> {
    let mut messages : Vec<TaskMessage> = Vec::new();
    let mut in_details_mode = false;
    
    // regexes:
    let message_line_regex = regex!(r"^([^ :]+):([^ :]+):([^ ]+)(\s(.*))?$");
    let start_tab_regex = regex!(r"^\t");
    for line in log.lines() {
      // Skip empty lines
      if line.is_empty() {continue;}
      // If we have found a message header and we're collecting details:
      if in_details_mode {
        // If the line starts with tab, we are indeed reading in details
        
        if start_tab_regex.is_match(line) {
          // Append details line to the last message
          let mut last_message = messages.pop().unwrap();
          last_message.details = last_message.details + "\n" + line;
          messages.push(last_message);
          continue; // This line has been consumed, next
        } else {
          // Otherwise, no tab at the line beginning means last message has ended
          in_details_mode = false;
        }
      }
      // Since this isn't a details line, check if it's a message line:
      match message_line_regex.captures(line) {
        Some(cap) => {
          // Indeed a message, so record it:
          let message = TaskMessage {
            severity : cap.at(1).unwrap_or("").to_string().to_lowercase(),
            category : cap.at(2).unwrap_or("").to_string().to_lowercase(),
            what     : cap.at(3).unwrap_or("").to_string().to_lowercase(),
            details  : cap.at(5).unwrap_or("").to_string()
          };
          // Prepare to record follow-up lines with the message details:
          in_details_mode = true;
          // Add to the array of parsed messages
          messages.push(message);
        },
        None => {
          // Otherwise line is just noise, continue...
          in_details_mode = false;
        }
      };
    }
    println!("All messages: {:?}", messages);
    messages
  }
}

// Task Reports (completed tasks)
pub struct TaskReport {
  pub task : Task,
  pub status : TaskStatus,
  pub messages : Vec<TaskMessage>
}
pub struct TaskMessage {
  pub category : String,
  pub severity : String, 
  pub what : String, 
  pub details : String
}
pub enum TaskStatus {
  NoProblem,
  Warning,
  Error,
  Fatal,
  TODO,
  Blocked(i32),
  Queued(i32)
}

impl fmt::Display for TaskMessage {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "(severity: {}, category: {},\n\twhat: {},\n\tdetails: {})\n", self.severity, self.category, self.what, self.details)
  }
}
impl fmt::Debug for TaskMessage {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "(severity: {}, category: {},\n\twhat: {},\n\tdetails: {})\n", self.severity, self.category, self.what, self.details)
  }
}
impl TaskStatus {
  pub fn raw(&self) -> i32 {
    match self {
      &TaskStatus::NoProblem => -1,
      &TaskStatus::Warning => -2,
      &TaskStatus::Error => -3,
      &TaskStatus::Fatal => -4,
      &TaskStatus::TODO => -5,
      &TaskStatus::Blocked(x) => x,
      &TaskStatus::Queued(x) => x
    }
  }
  pub fn from_raw(num : i32) -> Self {
    match num {
      -1 => TaskStatus::NoProblem,
      -2 => TaskStatus::Warning, 
      -3 => TaskStatus::Error,
      -4 => TaskStatus::Fatal,
      -5 => TaskStatus::TODO,
      num if num < -5 => TaskStatus::Blocked(num.clone()),
      _ => TaskStatus::Queued(num.clone())
    }
  }
}
/// A CorTeX "Corpus" is a minimal description of a document collection. It is defined by a name, path and simple/complex file system setup.
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

impl CortexORM for Corpus {
  fn get_id(&self) -> Option<i32> {self.id}
  fn select_by_id<'a>(&'a self, connection : &'a Connection) -> Result<Option<Corpus>,Error> {
    let stmt = try!(connection.prepare("SELECT corpusid,name,path,complex FROM corpora WHERE corpusid = $1"));
    let rows = try!(stmt.query(&[&self.id]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Corpus::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn select_by_key<'a>(&'a self, connection : &'a Connection) -> Result<Option<Corpus>,Error> {
    let stmt = try!(connection.prepare("SELECT corpusid,name,path,complex FROM corpora WHERE name = $1"));
    let rows = try!(stmt.query(&[&self.name]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Corpus::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn insert(&self, connection : &Connection) -> Result<(), Error> {
    try!(connection.execute("INSERT INTO corpora (name, path, complex) values($1, $2, $3)", &[&self.name, &self.path, &self.complex]));
    Ok(()) }
  fn delete(&self, connection: &Connection) -> Result<(),Error> {
    try!(connection.execute("DELETE FROM tasks WHERE corpusid = $1", &[&self.id])); 
    try!(connection.execute("DELETE FROM corpora WHERE corpusid = $1", &[&self.id])); 
    Ok(()) 
  }
  fn from_row(row : Row) -> Self {
    Corpus {
      id : Some(row.get(0)),
      name : row.get(1),
      path : row.get(2),
      complex : row.get(3)
    }
  }
}
// Services
pub struct Service {
  pub id : Option<i32>,
  pub name : String,
  pub version : f32,
  // pub url : String,
  pub inputformat : String,
  pub outputformat : String,
  // pub xpath : String,
  // pub resource : String,
  pub inputconverter : Option<String>,
  pub complex : bool, 
}
impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // The `f` value implements the `Write` trait, which is what the
        // write! macro is expecting. Note that this formatting ignores the
        // various flags provided to format strings.
        let serviceid = match self.id {
          Some(serviceid) => serviceid.to_string(),
          None => "None".to_string()
        };
        let mut ic = "None".to_string();
        if self.inputconverter.is_some() {
          ic = self.inputconverter.clone().unwrap();
        };
        write!(f, "(serviceid: {},\n\tname: {},\n\tversion: {},\n\tinputformat: {},\n\toutputformat: {},\n\tinputconverter: {},\n\tcomplex: {})\n",
                    serviceid, self.name, self.version, self.inputformat, self.outputformat, ic, self.complex)
    }
}
impl Clone for Service {
  fn clone(&self) -> Self {
    Service {
      id : self.id.clone(),
      name : self.name.clone(),
      version : self.version.clone(),
      inputformat : self.inputformat.clone(),
      outputformat : self.outputformat.clone(),
      inputconverter : self.inputconverter.clone(),
      complex : self.complex.clone()
    }
  }
}
impl CortexORM for Service {
  fn get_id(&self) -> Option<i32> {self.id}
  fn select_by_id<'a>(&'a self, connection : &'a Connection) -> Result<Option<Service>,Error> {
    let stmt =  try!(connection.prepare("SELECT serviceid,name,version,inputformat,outputformat,inputconverter,complex FROM services WHERE serviceid = $1"));
    let rows = try!(stmt.query(&[&self.id]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Service::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn select_by_key<'a>(&'a self, connection : &'a Connection) -> Result<Option<Service>,Error> {
    let stmt =  try!(connection.prepare("SELECT serviceid,name,version,inputformat,outputformat,inputconverter,complex FROM services WHERE name = $1 and version = $2"));
    let rows = try!(stmt.query(&[&self.name, &self.version]));
    if rows.len() > 0 {
      let row = rows.get(0);
      Ok(Some(Service::from_row(row)))
    } else {
      Ok(None)
    }
  }
  fn insert(&self, connection : &Connection) -> Result<(), Error> {
    try!(connection.execute("INSERT INTO services (name, version, inputformat, outputformat, inputconverter, complex) values($1, $2, $3, $4, $5, $6)",
       &[&self.name, &self.version, &self.inputformat, &self.outputformat, &self.inputconverter, &self.complex]));
    Ok(()) }
  fn delete(&self, connection: &Connection) -> Result<(),Error> {
    try!(connection.execute("DELETE FROM tasks WHERE serviceid = $1", &[&self.id])); 
    try!(connection.execute("DELETE FROM services WHERE serviceid = $1", &[&self.id])); 
    Ok(()) 
  }
  fn from_row(row : Row) -> Self {
    Service {
      id : Some(row.get(0)),
      name : row.get(1),
      version : row.get(2),
      inputformat : row.get(3),
      outputformat : row.get(4),
      inputconverter : row.get(5),
      complex : row.get(6)
    }
  }
}
impl Service { 
  pub fn from_name(connection : &Connection, name : String) -> Result<Option<Self>, Error> { 
    let stmt =  try!(connection.prepare("SELECT serviceid,name,version,inputformat,outputformat,inputconverter,complex FROM services WHERE name = $1"));
    let rows = try!(stmt.query(&[&name]));
    if rows.len() == 1 {
      let row = rows.get(0);
      Ok(Some(Service::from_row(row)))
    } else {
      Ok(None)
    }
  }
  pub fn prepare_input_stream(&self, task: Task) -> Result<File, Error> {
    let entry_path = Path::new(&task.entry);
    let file = try!(File::open(entry_path));
    Ok(file)
  }
}