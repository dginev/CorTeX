extern crate tempfile;
extern crate zmq;

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::Mutex;
use time;

use backend;
use dispatcher::server;
use helpers;
use helpers::{NewTaskMessage, TaskProgress, TaskReport, TaskStatus};
use models::Service;
use zmq::{Error, SNDMORE};

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
    services_arc: Arc<Mutex<HashMap<String, Option<Service>>>>,
    progress_queue_arc: Arc<Mutex<HashMap<i64, TaskProgress>>>,
    done_queue_arc: Arc<Mutex<Vec<TaskReport>>>,
    job_limit: Option<usize>,
  ) -> Result<(), Error>
  {
    // We have a Ventilator-exclusive "queues" stack for tasks to be dispatched
    let mut queues: HashMap<String, Vec<TaskProgress>> = HashMap::new();
    // Assuming this is the only And tidy up the postgres tasks:
    let backend = backend::from_address(&self.backend_address);
    backend.clear_limbo_tasks().unwrap();
    // Ok, let's bind to a port and start broadcasting
    let context = zmq::Context::new();
    let ventilator = context.socket(zmq::ROUTER).unwrap();
    let port_str = self.port.to_string();
    let address = "tcp://*:".to_string() + &port_str;
    assert!(ventilator.bind(&address).is_ok());
    let mut source_job_count: usize = 0;

    loop {
      let mut msg = zmq::Message::new().unwrap();
      let mut identity = zmq::Message::new().unwrap();
      ventilator.recv(&mut identity, 0).unwrap();
      ventilator.recv(&mut msg, 0).unwrap();
      let service_name = msg.as_str().unwrap().to_string();
      // println!("Task requested for service: {}", service_name.clone());
      let request_time = time::get_time();
      source_job_count += 1;

      let mut dispatched_task: Option<TaskProgress> = None;
      match server::get_sync_service(&service_name, &services_arc, &backend) {
        None => {},
        Some(service) => {
          if !queues.contains_key(&service_name) {
            queues.insert(service_name.clone(), Vec::new());
          }
          let mut task_queue: &mut Vec<TaskProgress> = queues.get_mut(&service_name).unwrap();
          if task_queue.is_empty() {
            // Refetch a new batch of tasks
            let now = time::get_time().sec;
            task_queue.extend(
              backend
                .fetch_tasks(&service, self.queue_size)
                .unwrap()
                .into_iter()
                .map(|task| TaskProgress {
                  task: task,
                  created_at: now,
                  retries: 0,
                }),
            );

            // This is a good time to also take care that none of the old tasks are dead in the
            // progress queue since the re-fetch happens infrequently, and directly
            // implies the progress queue will grow
            let expired_tasks = server::timeout_progress_tasks(&progress_queue_arc);
            for expired_t in expired_tasks {
              if expired_t.retries > 1 {
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
          if let Some(current_task_progress) = task_queue.pop() {
            dispatched_task = Some(current_task_progress.clone());

            let current_task = current_task_progress.task;
            let taskid = current_task.id;
            let serviceid = current_task.service_id;

            ventilator.send_msg(identity, SNDMORE).unwrap();
            ventilator.send_str(&taskid.to_string(), SNDMORE).unwrap();
            if serviceid == 1 {
              // No payload needed for init
              ventilator.send(&[], 0).unwrap();
            } else {
              // Regular services fetch the task payload and transfer it to the worker
              let file_opt = helpers::prepare_input_stream(&current_task);
              if file_opt.is_ok() {
                let mut file = file_opt.unwrap();
                let mut total_outgoing: usize = 0;
                loop {
                  // Stream input data via zmq
                  let mut data = vec![0; self.message_size];
                  let size = file.read(&mut data).unwrap();
                  total_outgoing += size;
                  data.truncate(size);

                  if size < self.message_size {
                    // If exhausted, send the last frame
                    ventilator.send(&data, 0).unwrap();
                    // And terminate
                    break;
                  } else {
                    // If more to go, send the frame and indicate there's more to come
                    ventilator.send(&data, SNDMORE).unwrap();
                  }
                }
                let responded_time = time::get_time();
                let request_duration = (responded_time - request_time).num_milliseconds();
                println!(
                  "Source job {}, message size: {}, took {}ms.",
                  source_job_count, total_outgoing, request_duration
                );
              } else {
                // TODO: smart handling of failures
                ventilator.send(&[], 0).unwrap();
              }
            }
          }
        },
      };
      // Record that a task has been dispatched in the progress queue
      if dispatched_task.is_some() {
        server::push_progress_task(&progress_queue_arc, dispatched_task.unwrap());
      }
      if job_limit.is_some() && (source_job_count >= job_limit.unwrap()) {
        println!(
          "Manager job limit of {:?} reached, terminating Ventilator thread...",
          job_limit.unwrap()
        );
        break;
      }
    }
    Ok(())
  }
}
