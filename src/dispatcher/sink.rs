use std::error::Error;
use std::fs::File;
use std::io;
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::config::config;
use crate::dispatcher::server;
use crate::helpers;
use crate::helpers::{NewTaskMessage, TaskReport, TaskStatus};
use crate::models::{Task, WorkerMetadataSender};

/// Capacity of each archive-writer's command channel. A small bound keeps resident memory at
/// O(chunk) per writer (a handful of `message_size` chunks at most) while letting a writer stay a
/// little ahead of the receive loop; a full channel simply makes the receive loop wait, which is
/// the correct backpressure when the disk cannot keep up.
const SINK_WRITER_CHANNEL_CAPACITY: usize = 64;

/// A unit of work streamed from the sink's receive loop to an archive-writer thread (dispatcher
/// rationalization phase 3 — D-7 sink fan-out). The receive loop owns the ZMQ socket and reads each
/// result's frames; rather than block on the `/data` write itself, it hands a writer the task, then
/// the received chunks, then a commit/reject — so receiving the next result is no longer hostage to
/// the current one's disk latency.
enum WriteCommand {
  /// Begin a new result file for `task` at `recv_path`.
  Begin {
    /// the task this result belongs to (moved to the writer, which reports it on commit)
    task: Box<Task>,
    /// `<entry-dir>/<service>.zip` — where the result archive is written
    recv_path: PathBuf,
  },
  /// Append a received data chunk to the in-progress result file. The bytes are owned/moved here
  /// and dropped right after the write, so the per-job footprint stays O(chunk), never the whole
  /// archive.
  Chunk(Vec<u8>),
  /// Close the in-progress file, parse its `cortex.log` into a report, and hand it to finalize.
  Commit,
  /// Reject the in-progress (over-cap) result: remove the partial file and report the task
  /// `Invalid` (`result_too_large`).
  Reject {
    /// the configured cap, for the rejection message
    max_result_bytes: usize,
  },
}

/// One archive-writer thread. It drains [`WriteCommand`]s for the tasks the receive loop assigns
/// it, performing the blocking `/data` write + `cortex.log` parse + finalize hand-off **off** the
/// receive loop. Per-task ordering is guaranteed because the receive loop sends a task's
/// `Begin → Chunk* → Commit|Reject` contiguously to one writer's FIFO channel; fan-out is across
/// *different* tasks. The writer exits when its channel disconnects (the sink shutting down on
/// `job_limit`), after draining any buffered commands (so the last result's `Commit` still reaches
/// finalize — loss-free). A panic here is surfaced by the receive loop (its liveness check / a
/// send error) → the manager aborts the dispatcher (fail-fast preserved).
fn run_writer(rx: &Receiver<WriteCommand>, done_tx: &SyncSender<TaskReport>) {
  // In-progress job state: the task, its result path, and the open file (`None` if create/write
  // failed — then the result is abandoned and the task is left `Queued` for the reaper, matching
  // the legacy inline behavior but without the socket desync the old early `continue` caused).
  let mut current: Option<(Task, PathBuf, Option<File>)> = None;
  while let Ok(cmd) = rx.recv() {
    match cmd {
      WriteCommand::Begin { task, recv_path } => {
        let file = match File::create(&recv_path) {
          Ok(f) => Some(f),
          Err(e) => {
            eprintln!(
              "-- sink writer: File::create({recv_path:?}) failed ({e:?}); task {} left Queued for the reaper",
              task.id
            );
            None
          },
        };
        current = Some((*task, recv_path, file));
      },
      WriteCommand::Chunk(bytes) => {
        if let Some((_, _, slot)) = current.as_mut() {
          if let Some(file) = slot {
            if let Err(e) = file.write_all(&bytes) {
              eprintln!("-- sink writer: write to result file failed ({e:?}); abandoning result");
              // Stop writing; the (now-partial) file makes the result untrustworthy, so Commit
              // skips the report and the task is recovered by the reaper.
              *slot = None;
            }
          }
        }
        // `bytes` dropped here — tight deallocation, O(chunk) resident.
      },
      WriteCommand::Commit => {
        if let Some((task, recv_path, file)) = current.take() {
          match file {
            Some(f) => {
              drop(f); // flush + close before generate_report reads the archive back
              let report = helpers::generate_report(task, &recv_path);
              server::send_done(done_tx, report);
            },
            None => {
              eprintln!(
                "-- sink writer: no result file for task {} (create/write failed); skipping report (reaper will recover)",
                task.id
              );
            },
          }
        }
      },
      WriteCommand::Reject { max_result_bytes } => {
        if let Some((task, recv_path, file)) = current.take() {
          drop(file);
          std::fs::remove_file(&recv_path).ok();
          let taskid = task.id;
          eprintln!(
            "-- sink: result for task {taskid} exceeded the {max_result_bytes}-byte cap — rejected (Invalid)"
          );
          let report = TaskReport {
            task,
            status: TaskStatus::Invalid,
            messages: vec![NewTaskMessage::new(
              taskid,
              "invalid",
              "cortex".to_string(),
              "result_too_large".to_string(),
              format!("worker result exceeded the {max_result_bytes}-byte hard cap"),
            )],
          };
          server::send_done(done_tx, report);
        }
      },
    }
  }
}

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
  ///
  /// The slow part of each result — the blocking `/data` archive write and `cortex.log` parse — is
  /// fanned out to a pool of [`run_writer`] threads (size `dispatcher.sink_writers`), so the single
  /// ZMQ-PULL receive loop only reads frames + hands them off and is never hostage to disk latency
  /// (dispatcher rationalization phase 3, closes D-7). Every framing / size-cap / discard invariant
  /// of the receive path is unchanged; only *where* the bytes get written moved off-thread.
  pub fn start(
    &self,
    services_arc: &Arc<server::ServiceCache>,
    progress_queue_arc: &Arc<server::InFlightSet>,
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
    let max_result_bytes = config().dispatcher.max_result_bytes;

    // Spin up the archive-writer pool (D-7 fan-out). Each writer owns a bounded command channel and
    // a clone of the done sender; the receive loop keeps the senders + handles to stream work and
    // to detect a writer death.
    let num_writers = config().dispatcher.sink_writers.max(1);
    let mut writer_txs: Vec<SyncSender<WriteCommand>> = Vec::with_capacity(num_writers);
    let mut writer_handles: Vec<JoinHandle<()>> = Vec::with_capacity(num_writers);
    for _ in 0..num_writers {
      let (tx, rx) = sync_channel::<WriteCommand>(SINK_WRITER_CHANNEL_CAPACITY);
      let writer_done_tx = done_tx.clone();
      writer_txs.push(tx);
      writer_handles.push(thread::spawn(move || run_writer(&rx, &writer_done_tx)));
    }
    let mut next_writer = 0_usize;

    let mut sink_job_count: usize = 0;
    // Rate-limited logging for discarded replies (malformed envelopes, unknown task ids). A
    // sustained bad-reply flood must not turn per-message `stderr` writes into a throughput-DoS
    // (KNOWN_ISSUES D-11) — count, don't narrate.
    let mut discard_log = server::RateLimitedLog::new(Duration::from_secs(5));

    loop {
      // Fail-fast: a writer thread that died (e.g. a panic in `generate_report`) must bring down
      // the dispatcher, not leave a half-working pipeline. Detect it promptly here (and again
      // via a send error below) so the manager aborts for a supervised restart (CLAUDE.md
      // fail-fast).
      if let Some(idx) = writer_handles.iter().position(JoinHandle::is_finished) {
        return Err(Box::new(io::Error::other(format!(
          "sink writer thread {idx} died unexpectedly; aborting for a supervised restart"
        ))));
      }

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

      if let Some(task_progress) = progress_queue_arc.remove(taskid) {
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
                // No payload needed for init — read its single (empty) data frame and report
                // inline; no disk write, so no writer involved.
                sink.recv(&mut recv_msg, 0)?;
                let done_report = TaskReport {
                  task,
                  status: TaskStatus::NoProblem,
                  messages: Vec::new(),
                };
                server::send_done(done_tx, done_report);
              } else {
                // Derive the result path up front (cheap, no I/O): `<entry-dir>/<service>.zip`. We
                // do this *before* receiving the data frames so that even a path
                // failure still drains the socket below (no desync) — only the
                // write target differs.
                let recv_path_opt = Path::new(&task.entry)
                  .parent()
                  .and_then(Path::to_str)
                  .map(|dir| PathBuf::from(format!("{dir}/{}.zip", service.name)));

                // Assign this task to a writer (round-robin) and open the stream. `Begin` moves the
                // task to the writer; a send error means that writer died → fail-fast.
                let widx = next_writer;
                next_writer = (next_writer + 1) % num_writers;
                let streaming = match recv_path_opt {
                  Some(recv_path) => {
                    if writer_txs[widx]
                      .send(WriteCommand::Begin {
                        task: Box::new(task),
                        recv_path,
                      })
                      .is_err()
                    {
                      return Err(Box::new(io::Error::other(
                        "sink writer thread died while beginning a result; aborting",
                      )));
                    }
                    true
                  },
                  None => {
                    eprintln!(
                      "-- sink: could not derive a result path for task entry {:?}; draining + leaving Queued",
                      task.entry
                    );
                    false
                  },
                };

                // Receive the rest of the input. Stream each data frame to the writer (or just
                // drain it, if we have no valid target), enforcing the hard size
                // cap on the receive side (the receive loop knows the running byte
                // total as frames arrive).
                let mut oversized = false;
                while sink.recv(&mut recv_msg, 0).is_ok() {
                  // Hard cap: a result over `max_result_bytes` must not be written (disk
                  // protection). Stop forwarding, drain the rest of the message frame-by-frame
                  // (bounded memory — never the whole reply resident) to resync the socket, and
                  // reject the task on the writer side.
                  if total_incoming + recv_msg.len() > max_result_bytes {
                    oversized = true;
                    while sink.get_rcvmore().unwrap_or(false) {
                      let _ = sink.recv(&mut recv_msg, 0);
                    }
                    break;
                  }
                  if streaming
                    && writer_txs[widx]
                      .send(WriteCommand::Chunk(recv_msg.deref().to_vec()))
                      .is_err()
                  {
                    return Err(Box::new(io::Error::other(
                      "sink writer thread died while streaming a result; aborting",
                    )));
                  }
                  total_incoming += recv_msg.len();
                  match sink.get_rcvmore() {
                    Ok(true) => {}, // keep receiving
                    _ => break,     /* println!("Error TODO: sink.get_rcvmore failed:
                                      * {:?}", e); */
                  };
                }

                // Close the stream: commit the written result (→ generate_report → finalize) or
                // reject the over-cap one. A send error means the writer died → fail-fast.
                if streaming {
                  let cmd = if oversized {
                    WriteCommand::Reject { max_result_bytes }
                  } else {
                    WriteCommand::Commit
                  };
                  if writer_txs[widx].send(cmd).is_err() {
                    return Err(Box::new(io::Error::other(
                      "sink writer thread died while finishing a result; aborting",
                    )));
                  }
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
      println!(
        "sink {sink_job_count}: received {total_incoming} bytes, recv took {request_duration}ms."
      );
      if let Some(limit_number) = job_limit {
        if sink_job_count >= limit_number {
          println!("sink {limit_number}: job limit reached, terminating Sink thread...");
          break;
        }
      }
    }

    // Shut the writer pool down cleanly: dropping the senders disconnects each writer's channel; it
    // drains any buffered commands first (so the final `Commit` still reaches finalize —
    // loss-free), then exits. Join them before returning so a `job_limit` run does not race
    // teardown.
    drop(writer_txs);
    for handle in writer_handles {
      let _ = handle.join();
    }
    Ok(())
  }
}
