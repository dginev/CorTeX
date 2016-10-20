// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate pericortex;

use cortex::backend::{Backend, TEST_DB_ADDRESS};
use cortex::data::{Corpus,Service, Task, TaskStatus};
use cortex::manager::{TaskManager};
use pericortex::worker::{EchoWorker, Worker};
use cortex::importer::Importer;
use std::thread;
#[test]
fn mock_round_trip() {
  // Initialize a corpus, import a single task, and enable a service on it
  let job_limit : Option<usize> = Some(1);
  let test_backend = Backend::testdb();
  assert!(test_backend.setup_task_tables().is_ok());

  let mock_corpus = test_backend.add(
    Corpus {
      id : None,
      name : "mock round-trip corpus".to_string(),
      path : "tests/data/".to_string(),
      complex : true,
    }).unwrap();
  let echo_service = test_backend.add(
    Service {
      id : None,
      name : "echo_service".to_string(),
      version : 0.1,
      inputformat : "tex".to_string(),
      outputformat : "tex".to_string(),
      inputconverter : Some("import".to_string()),
      complex : true
    }).unwrap();
  let mut abs_path = Importer::cwd();
  abs_path.push("tests/data/1508.01222/1508.01222.zip");
  let abs_entry = abs_path.to_str().unwrap().to_string();
  test_backend.add(
    Task {
      id : None,
      entry : abs_entry.clone(),
      serviceid : 2, // Import service always has id 2
      corpusid : mock_corpus.id.unwrap(),
      status : TaskStatus::NoProblem.raw()
    }).unwrap();
  test_backend.add(
    Task {
      id : None,
      entry : abs_entry.clone(),
      serviceid : echo_service.id.unwrap(),
      corpusid : mock_corpus.id.unwrap(),
      status : TaskStatus::TODO.raw()
    }).unwrap();

  // Start up a ventilator/sink pair
  let manager_thread = thread::spawn(move || {
    let manager = TaskManager {
      source_port : 5555,
      result_port : 5556,
      queue_size : 100000,
      message_size : 100,
      backend_address : TEST_DB_ADDRESS.to_string()
    };
    assert!(manager.start(job_limit).is_ok());
  });

  // Start up an echo worker
  let worker = EchoWorker::default();
  // Perform a single echo task
  assert!(worker.start(job_limit).is_ok());
  assert!(manager_thread.join().is_ok());
  // TODO: Check round-trip success
}