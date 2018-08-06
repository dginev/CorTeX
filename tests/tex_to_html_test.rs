// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate diesel;
extern crate pericortex;

use cortex::backend;
use cortex::backend::TEST_DB_ADDRESS;
use cortex::dispatcher::manager::TaskManager;
use cortex::helpers::TaskStatus;
use cortex::importer::Importer;
use cortex::models::{Corpus, NewCorpus, NewService, NewTask, Service, Task};
use cortex::schema::{corpora, services, tasks};
use diesel::delete;
use diesel::prelude::*;
use pericortex::worker::{TexToHtmlWorker, Worker};
use std::process::Command;
use std::str;
use std::thread;
use std::time::Duration;

#[test]
fn mock_tex_to_html() {
  let job_limit: Option<usize> = Some(1);
  // Check if we have latexmlc installed, skip otherwise:
  let which_result = Command::new("which")
    .arg("latexmlc")
    .output()
    .unwrap()
    .stdout;
  let latexmlc_path = str::from_utf8(&which_result).unwrap();
  if latexmlc_path.is_empty() {
    println!("latexmlc not installed, skipping test");
    return assert!(true);
  }
  // Initialize a corpus, import a single task, and enable a service on it
  let test_backend = backend::testdb();
  // assert!(test_backend.setup_task_tables().is_ok());
  let corpus_name = "tex_to_html_test corpus";
  let service_name = "tex_to_html";

  let mut abs_path = Importer::cwd();
  abs_path.push("tests/data/cond-mat9912403/cond-mat9912403.zip");
  let abs_entry = abs_path.to_str().unwrap().to_string();

  // Clean slate
  let clean_slate_result = delete(corpora::table)
    .filter(corpora::name.eq(corpus_name))
    .execute(&test_backend.connection);
  assert!(clean_slate_result.is_ok());
  let service_clean_slate_result = delete(services::table)
    .filter(services::name.eq(service_name))
    .execute(&test_backend.connection);
  assert!(service_clean_slate_result.is_ok());
  let task_clean_slate_result = delete(tasks::table)
    .filter(tasks::entry.eq(&abs_entry))
    .execute(&test_backend.connection);
  assert!(task_clean_slate_result.is_ok());

  let add_corpus_result = test_backend.add(&NewCorpus {
    name: corpus_name.to_string(),
    path: "tests/data/".to_string(),
    complex: true,
    description: String::new(),
  });
  assert!(add_corpus_result.is_ok());
  let corpus_result = Corpus::find_by_name(corpus_name, &test_backend.connection);
  assert!(corpus_result.is_ok());
  let registered_corpus = corpus_result.unwrap();

  let add_service_result = test_backend.add(&NewService {
    name: service_name.to_string(),
    version: 0.1,
    inputformat: "tex".to_string(),
    outputformat: "html".to_string(),
    inputconverter: Some("import".to_string()),
    complex: true,
    description: String::from("mock"),
  });
  assert!(add_service_result.is_ok());
  let service_result = Service::find_by_name(service_name, &test_backend.connection);
  let tex_to_html_service = service_result.unwrap();

  let conversion_task = NewTask {
    entry: abs_entry.clone(),
    service_id: tex_to_html_service.id,
    corpus_id: registered_corpus.id,
    status: TaskStatus::TODO.raw(),
  };
  let add_conversion_task = test_backend.add(&conversion_task);
  assert!(add_conversion_task.is_ok());

  // Start up a ventilator/sink pair
  let manager_thread = thread::spawn(move || {
    let manager = TaskManager {
      backend_address: TEST_DB_ADDRESS.to_string(),
      ..TaskManager::default()
    };
    assert!(manager.start(job_limit).is_ok());
  });
  // Start up an tex to html worker
  let worker = TexToHtmlWorker {
    source: "tcp://localhost:51695".to_string(),
    sink: "tcp://localhost:51696".to_string(),
    ..TexToHtmlWorker::default()
  };
  // Perform a single echo task
  assert!(worker.start(job_limit).is_ok());
  // Wait for the finisher to persist to DB
  thread::sleep(Duration::new(2, 0)); // TODO: Can this be deterministic? Join?
  assert!(manager_thread.join().is_ok());
  // Check round-trip success
  let finished_task_result = Task::find_by_entry(&conversion_task.entry, &test_backend.connection);
  assert!(finished_task_result.is_ok());
  let finished_task = finished_task_result.unwrap();
  println!("Finished: {:?}", finished_task);
  // This particular test finishes with an Error with the current LaTeXML (needs cmp.sty).
  assert!(finished_task.status == TaskStatus::Error.raw())
}
