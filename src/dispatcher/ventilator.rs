use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::Mutex;
use time;

use crate::backend;
use crate::dispatcher::server;
use crate::helpers;
use crate::helpers::{NewTaskMessage, TaskProgress, TaskReport, TaskStatus};
use crate::models::{Service, WorkerMetadata};
use std::error::Error;
use zmq::SNDMORE;

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
}

impl Ventilator {
  /// Starts a new dispatch `Server` (ZMQ Ventilator), to serve tasks to processing workers.
  /// The ventilator shares state with other manager threads via queues for tasks in progress,
  /// as well as a queue for completed tasks pending persisting to disk.
  /// A job limit can be provided as a termination condition for the sink server.
  pub fn start(
    &self,
    services_arc: &Arc<Mutex<HashMap<String, Option<Service>>>>,
    progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress>>>,
    done_queue_arc: &Arc<Mutex<Vec<TaskReport>>>,
    job_limit: Option<usize>,
  ) -> Result<(), Box<Error>>
  {
    // We have a Ventilator-exclusive "queues" stack for tasks to be dispatched
    let mut queues: HashMap<String, Vec<TaskProgress>> = HashMap::new();
    // Assuming this is the only And tidy up the postgres tasks:
    let backend = backend::from_address(&self.backend_address);
    backend.clear_limbo_tasks()?;
    // Ok, let's bind to a port and start broadcasting
    let context = zmq::Context::new();
    let ventilator = context.socket(zmq::ROUTER)?;
    ventilator.set_router_handover(true)?;

    let address = format!("tcp://*:{}", self.port);
    assert!(ventilator.bind(&address).is_ok());
    let mut source_job_count: usize = 0;

    loop {
      let mut msg = zmq::Message::new();
      let mut identity = zmq::Message::new();
      ventilator.recv(&mut identity, 0)?;
      ventilator.recv(&mut msg, 0)?;
      let service_name = msg.as_str().unwrap_or_default().to_string();
      let identity_str = identity.as_str().unwrap_or_default().to_string();
      // println!("Task requested for service: {}", service_name.clone());
      let request_time = time::get_time();
      source_job_count += 1;

      let mut dispatched_task_opt: Option<TaskProgress> = None;
      // Requests for unknown service names will be silently ignored.
      if let Some(service) = server::get_sync_service(&service_name, &services_arc, &backend) {
        if !queues.contains_key(&service_name) {
          queues.insert(service_name.clone(), Vec::new());
        }
        let task_queue: &mut Vec<TaskProgress> = queues
          .get_mut(&service_name)
          .unwrap_or_else(|| panic!("Could not obtain queue mutex lock in main ventilator loop"));
        if task_queue.is_empty() {
          println!(
            "-- No tasks in task queue for service {:?}, fetching up to {:?} more from backend...",
            service_name, self.queue_size
          );
          // Refetch a new batch of tasks
          let now = time::get_time().sec;
          let fetched_tasks = backend
            .fetch_tasks(&service, self.queue_size)
            .unwrap_or_default();
          task_queue.extend(fetched_tasks.into_iter().map(|task| TaskProgress {
            task,
            created_at: now,
            retries: 0,
          }));

          // This is a good time to also take care that none of the old tasks are dead in the
          // progress queue since the re-fetch happens infrequently, and directly
          // implies the progress queue will grow
          let expired_tasks = server::timeout_progress_tasks(&progress_queue_arc);
          for expired_t in expired_tasks {
            if expired_t.retries > 4 {
              // Too many retries, mark as fatal failure
              server::push_done_queue(
                &done_queue_arc,
                TaskReport {
                  task: expired_t.task.clone(),
                  status: TaskStatus::Fatal,
                  messages: vec![NewTaskMessage::new(
                    expired_t.task.id,
                    "fatal",
                    "cortex".to_string(),
                    "never_completed_with_retries".to_string(),
                    String::new(),
                  )],
                },
              );
            } else {
              // We can still retry, re-add to the dispatch queue
              task_queue.push(TaskProgress {
                task: expired_t.task,
                created_at: expired_t.created_at,
                retries: expired_t.retries + 1,
              });
            }
          }
        }

        ventilator.send(identity, SNDMORE)?;
        let mut taskid = -1;
        if let Some(current_task_progress) = task_queue.pop() {
          dispatched_task_opt = Some(current_task_progress.clone());

          let current_task = current_task_progress.task;
          taskid = current_task.id;
          let serviceid = current_task.service_id;
          println!(
            "vent {}: worker {:?} received task {:?}",
            source_job_count, identity_str, taskid
          );
          ventilator.send(&taskid.to_string(), SNDMORE)?;
          if serviceid == 1 {
            // No payload needed for init
            ventilator.send(&Vec::new(), 0)?;
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
              let responded_time = time::get_time();
              let request_duration = (responded_time - request_time).num_milliseconds();
              println!(
                "vent {}: message size: {}, took {}ms.",
                source_job_count, total_outgoing, request_duration
              );
            } else {
              println!("-- Failed to prepare input stream for taskid {:?}", taskid);
              println!("-- task details: {:?}", current_task);
              taskid = -1;
              ventilator.send(&Vec::new(), 0)?;
            }
          }
        } else {
          println!(
            "vent {:?}: worker {:?} received mock reply.",
            source_job_count, identity_str
          );
          ventilator.send("0", SNDMORE)?;
          ventilator.send(&Vec::new(), 0)?;
        }
        // Update this worker's metadata
        WorkerMetadata::record_dispatched(
          identity_str,
          service.id,
          taskid,
          self.backend_address.clone(),
        )?;
      } else {
        println!(
          "-- No such service in ventilator request: {:?}",
          service_name
        );
      }
      // Record that a task has been dispatched in the progress queue
      if let Some(dispatched_task) = dispatched_task_opt {
        server::push_progress_task(&progress_queue_arc, dispatched_task);
      }
      if let Some(limit_number) = job_limit {
        if source_job_count >= limit_number {
          println!(
            "vent {}: job limit reached, terminating Ventilator thread...",
            limit_number
          );
          break;
        }
      }
    }
    Ok(())
  }
}
