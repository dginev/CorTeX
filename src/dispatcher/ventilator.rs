use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::Mutex;

use crate::backend;
use crate::dispatcher::server;
use crate::helpers;
use crate::helpers::{TaskProgress, TaskReport};
use crate::models::{Service, WorkerMetadataSender};
use std::error::Error;
use zmq::SNDMORE;

/// How often (seconds) the ventilator reaps timed-out in-flight tasks. Well below the per-task
/// timeout (`TaskProgress::expected_at`, ≥1h), so expired tasks are recovered promptly without
/// scanning the in-flight set on every request.
const REAP_INTERVAL_SECS: i64 = 60;

/// Specifies the binding and operation parameters for a ZMQ ventilator component
pub struct Ventilator {
  /// port to listen on
  pub port: usize,
  /// the size of the dispatch queue
  /// (also the batch size for Task store queue requests)
  pub queue_size: usize,
  /// size of an individual message chunk sent via zeromq
  /// (keep this small to avoid large RAM use, increase to reduce network bandwidth)
  pub message_size: usize,
  /// address for the Task store postgres endpoint
  pub backend_address: String,
  /// backpressure threshold: stop leasing new work once this many tasks are in-flight
  /// (dispatched-but-unfinished), so the in-flight set drains via the sink instead of growing
  /// toward the hard panic bound (KNOWN_ISSUES D-6)
  pub max_in_flight: usize,
  /// non-blocking handle to the background worker-metadata writer
  pub metadata: WorkerMetadataSender,
}

impl Ventilator {
  /// Starts a new dispatch `Server` (ZMQ Ventilator), to serve tasks to processing workers.
  /// The ventilator shares state with other manager threads via queues for tasks in progress,
  /// as well as a queue for completed tasks pending persisting to disk.
  /// A job limit can be provided as a termination condition for the sink server.
  ///
  /// Upon premature termination, returns the number of tasks processed.
  pub fn start(
    &self,
    services_arc: &Arc<Mutex<HashMap<String, Option<Service>>>>,
    progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress>>>,
    done_queue_arc: &Arc<Mutex<Vec<TaskReport>>>,
    job_limit: Option<usize>,
  ) -> Result<usize, Box<dyn Error>> {
    // We have a Ventilator-exclusive "queues" stack of tasks to be dispatched, keyed by service id
    // so a reaped task is always re-queued to its own service (not whichever service is
    // requesting).
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    // Recover leftover Queued tasks from a previously-crashed run — but NOT the ones currently in
    // flight. On a ventilator *restart* (KNOWN_ISSUES D-4) the sink is still processing dispatched
    // tasks held in `progress_queue`; resetting those to TODO would re-lease them while their
    // original results are still pending (a double-dispatch). On first start `progress_queue` is
    // empty, so this recovers all leftover Queued tasks exactly as before.
    let mut backend = backend::from_address(&self.backend_address);
    let in_flight_ids: Vec<i64> = progress_queue_arc
      .lock()
      .expect("progress_queue mutex poisoned")
      .keys()
      .copied()
      .collect();
    backend.clear_limbo_tasks_except(&in_flight_ids)?;
    // Ok, let's bind to a port and start broadcasting
    let context = zmq::Context::new();
    let ventilator = context.socket(zmq::ROUTER)?;
    ventilator.set_router_handover(true)?;

    let address = format!("tcp://*:{}", self.port);
    ventilator.bind(&address).unwrap();
    let mut source_job_count: usize = 0;
    // Reap timed-out in-flight tasks on a cadence rather than only on refetch (KNOWN_ISSUES D-6),
    // so the in-flight set drains even under sustained backpressure (when refetch never runs).
    let mut last_reap_sec = chrono::Utc::now().timestamp();

    loop {
      let mut identity = zmq::Message::new();
      let mut msg = zmq::Message::new();
      // There appears to be a very rare failure mode in 08.2025 sandbox conversion testing,
      // where 3 adjacent empty messages are received by the ventilator, causing a permanetly
      // shuffled state.
      while identity.is_empty() {
        ventilator.recv(&mut identity, 0)?;
      }
      ventilator.recv(&mut msg, 0)?;
      let identity_str = identity.as_str().unwrap_or_default().to_string();
      let service_name = msg.as_str().unwrap_or_default().to_string();
      if identity_str.is_empty() && service_name.is_empty() {
        // careful to only skip if both empty, to avoid evenness issues. But a restart would be
        // healthier really.
        eprintln!(
          "-- FAILURE: empty request {service_name:?} requested by worker {identity_str:?}. Skip."
        );
        return Ok(source_job_count);
      }

      let request_time = chrono::Utc::now();
      source_job_count += 1;
      // Reap timed-out in-flight tasks on a cadence (decoupled from refetch): routes each expired
      // task back to its own service's queue or reports it Fatal, so the in-flight set drains even
      // while saturated (backpressure) — closes the D-6 reaping-coupling residual.
      if request_time.timestamp() - last_reap_sec >= REAP_INTERVAL_SECS {
        last_reap_sec = request_time.timestamp();
        server::reap_expired_into(&mut queues, progress_queue_arc, done_queue_arc);
      }
      let mut dispatched_task_opt: Option<TaskProgress> = None;
      // Requests for unknown service names will be silently ignored.
      let service_opt = match server::get_sync_service(&service_name, services_arc, &mut backend) {
        Some(s) => Some(s),
        None => {
          // As it happens, we can never survive this mistake with our current zmq code. We need a
          // full reboot to regain sanity.
          eprintln!("-- FAILURE: unknown service name {service_name:?} requested by worker {identity_str:?}. Mock response sent.");
          ventilator.send(identity, SNDMORE)?;
          ventilator.send("0", SNDMORE)?;
          ventilator.send(Vec::new(), 0)?;
          continue;
        },
      };
      if let Some(service) = service_opt {
        // Backpressure (KNOWN_ISSUES D-6, principle #4): if the in-flight set is saturated, don't
        // lease more work — mock-reply so the worker backs off and retries. The set then drains as
        // the sink receives results, instead of growing toward the hard panic bound. Degrade
        // gracefully under overload rather than crash.
        if server::in_flight_saturated(
          server::progress_queue_len(progress_queue_arc),
          self.max_in_flight,
        ) {
          eprintln!(
            "-- BACKPRESSURE: in-flight set at capacity ({}); mock-replying to worker {identity_str:?}",
            self.max_in_flight
          );
          ventilator.send(identity, SNDMORE)?;
          ventilator.send("0", SNDMORE)?;
          ventilator.send(Vec::new(), 0)?;
          continue;
        }
        let task_queue: &mut Vec<TaskProgress> = queues.entry(service.id).or_default();
        if task_queue.is_empty() {
          eprintln!(
            "-- No tasks in task queue for service {:?}, fetching up to {:?} more from backend...",
            service_name, self.queue_size
          );
          // Refetch a new batch of tasks
          let now = chrono::Utc::now().timestamp();
          let fetched_tasks = backend
            .fetch_tasks(&service, self.queue_size)
            .unwrap_or_default();
          task_queue.extend(fetched_tasks.into_iter().map(|task| TaskProgress {
            task,
            created_at: now,
            retries: 0,
          }));
        }

        ventilator.send(identity, SNDMORE)?;
        let mut taskid = -1;
        if let Some(current_task_progress) = task_queue.pop() {
          dispatched_task_opt = Some(current_task_progress.clone());

          let current_task = current_task_progress.task;
          taskid = current_task.id;
          let serviceid = current_task.service_id;
          eprintln!("vent {source_job_count}: worker {identity_str:?} received task {taskid:?}");
          ventilator.send(&taskid.to_string(), SNDMORE)?;
          if serviceid == 1 {
            // No payload needed for init
            ventilator.send(Vec::new(), 0)?;
          } else {
            // Regular services fetch the task payload and transfer it to the worker
            let file_opt = helpers::prepare_input_stream(&current_task);
            if file_opt.is_ok() {
              let mut file = file_opt?;
              let mut total_outgoing: usize = 0;
              loop {
                // Stream input data via zmq
                let mut data = vec![0; self.message_size];
                let size = file.read(&mut data)?;
                total_outgoing += size;
                data.truncate(size);

                if size < self.message_size {
                  // If exhausted, send the last frame
                  ventilator.send(&data, 0)?;
                  // And terminate
                  break;
                } else {
                  // If more to go, send the frame and indicate there's more to come
                  ventilator.send(&data, SNDMORE)?;
                }
              }
              let responded_time = chrono::Utc::now();
              let request_duration = (responded_time - request_time).num_milliseconds();
              eprintln!(
                "vent {source_job_count}: message size: {total_outgoing}, took {request_duration}ms.");
            } else {
              eprintln!("-- Failed to prepare input stream for taskid {taskid:?}");
              eprintln!("-- task details: {current_task:?}");
              taskid = -1;
              ventilator.send(Vec::new(), 0)?;
            }
          }
        } else {
          eprintln!("vent {source_job_count:?}: worker {identity_str:?} received mock reply.");
          ventilator.send("0", SNDMORE)?;
          ventilator.send(Vec::new(), 0)?;
        }
        // Update this worker's metadata (non-blocking enqueue to the background writer)
        self.metadata.dispatched(identity_str, service.id, taskid);
      } else {
        eprintln!(
          "-- No such service {service_name:?} in ventilator request from {identity_str:?}"
        );
      }
      // Record that a task has been dispatched in the progress queue
      if let Some(dispatched_task) = dispatched_task_opt {
        server::push_progress_task(progress_queue_arc, dispatched_task);
      }
      if let Some(limit_number) = job_limit {
        if source_job_count >= limit_number {
          eprintln!("vent {limit_number}: job limit reached, terminating Ventilator thread...");
          return Ok(source_job_count);
        }
      }
    }
  }
}
