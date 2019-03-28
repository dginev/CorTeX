// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

///! Import a new corpus into `CorTeX` from the command line.
///! Example run: `$ cargo run --release --example tex_to_html_import /data/arxmliv/ arXMLiv`
use std::env;

use cortex::backend::{Backend, DEFAULT_DB_ADDRESS};
use cortex::dispatcher::manager::TaskManager;
use cortex::helpers::TaskStatus;
use cortex::models::{Corpus, NewService, NewTask, Service};
use cortex::worker::InitWorker;
use pericortex::worker::Worker;
use std::thread;
use std::time::Duration;

fn main() {
  let mut input_args = env::args();
  let _ = input_args.next();
  let mut corpus_path = match input_args.next() {
    Some(path) => path,
    None => "/arXMLiv/modern".to_string(),
  };
  if let Some(c) = corpus_path.pop() {
    if c != '/' {
      corpus_path.push(c);
    }
  }
  corpus_path.push('/');
  println!("-- Importing corpus at {:?} ...", &corpus_path);
  let backend = Backend::default();

  if let Ok(corpus) = Corpus::find_by_path(&corpus_path, &backend.connection) {
    assert!(corpus.destroy(&backend.connection).is_ok());
  }

  backend
    .add(&NewTask {
      entry: corpus_path.clone(),
      service_id: 1, // Init service always has id 1
      corpus_id: 1,
      status: TaskStatus::TODO.raw(),
    })
    .unwrap();

  // Let us thread out a ventilator on a special port
  // Start up a ventilator/sink pair
  thread::spawn(move || {
    let manager = TaskManager {
      source_port: 5757,
      result_port: 5758,
      queue_size: 100_000,
      message_size: 100,
      backend_address: DEFAULT_DB_ADDRESS.to_string(),
    };
    assert!(manager.start(Some(1)).is_ok());
  });

  // Start up an init worker
  let mut worker = InitWorker {
    service: "init".to_string(),
    version: 0.1,
    message_size: 100_000,
    source: "tcp://localhost:5757".to_string(),
    sink: "tcp://localhost:5758".to_string(),
    backend_address: DEFAULT_DB_ADDRESS.to_string(),
    identity: "unknown:init:1".to_string()
  };
  // Perform a single echo task
  assert!(worker.start(Some(1)).is_ok());
  // Wait for the final finisher to persist to DB
  thread::sleep(Duration::new(2, 0)); // TODO: Can this be deterministic? Join?

  // Then add a TeX-to-HTML service on this corpus.
  let service_name = "tex_to_html";
  let service_registered = match Service::find_by_name(service_name, &backend.connection) {
    Ok(s) => s,
    Err(_) => {
      let new_service = NewService {
        name: service_name.to_string(),
        version: 0.1,
        inputformat: "tex".to_string(),
        outputformat: "html".to_string(),
        inputconverter: Some("import".to_string()),
        complex: true,
        description: String::from("mock"),
      };
      assert!(backend.add(&new_service).is_ok());
      let service_registered_result = Service::find_by_name(service_name, &backend.connection);
      assert!(service_registered_result.is_ok());
      service_registered_result.unwrap()
    },
  };

  assert!(backend
    .register_service(&service_registered, &corpus_path)
    .is_ok());
}
