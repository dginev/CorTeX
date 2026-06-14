use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io;
use std::io::Write;
use std::ops::Deref;
use std::path::Path;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::dispatcher::server;
use crate::helpers;
use crate::helpers::{NewTaskMessage, TaskProgress, TaskReport, TaskStatus};
use crate::models::{Service, WorkerMetadataSender};

/// Specifies the binding and operation parameters for a ZMQ sink component
pub struct Sink {
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
  /// non-blocking handle to the background worker-metadata writer
  pub metadata: WorkerMetadataSender,
}

impl Sink {
  /// Starts a receiver/sink `Server` (ZMQ Pull), to accept processing responses.
  /// The sink shares state with other manager threads via queues for tasks in progress,
  /// as well as a queue for completed tasks pending persisting to disk.
  /// A job limit can be provided as a termination condition for the sink server.
  pub fn start(
    &self,
    services_arc: &Arc<Mutex<HashMap<String, Option<Service>>>>,
    progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress>>>,
    done_tx: &SyncSender<TaskReport>,
    job_limit: Option<usize>,
  ) -> Result<(), Box<dyn Error>> {
    // Ok, let's bind to a port and start broadcasting
    let context = zmq::Context::new();
    let sink = context.socket(zmq::PULL)?;
    let address = format!("tcp://*:{}", self.port);
    sink.bind(&address).unwrap();

    // Hard cap on a single result's on-disk size (config `dispatcher.max_result_bytes`, default
    // 2 GiB): a runaway worker must not fill `/data`.
    let max_result_bytes = crate::config::config().dispatcher.max_result_bytes;

    let mut sink_job_count: usize = 0;
    // Rate-limited logging for discarded replies (malformed envelopes, unknown task ids). A
    // sustained bad-reply flood must not turn per-message `stderr` writes into a throughput-DoS
    // (KNOWN_ISSUES D-11) — count, don't narrate.
    let mut discard_log = server::RateLimitedLog::new(Duration::from_secs(5));

    loop {
      let mut recv_msg = zmq::Message::new();
      let mut identity_msg = zmq::Message::new();
      let mut taskid_msg = zmq::Message::new();
      let mut service_msg = zmq::Message::new();

      // Read the reply envelope `[identity, service, taskid, ...data]`. A malformed / empty /
      // truncated reply (a worker crash mid-send, or a hostile post: no frames, id-only, etc.)
      // would otherwise desync the multipart framing of the *next* reply — the sink would
      // read the next reply's frames as this one's. So require `RCVMORE` after each header
      // frame: if the message ends early its frames are already fully consumed, and skipping
      // it leaves the next reply to parse cleanly. (Envelope robustness — torture-tested by
      // the bad-reply barrage.)
      sink.recv(&mut identity_msg, 0)?;
      let identity = identity_msg.as_str().unwrap_or("_worker_");
      if !sink.get_rcvmore().unwrap_or(false) {
        if let Some(n) = discard_log.record() {
          eprintln!(
            "-- sink: discarded {n} malformed reply(ies) [latest: truncated after identity {identity:?}] (rate-limited)"
          );
        }
        continue;
      }
      sink.recv(&mut service_msg, 0)?;
      let service_name = service_msg.as_str().unwrap_or("_unknown_");
      if !sink.get_rcvmore().unwrap_or(false) {
        if let Some(n) = discard_log.record() {
          eprintln!(
            "-- sink: discarded {n} malformed reply(ies) [latest: no taskid, worker {identity:?}] (rate-limited)"
          );
        }
        continue;
      }
      sink.recv(&mut taskid_msg, 0)?;
      let taskid_str = taskid_msg.as_str().unwrap_or("-1");
      let taskid = taskid_str.parse::<i64>().unwrap_or(-1);
      if !sink.get_rcvmore().unwrap_or(false) {
        // A well-formed reply is `[identity, service, taskid, <≥1 data frame>]` — even an empty
        // result carries one empty data frame (the worker's `respond_to_cortex` always sends one).
        // No frame after the taskid means a truncated / malformed reply whose frames are *already
        // fully consumed*. We must `continue` WITHOUT draining: the drain loops below `recv()`
        // first and only then check `RCVMORE`, so on an already-complete message that first
        // `recv()` would cross the message boundary and swallow the *entire next reply* — a
        // real worker result read as this one's payload and lost, stranding its task
        // `Queued` (KNOWN_ISSUES D-12). This is the taskid-frame analogue of the
        // identity/service `RCVMORE` guards above; it completes the envelope hardening
        // (D-4) for the no-data-frame case.
        if let Some(n) = discard_log.record() {
          eprintln!(
            "-- sink: discarded {n} malformed reply(ies) [latest: no data frame after taskid {taskid:?}, worker {identity:?}] (rate-limited)"
          );
        }
        continue;
      }

      // We have a job, count it
      sink_job_count += 1;
      let mut total_incoming = 0;
      let request_time = chrono::Utc::now();
      println!(
        "sink {sink_job_count:?}: incoming result for {service_name:?}, worker {identity:?}, taskid: {taskid}");

      if let Some(task_progress) = server::pop_progress_task(progress_queue_arc, taskid) {
        let task = task_progress.task;
        match server::get_service(service_name, services_arc) {
          None => {
            return Err(Box::new(io::Error::other(
              "TODO: Server::get_service found nothing.",
            )));
          }, // TODO: Handle errors
          Some(service) => {
            if service.id == task.service_id {
              // println!("Task and Service match up.");
              if service.id == 1 {
                // No payload needed for init
                sink.recv(&mut recv_msg, 0)?;
                let done_report = TaskReport {
                  task: task.clone(),
                  status: TaskStatus::NoProblem,
                  messages: Vec::new(),
                };
                server::send_done(done_tx, done_report);
              } else {
                // Receive the rest of the input in the correct file
                match Path::new(&task.entry.clone()).parent() {
                  None => {
                    println!("-- Error TODO: Path::new(&task.entry).parent() failed.");
                  },
                  Some(recv_dir) => {
                    match recv_dir.to_str() {
                      None => {
                        println!("-- Error TODO: recv_dir.to_str() failed");
                      },
                      Some(recv_dir_str) => {
                        let recv_dir_string = recv_dir_str.to_string();
                        let recv_pathname = recv_dir_string + "/" + &service.name + ".zip";
                        let recv_path = Path::new(&recv_pathname);
                        // println!("Will write to {:?}", recv_path);
                        let mut oversized = false;
                        {
                          // Explicitly scope file, so that we drop it the moment we are done
                          // writing.
                          let mut file = match File::create(recv_path) {
                            Ok(f) => f,
                            Err(e) => {
                              println!("-- Error TODO: File::create(recv_path): {e:?}");
                              continue;
                            },
                          };
                          while sink.recv(&mut recv_msg, 0).is_ok() {
                            // Hard cap: a result over `max_result_bytes` must not be written (disk
                            // protection). Stop writing, drain the rest of the message
                            // frame-by-frame (bounded memory — never
                            // the whole reply resident) to resync the socket,
                            // and reject the task below.
                            if total_incoming + recv_msg.len() > max_result_bytes {
                              oversized = true;
                              while sink.get_rcvmore().unwrap_or(false) {
                                let _ = sink.recv(&mut recv_msg, 0);
                              }
                              break;
                            }
                            match file.write(recv_msg.deref()) {
                              Ok(written_bytes) => total_incoming += written_bytes,
                              Err(e) => {
                                println!(
                                  "-- Error TODO: file.write(recv_msg.deref()) failed: {e:?}"
                                );
                                break;
                              },
                            };
                            match sink.get_rcvmore() {
                              Ok(true) => {}, // keep receiving
                              _ => break,     /* println!("Error TODO: sink.get_rcvmore failed:
                                                * {:?}", e); */
                            };
                          }
                          drop(file);
                        }
                        // Then mark the task done. This can be in a new thread later on.
                        let done_report = if oversized {
                          // Reject an over-cap result: remove the partial file + mark Invalid, so a
                          // runaway worker can't fill /data and the task is transparently failed.
                          std::fs::remove_file(recv_path).ok();
                          eprintln!(
                            "-- sink: result for task {taskid} exceeded the {max_result_bytes}-byte cap — rejected (Invalid)"
                          );
                          TaskReport {
                            task,
                            status: TaskStatus::Invalid,
                            messages: vec![NewTaskMessage::new(
                              taskid,
                              "invalid",
                              "cortex".to_string(),
                              "result_too_large".to_string(),
                              format!(
                                "worker result exceeded the {max_result_bytes}-byte hard cap"
                              ),
                            )],
                          }
                        } else {
                          helpers::generate_report(task, recv_path)
                        };
                        server::send_done(done_tx, done_report);
                      },
                    }
                  },
                }
              }
              // Also update worker metadata (non-blocking enqueue to the background writer)
              self
                .metadata
                .received(identity.to_string(), service.id, taskid);
            } else {
              // Otherwise just discard the rest of the message
              if let Some(n) = discard_log.record() {
                println!(
                  "-- sink: discarded {n} reply(ies) [latest: service-id mismatch — requested {:?}, task is {:?}, task {taskid:?}] (rate-limited)",
                  service.id, task.service_id
                );
              }
              while sink.recv(&mut recv_msg, 0).is_ok() {
                if !sink.get_rcvmore()? {
                  break;
                }
              }
            }
          },
        };
      } else {
        // No such task, just discard the next message from the sink
        if let Some(n) = discard_log.record() {
          println!(
            "-- sink: discarded {n} reply(ies) for unknown task id(s) [latest: {taskid:?}] (rate-limited)"
          );
        }
        while sink.recv(&mut recv_msg, 0).is_ok() {
          if !sink.get_rcvmore()? {
            break;
          }
        }
      }
      let responded_time = chrono::Utc::now();
      let request_duration = (responded_time - request_time).num_milliseconds();
      println!("sink {sink_job_count}: message size: {total_incoming}, took {request_duration}ms.");
      if let Some(limit_number) = job_limit {
        if sink_job_count >= limit_number {
          println!("sink {limit_number}: job limit reached, terminating Sink thread...");
          break;
        }
      }
    }
    Ok(())
  }
}
