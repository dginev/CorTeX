// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Worker for performing corpus imports, when served as "init" tasks by the `CorTeX` dispatcher
use rand::seq::SliceRandom;
use rand::thread_rng;
use std::borrow::Cow;
use std::error::Error;
use std::fs::File;
use std::path::Path;
use std::thread;
use std::time::Duration;
use zmq::{Context, Message, SNDMORE};

use crate::backend;
use crate::backend::default_db_address;
use crate::importer::Importer;
use crate::models::{Corpus, NewCorpus, Task};
use pericortex::worker::Worker;

/// Resolves the ZMQ socket identity for a worker: the **operator-configured** `identity` when set
/// (a stable, operator-controlled metadata key — its `worker_metadata` row then accumulates across
/// restarts instead of fragmenting under a fresh random name each start, KNOWN_ISSUES W-3),
/// otherwise a random ephemeral handle so an unconfigured worker still gets a unique name. The
/// operator is responsible for keeping configured identities unique per worker (two DEALER sockets
/// sharing an identity would break the dispatcher's ROUTER addressing).
fn resolve_worker_identity(configured: &str, rng: &mut impl rand::Rng) -> String {
  if !configured.is_empty() {
    return configured.to_string();
  }
  let letters: Vec<char> = "abcdefghijklmonpqrstuvwxyz".chars().collect();
  // 19 random letters — preserves the historical ephemeral-identity length/charset.
  (1..20).map(|_| *letters.choose(rng).unwrap()).collect()
}

/// `Worker` for initializing/importing a new corpus into `CorTeX`
#[derive(Debug, Clone)]
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
  /// thread-local unique identifier
  pub identity: String,
}
impl Default for InitWorker {
  fn default() -> InitWorker {
    InitWorker {
      service: "init".to_string(),
      version: 0.1,
      message_size: 100_000,
      source: "tcp://localhost:51695".to_string(),
      sink: "tcp://localhost:51696".to_string(),
      backend_address: default_db_address().to_string(),
      identity: String::new(),
    }
  }
}
impl Worker for InitWorker {
  fn get_service(&self) -> &str { &self.service }
  fn get_source_address(&self) -> Cow<'_, str> { Cow::Borrowed(&self.source) }
  fn get_sink_address(&self) -> Cow<'_, str> { Cow::Borrowed(&self.sink) }
  fn get_identity(&self) -> &str { &self.identity }
  fn set_identity(&mut self, identity: String) { self.identity = identity; }
  fn message_size(&self) -> usize { self.message_size }

  fn convert(&self, path_opt: &Path) -> Result<File, Box<dyn Error>> {
    let path = path_opt.to_str().unwrap().to_string();
    let name = path
      .rsplit_once('/')
      .map(|x| x.1)
      .unwrap_or(&path)
      .to_lowercase(); // TODO: this is Unix path only
    let mut backend = backend::from_address(&self.backend_address);
    let corpus = NewCorpus {
      name,
      path,
      complex: true,
      description: String::new(),
    };
    // Add the new corpus.
    backend.add(&corpus).expect("Failed to create new corpus.");
    let registered_corpus = Corpus::find_by_name(&corpus.name, &mut backend.connection)
      .expect("Failed to create new corpus.");

    // Create an importer for the corpus, and then process all entries to populate CorTeX tasks
    let mut importer = Importer {
      corpus: registered_corpus,
      backend: backend::from_address(&self.backend_address),
      ..Importer::default()
    };

    importer.process()?;
    // TODO: Stopgap, we should do the error-reporting well
    Err(From::from("init worker does not return a file handle."))
  }

  fn start(&mut self, limit: Option<usize>) -> Result<(), Box<dyn Error>> {
    let mut work_counter = 0;
    let mut rng = thread_rng();
    // Connect to a task ventilator
    let context_source = Context::new();
    let source = context_source.socket(zmq::DEALER).unwrap();
    let identity = resolve_worker_identity(&self.identity, &mut rng);
    source.set_identity(identity.as_bytes()).unwrap();

    source.connect(&self.get_source_address()).unwrap();
    // Connect to a task sink
    let context_sink = Context::new();
    let sink = context_sink.socket(zmq::PUSH).unwrap();
    sink.connect(&self.get_sink_address()).unwrap();
    let mut backend = backend::from_address(&self.backend_address);
    // Work in perpetuity
    loop {
      let mut taskid_msg = Message::new();
      let mut recv_msg = Message::new();

      source.send(self.get_service(), 0).unwrap();
      source.recv(&mut taskid_msg, 0).unwrap();
      let taskid = taskid_msg.as_str().unwrap();

      // Terminating with an empty message in place of a payload (INIT is special)
      source.recv(&mut recv_msg, 0).unwrap();

      let task_result = Task::find(taskid.parse::<i64>().unwrap(), &mut backend.connection);
      let task = match task_result {
        Ok(t) => t,
        _ => {
          // If there was nothing to do, retry a minute later
          thread::sleep(Duration::new(60, 0));
          continue;
        },
      };
      // ignore error for now, complete the task.
      let _ = self.convert(Path::new(&task.entry));
      sink.send(&identity, SNDMORE).unwrap();
      sink.send(self.get_service(), SNDMORE).unwrap();
      sink.send(taskid, SNDMORE).unwrap();
      sink.send(Vec::new(), 0).unwrap();

      work_counter += 1;
      if let Some(upper_bound) = limit
        && work_counter >= upper_bound
      {
        // Give enough time to complete the Final job.
        thread::sleep(Duration::from_millis(500));
        break;
      };
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::resolve_worker_identity;

  #[test]
  fn configured_identity_is_honored_verbatim() {
    // An operator-set identity is used as-is, giving a stable worker_metadata key (W-3).
    let mut rng = rand::thread_rng();
    assert_eq!(
      resolve_worker_identity("arxiv-host3:init:1", &mut rng),
      "arxiv-host3:init:1"
    );
  }

  #[test]
  fn empty_identity_falls_back_to_a_random_handle() {
    let mut rng = rand::thread_rng();
    let id = resolve_worker_identity("", &mut rng);
    assert_eq!(id.len(), 19, "preserves the historical 19-char length");
    assert!(
      id.chars().all(|c| c.is_ascii_lowercase()),
      "random handle is lowercase letters only"
    );
  }
}
