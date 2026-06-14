// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::HashMap;
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::{self, sleep};
use std::time::Duration;

use crate::backend::{build_pool, default_db_address};
use crate::config::config;
use crate::dispatcher::finalize::Finalize;
use crate::dispatcher::sink::Sink;
use crate::dispatcher::ventilator::Ventilator;
use crate::helpers::{TaskProgress, TaskReport};
use crate::models::{start_metadata_writer, Service};
use zmq::Error;

/// Manager struct responsible for dispatching and receiving tasks
pub struct TaskManager {
  /// port for requesting/dispatching jobs
  pub source_port: usize,
  /// port for responding/receiving results
  pub result_port: usize,
  /// the size of the dispatch queue
  /// (also the batch size for Task store queue requests)
  pub queue_size: usize,
  /// size of an individual message chunk sent via zeromq
  /// (keep this small to avoid large RAM use, increase to reduce network bandwidth)
  pub message_size: usize,
  /// backpressure threshold: max in-flight (dispatched-but-unfinished) tasks before the ventilator
  /// stops leasing new work and mock-replies (KNOWN_ISSUES D-6)
  pub max_in_flight: usize,
  /// address for the Task store postgres endpoint
  pub backend_address: String,
}

impl Default for TaskManager {
  fn default() -> TaskManager {
    TaskManager {
      source_port: 51695,
      result_port: 51696,
      queue_size: 100,
      message_size: 100_000,
      max_in_flight: config().dispatcher.max_in_flight,
      backend_address: default_db_address().to_string(),
    }
  }
}

impl TaskManager {
  /// Starts a new manager, spinning of dispatch/sink servers, listening on the specified ports
  pub fn start(&self, job_limit: Option<usize>) -> Result<(), Error> {
    // We'll use some local memoization shared between source and sink:
    let services: HashMap<String, Option<Service>> = HashMap::new();
    let progress_queue: HashMap<i64, TaskProgress> = HashMap::new();

    let services_arc = Arc::new(Mutex::new(services));
    let progress_queue_arc = Arc::new(Mutex::new(progress_queue));

    // Done queue (phase 1): a **bounded** channel instead of `Arc<Mutex<Vec<TaskReport>>>` + a
    // panic backstop. The sink + ventilator-reaper `send` finished reports (cloning the
    // sender); the finalize thread owns the single receiver. A full channel blocks the
    // producers (backpressure), never drops. `done_tx` is kept alive in this scope so the
    // channel stays open across ventilator restarts — it disconnects (a clean finalize
    // shutdown) only when this method returns.
    let (done_tx, done_rx) =
      sync_channel::<TaskReport>(crate::dispatcher::server::DONE_QUEUE_CAPACITY);

    // Single background worker-metadata writer fed by a non-blocking channel: O(1) threads instead
    // of a detached thread per ZMQ event (KNOWN_ISSUES D-1), writing over a pooled connection
    // (~11us vs a ~4.5ms fresh connect; the Arm 14 spike). The ventilator/sink clone the sender;
    // the writer stops when all senders drop (i.e. when this method returns).
    let metadata = start_metadata_writer(build_pool(
      &self.backend_address,
      config().database.pool_size,
    ));

    // First prepare the source ventilator
    let source_port = self.source_port;
    let source_queue_size = self.queue_size;
    let source_message_size = self.message_size;
    let source_max_in_flight = self.max_in_flight;
    let source_backend_address = self.backend_address.clone();

    // Next prepare the finalize thread which will persist finished jobs to the DB. It owns the
    // single receiver end of the bounded done channel (moved in here).
    let finalize_backend_address = self.backend_address.clone();
    let finalize_thread = thread::spawn(move || {
      Finalize {
        backend_address: finalize_backend_address,
        job_limit,
      }
      .start(done_rx)
      .unwrap_or_else(|e| panic!("Failed in finalize thread: {e:?}"));
    });

    // Now prepare the results sink
    let result_port = self.result_port;
    let result_queue_size = self.queue_size;
    let result_message_size = self.message_size;
    let result_backend_address = self.backend_address.clone();

    let sink_services_arc = services_arc.clone();
    let sink_progress_queue_arc = progress_queue_arc.clone();

    let sink_done_tx = done_tx.clone();
    let sink_metadata = metadata.clone();
    let sink_thread = thread::spawn(move || {
      Sink {
        port: result_port,
        queue_size: result_queue_size,
        message_size: result_message_size,
        backend_address: result_backend_address.clone(),
        metadata: sink_metadata,
      }
      .start(
        &sink_services_arc,
        &sink_progress_queue_arc,
        &sink_done_tx,
        job_limit,
      )
      .unwrap_or_else(|e| panic!("Failed in sink thread: {e:?}"));
    });

    // 09.2025, Currently the ventilator has some hard to reproduce fragility to empty messages
    //          which necessitates a restart of the thread. If we can reproduce that better,
    //          it may be possible to return to the previous single-threaded lifecycle.
    loop {
      let vent_services_arc = services_arc.clone();
      let vent_progress_queue_arc = progress_queue_arc.clone();
      let vent_done_tx = done_tx.clone();
      let vent_backend_address = source_backend_address.clone();
      let vent_metadata = metadata.clone();
      let vent_thread = thread::spawn(move || {
        let ventilator = Ventilator {
          port: source_port,
          queue_size: source_queue_size,
          message_size: source_message_size,
          max_in_flight: source_max_in_flight,
          backend_address: vent_backend_address,
          metadata: vent_metadata,
        };
        ventilator
          .start(
            &vent_services_arc,
            &vent_progress_queue_arc,
            &vent_done_tx,
            job_limit,
          )
          .unwrap_or_else(|e| panic!("Failed in ventilator thread: {e:?}"));
      });
      if vent_thread.join().is_err() {
        eprintln!("-- Ventilator thread died unexpectedly!");
        return Err(zmq::Error::ETERM);
      }
      if job_limit.is_some() {
        break;
      }
      // Perpetual mode (`job_limit = None`, i.e. production): the sink and finalize threads are
      // spawned **once** (only the ventilator is restart-looped), and this loop never reaches their
      // joins below — so a sink/finalize that died (e.g. a panic on a DB runaway, or an unexpected
      // result) would otherwise leave the pipeline **silently stalled**: results pile up
      // unprocessed, the in-flight set saturates, the ventilator mock-replies forever, and nothing
      // aborts. Surface it as the intended fail-fast — abort so the external supervisor restarts
      // the whole dispatcher (CLAUDE.md "process abort → external restart"), rather than
      // stall unnoticed. (In `job_limit` mode we already `break`ed above, so a *cleanly
      // finished* sink/finalize is never mistaken for a death here.)
      if sink_thread.is_finished() {
        eprintln!("-- Sink thread died unexpectedly! Aborting for a supervised restart.");
        return Err(zmq::Error::ETERM);
      }
      if finalize_thread.is_finished() {
        eprintln!("-- Finalize (DB) thread died unexpectedly! Aborting for a supervised restart.");
        return Err(zmq::Error::ETERM);
      }
      sleep(Duration::from_secs(1));
    }
    if sink_thread.join().is_err() {
      eprintln!("-- Sink thread died unexpectedly!");
      Err(zmq::Error::ETERM)
    } else if finalize_thread.join().is_err() {
      eprintln!("-- DB thread died unexpectedly!");
      Err(zmq::Error::ETERM)
    } else {
      eprintln!("-- Manager successfully terminated!");
      Ok(())
    }
  }
}
