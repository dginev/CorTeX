// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate pericortex;
extern crate zmq;
extern crate rand;

use zmq::{Context, Message, SNDMORE, Error};
use std::path::Path;
use std::fs::File;
use std::thread;
use backend::{DEFAULT_DB_ADDRESS, Backend};
use data::{Task, Corpus};
use importer::Importer;
use pericortex::worker::Worker;

pub struct InitWorker {
  pub service : String,
  pub version : f32,
  pub message_size : usize,
  pub source : String,
  pub sink : String,
  pub backend_address : String
}
impl Default for InitWorker {
  fn default() -> InitWorker {
    InitWorker {
      service : "init".to_string(),
      version : 0.1,
      message_size : 100000,
      source : "tcp://localhost:5555".to_string(),
      sink : "tcp://localhost:5556".to_string(),
      backend_address : DEFAULT_DB_ADDRESS.to_string()
    }
  }
}
impl Worker for InitWorker {
  fn service(&self) -> String {self.service.clone()}
  fn source(&self) -> String {self.source.clone()}
  fn sink(&self) -> String {self.sink.clone()}
  fn message_size(&self) -> usize {self.message_size.clone()}

  fn convert(&self, path : &Path) -> Option<File> {
    let path_str = path.to_str().unwrap().to_string();
    let backend = Backend::from_address(&self.backend_address);
    let corpus = Corpus {
          id: None,
          path : path_str.clone(),
          name : path_str,
          complex : true };
    let checked_corpus = backend.add(corpus).unwrap();

    let importer = Importer {
      corpus: checked_corpus,
      backend: Backend::from_address(&self.backend_address),
      cwd : Importer::cwd() };
    
    importer.process().unwrap();
    // TODO: Stopgap, we should do the error-reporting well
    None
  }

  fn start(&self, limit : Option<i32>) -> Result<(), Error> {
    let mut work_counter = 0;
    // Connect to a task ventilator
    let mut context_source = Context::new();
    let mut source = context_source.socket(zmq::DEALER).unwrap();
    let identity : String = (0..10).map(|_| rand::random::<u8>() as char).collect();
    source.set_identity(identity.as_bytes()).unwrap();

    assert!(source.connect(&self.source()).is_ok());
    // Connect to a task sink
    let mut context_sink = Context::new();
    let mut sink = context_sink.socket(zmq::PUSH).unwrap();
    assert!(sink.connect(&self.sink()).is_ok());
    let backend = Backend::from_address(&self.backend_address);
    // Work in perpetuity
    loop {
      let mut taskid_msg = Message::new().unwrap();
      let mut recv_msg = Message::new().unwrap();

      source.send_str(&self.service(), 0).unwrap();
      source.recv(&mut taskid_msg, 0).unwrap();
      let taskid = taskid_msg.as_str().unwrap();
      
      // Terminating with an empty message in place of a payload (INIT is special)
      source.recv(&mut recv_msg, 0).unwrap();

      let placeholder_task = Task{
        id: Some(taskid.parse::<i64>().unwrap()),
        entry: String::new(),
        corpusid : 0,
        serviceid : 0,
        status : 0
      };
      let task = match backend.sync(&placeholder_task) {
        Ok(t) => t,
        _ => {
          // If there was nothing to do, retry a minute later
          thread::sleep_ms(60000);
          continue;
        }
      };

      self.convert(Path::new(&task.entry));
      
      sink.send_str(&self.service(), SNDMORE).unwrap();
      sink.send_str(taskid, SNDMORE).unwrap();
      sink.send(&[],0).unwrap(); 

      work_counter += 1;
      match limit {
        Some(upper_bound) => {
          if work_counter >= upper_bound {
            // Give enough time to complete the Final job.
            thread::sleep_ms(500);
            break;
          }
        },
        None => {}
      };
    }
    Ok(())
  }
}
