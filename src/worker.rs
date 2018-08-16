// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Worker for performing corpus imports, when served as "init" tasks by the `CorTeX` dispatcher

extern crate pericortex;
extern crate rand;
extern crate zmq;

use rand::{thread_rng, Rng};
use std::fs::File;
use std::path::Path;
use std::thread;
use std::time::Duration;
use zmq::{Context, Error, Message, SNDMORE};

use backend;
use backend::DEFAULT_DB_ADDRESS;
use importer::Importer;
use models::{Corpus, NewCorpus, Task};
use pericortex::worker::Worker;

/// `Worker` for initializing/importing a new corpus into `CorTeX`
pub struct InitWorker {
  /// name of the service ("init")
  pub service: String,
  /// version, as usual
  pub version: f32,
  /// message size, as usual
  pub message_size: usize,
  /// full URL (including port) to task source/dispatcher
  pub source: String,
  /// full URL (including port) to task sink/receiver
  pub sink: String,
  /// address to the Task store backend
  /// (special case, only for the init service, third-party workers can't access the Task store
  /// directly)
  pub backend_address: String,
}
impl Default for InitWorker {
  fn default() -> InitWorker {
    InitWorker {
      service: "init".to_string(),
      version: 0.1,
      message_size: 100_000,
      source: "tcp://localhost:51695".to_string(),
      sink: "tcp://localhost:51696".to_string(),
      backend_address: DEFAULT_DB_ADDRESS.to_string(),
    }
  }
}
impl Worker for InitWorker {
  fn service(&self) -> String { self.service.clone() }
  fn source(&self) -> String { self.source.clone() }
  fn sink(&self) -> String { self.sink.clone() }
  fn message_size(&self) -> usize { self.message_size }

  fn convert(&self, path_opt: &Path) -> Option<File> {
    let path = path_opt.to_str().unwrap().to_string();
    let name = path.rsplitn(1, '/').next().unwrap_or(&path).to_lowercase(); // TODO: this is Unix path only
    let backend = backend::from_address(&self.backend_address);
    let corpus = NewCorpus {
      path: path.clone(),
      name: name.clone(),
      complex: true,
      description: String::new(),
    };
    // Add the new corpus.
    backend.add(&corpus).expect("Failed to create new corpus.");
    let registered_corpus =
      Corpus::find_by_name(&path, &backend.connection).expect("Failed to create new corpus.");

    // Create an importer for the corpus, and then process all entries to populate CorTeX tasks
    let importer = Importer {
      corpus: registered_corpus,
      backend: backend::from_address(&self.backend_address),
      cwd: Importer::cwd(),
    };

    importer.process().unwrap();
    // TODO: Stopgap, we should do the error-reporting well
    None
  }

  fn start(&self, limit: Option<usize>) -> Result<(), Error> {
    let mut work_counter = 0;
    // Connect to a task ventilator
    let context_source = Context::new();
    let source = context_source.socket(zmq::DEALER).unwrap();
    let letters: Vec<_> = "abcdefghijklmonpqrstuvwxyz".chars().collect();
    let mut identity = String::new();
    for _step in 1..20 {
      identity.push(*thread_rng().choose(&letters).unwrap());
    }
    source.set_identity(identity.as_bytes()).unwrap();

    assert!(source.connect(&self.source()).is_ok());
    // Connect to a task sink
    let context_sink = Context::new();
    let sink = context_sink.socket(zmq::PUSH).unwrap();
    assert!(sink.connect(&self.sink()).is_ok());
    let backend = backend::from_address(&self.backend_address);
    // Work in perpetuity
    loop {
      let mut taskid_msg = Message::new().unwrap();
      let mut recv_msg = Message::new().unwrap();

      source.send_str(&self.service(), 0).unwrap();
      source.recv(&mut taskid_msg, 0).unwrap();
      let taskid = taskid_msg.as_str().unwrap();

      // Terminating with an empty message in place of a payload (INIT is special)
      source.recv(&mut recv_msg, 0).unwrap();

      let task_result = Task::find(taskid.parse::<i64>().unwrap(), &backend.connection);
      let task = match task_result {
        Ok(t) => t,
        _ => {
          // If there was nothing to do, retry a minute later
          thread::sleep(Duration::new(60, 0));
          continue;
        },
      };

      self.convert(Path::new(&task.entry));
      sink.send_str(&identity, SNDMORE).unwrap();
      sink.send_str(&self.service(), SNDMORE).unwrap();
      sink.send_str(taskid, SNDMORE).unwrap();
      sink.send(&[], 0).unwrap();

      work_counter += 1;
      if let Some(upper_bound) = limit {
        if work_counter >= upper_bound {
          // Give enough time to complete the Final job.
          thread::sleep(Duration::from_millis(500));
          break;
        }
      };
    }
    Ok(())
  }
}
