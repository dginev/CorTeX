// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate pericortex;
extern crate rustc_serialize;

// use std::collections::HashMap;
// use std::path::Path;
// use std::fs;
use std::env;
// use std::io::Read;
// use std::io::Error;

use std::thread;
use std::time::Duration;
use cortex::backend::{Backend, DEFAULT_DB_ADDRESS};
use cortex::data::{Task, TaskStatus, Service};
use cortex::manager::TaskManager;
use cortex::worker::InitWorker;
use pericortex::worker::Worker;

fn main() {
  let job_limit : Option<usize> = Some(1);
  let mut input_args = env::args();
  let _ = input_args.next();
  let corpus_path = match input_args.next() {
    Some(path) => path,
    None => "/arXMLiv/modern/".to_string()
  };
  let corpus_name = match input_args.next() {
    Some(name) => name,
    None => "arXMLiv".to_string()
  };
  println!("-- Importing {:?} at {:?} ...",corpus_name, corpus_path.clone());
  let backend = Backend::default();

  backend.add(
    Task {
      id : None,
      entry : corpus_path.clone(),
      serviceid : 1, // Init service always has id 1
      corpusid : 1,
      status : TaskStatus::TODO.raw()
    }).unwrap();

  // Let us thread out a ventilator on a special port
    // Start up a ventilator/sink pair
  thread::spawn(move || {
    let manager = TaskManager {
      source_port : 5757,
      result_port : 5758,
      queue_size : 100000,
      message_size : 100,
      backend_address : DEFAULT_DB_ADDRESS.to_string()
    };
    assert!(manager.start(job_limit).is_ok());
  });

  // Start up an init worker
  let worker = InitWorker {
    service : "init".to_string(),
    version : 0.1,
    message_size : 100000,
    source : "tcp://localhost:5757".to_string(),
    sink : "tcp://localhost:5758".to_string(),
    backend_address : DEFAULT_DB_ADDRESS.to_string()
  };
  // Perform a single echo task
  assert!(worker.start(job_limit).is_ok());
  // Wait for the final finisher to persist to DB
  thread::sleep(Duration::new(2,0)); // TODO: Can this be deterministic? Join?

  // Then add a TeX-to-HTML service on this corpus.
  let service_placeholder = Service {
    id : None,
    name : "tex_to_html".to_string(),
    version : 0.1,
    inputformat : "tex".to_string(),
    outputformat : "html".to_string(),
    inputconverter : Some("import".to_string()),
    complex : true
  };
  let service_synced = backend.sync(&service_placeholder).unwrap();
  let service_checked = match service_synced.id {
    Some(_) => service_synced,
    None => backend.add(service_synced).unwrap()
  };

  backend.register_service(service_checked, corpus_name).unwrap();
}
