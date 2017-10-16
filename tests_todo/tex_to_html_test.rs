// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate pericortex;

use cortex::backend;
use cortex::backend::TEST_DB_ADDRESS;
use cortex::data::{Corpus,Service, Task, TaskStatus};
use cortex::manager::{TaskManager};
use pericortex::worker::{TexToHtmlWorker, Worker};
use cortex::importer::Importer;
use std::thread;
use std::time::Duration;
use std::str;
use std::process::Command;

#[test]
fn mock_tex_to_html() {
  let job_limit : Option<usize> = Some(1);
  // Check if we have latexmlc installed, skip otherwise:
  let which_result = Command::new("which").arg("latexmlc").output().unwrap().stdout;
  let latexmlc_path = str::from_utf8(&which_result).unwrap();
  if latexmlc_path.is_empty() {
    println!("latexmlc not installed, skipping test");
    return assert!(true);
  }
  // Initialize a corpus, import a single task, and enable a service on it
  let test_backend = backend::testdb();
  // assert!(test_backend.setup_task_tables().is_ok());

  let mock_corpus = test_backend.add(
    Corpus {
      id : None,
      name : "mock round-trip corpus".to_string(),
      path : "tests/data/".to_string(),
      complex : true,
    }).unwrap();
  let tex_to_html_service = test_backend.add(
    Service {
      id : None,
      name : "tex_to_html".to_string(),
      version : 0.1,
      inputformat : "tex".to_string(),
      outputformat : "html".to_string(),
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
  let conversion_task = Task {
      id : None,
      entry : abs_entry.clone(),
      serviceid : tex_to_html_service.id.unwrap(),
      corpusid : mock_corpus.id.unwrap(),
      status : TaskStatus::TODO.raw()
    };
  test_backend.add(conversion_task.clone()).unwrap();

  // Start up a ventilator/sink pair
  let manager_thread = thread::spawn(move || {
    let manager = TaskManager {
      backend_address : TEST_DB_ADDRESS.to_string(),
      ..TaskManager::default()
    };
    assert!(manager.start(job_limit).is_ok());
  });
  // Start up an tex to html worker
  let worker = TexToHtmlWorker {
      source: "tcp://localhost:5555".to_string(),
      sink: "tcp://localhost:5556".to_string(),
    ..TexToHtmlWorker::default()
  };
  // Perform a single echo task
  assert!(worker.start(job_limit).is_ok());
  // Wait for the finisher to persist to DB
  thread::sleep(Duration::new(2,0)); // TODO: Can this be deterministic? Join?
  assert!(manager_thread.join().is_ok());
  // Check round-trip success
  let finished_task = test_backend.sync(&conversion_task).unwrap();
  println!("Finished: {:?}", finished_task);
  // This particular test finishes with an Error with the current LaTeXML (needs cmp.sty).
  assert!(finished_task.status == TaskStatus::Error.raw())
}
