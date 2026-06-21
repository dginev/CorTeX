use std::error::Error;
use std::fs::File;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tracing::{info, trace, warn};
use zeromq::{PullSocket, Socket, SocketRecv, ZmqMessage};

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

/// How often a **bounded** (`job_limit = Some(_)`) sink wakes from a blocked `recv()` to re-check
/// the shared completion signal, so it can terminate once the ventilator has finished dispatching
/// and the in-flight set has drained (KNOWN_ISSUES D-5). `recv()` is cancel-safe — its `FairQueue`
/// buffers messages behind a `Mutex` and the `recv` future holds no message — so a timed-out poll
/// loses nothing. Perpetual production runs (`job_limit = None`) never use this; they block
/// plainly.
const SINK_TERMINATION_POLL: Duration = Duration::from_millis(250);

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
            warn!(
              task_id = task.id,
              path = ?recv_path,
              error = ?e,
              "sink writer: File::create failed; task left Queued for the reaper"
            );
            None
          },
        };
        current = Some((*task, recv_path, file));
      },
      WriteCommand::Chunk(bytes) => {
        if let Some((task, _, slot)) = current.as_mut()
          && let Some(file) = slot
          && let Err(e) = file.write_all(&bytes)
        {
          warn!(
            task_id = task.id,
            error = ?e,
            "sink writer: write to result file failed; abandoning result"
          );
          // Stop writing; the (now-partial) file makes the result untrustworthy, so Commit
          // skips the report and the task is recovered by the reaper.
          *slot = None;
        }
        // `bytes` dropped here — tight deallocation, O(chunk) resident.
      },
      WriteCommand::Commit => {
        if let Some((task, recv_path, file)) = current.take() {
          match file {
            Some(f) => {
              drop(f); // flush + close before generate_report reads the archive back
              let task_id = task.id;
              match helpers::generate_report(task, &recv_path) {
                Some(report) => server::send_done(done_tx, report),
                // Unreadable / empty result archive (0-byte, truncated, no `cortex.log`) — an
                // infrastructure failure, not a verdict. Skip finalizing it (exactly like the
                // no-file case below) so the lease reaper recovers the task: retry, then
                // dead-letter with a message after MAX_DISPATCH_RETRIES — never a
                // silent terminal Fatal (D-18). Drop the stale 0-byte artifact so a
                // retry writes a clean file.
                None => {
                  std::fs::remove_file(&recv_path).ok();
                  warn!(
                    task_id,
                    "sink writer: empty/unreadable result archive; skipping report so the reaper recovers the task (D-18)"
                  );
                },
              }
            },
            None => {
              warn!(
                task_id = task.id,
                "sink writer: no result file (create/write failed); skipping report (reaper will recover)"
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
          warn!(
            task_id = taskid,
            max_result_bytes, "sink: result exceeded byte cap — rejected (Invalid)"
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

/// The parsed header of a worker reply envelope `[identity, service, taskid, <≥1 data frame>]`.
/// The data frames (index 3..) are read directly off the [`ZmqMessage`] by the caller.
struct ReplyHeader {
  /// the worker's ZMQ identity (for metadata + logs)
  identity: String,
  /// the service name the worker claims to have run
  service: String,
  /// the task id this result is for (`-1` if the frame wasn't a valid integer)
  taskid: i64,
}

/// Validates + parses the reply envelope from one **atomically-received** `zeromq` message. Because
/// `zeromq` delivers the whole multipart message at once (unlike libzmq's frame-by-frame `recv` +
/// `RCVMORE`), a short/truncated/malformed reply is simply a message with too few frames — dropping
/// it cannot desync the *next* reply, so the entire libzmq desync bug class (KNOWN_ISSUES D-4/D-12)
/// is structurally gone and a frame-count check replaces the four chained `RCVMORE` guards. A valid
/// reply has **≥4** frames (`identity, service, taskid`, then ≥1 data frame — even an empty result
/// carries one empty data frame); fewer ⇒ `Err(reason)` for the rate-limited discard log. Pure
/// (no I/O), so it is unit-tested directly.
fn parse_reply_envelope(msg: &ZmqMessage) -> Result<ReplyHeader, &'static str> {
  if msg.len() < 4 {
    return Err("truncated reply: fewer than 4 frames (no data frame after taskid)");
  }
  let frame_str = |i: usize, default: &str| -> String {
    msg
      .get(i)
      .and_then(|f| std::str::from_utf8(f).ok())
      .unwrap_or(default)
      .to_string()
  };
  Ok(ReplyHeader {
    identity: frame_str(0, "_worker_"),
    service: frame_str(1, "_unknown_"),
    taskid: frame_str(2, "-1").parse::<i64>().unwrap_or(-1),
  })
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
  /// **Phase 5a (transport swap):** the receive loop now runs on the pure-Rust async `zeromq`
  /// transport, driven by a current-thread tokio runtime this sink thread owns. `zeromq` delivers
  /// each result as **one atomic multipart [`ZmqMessage`]**, which retires the libzmq
  /// frame-by-frame desync bug class (D-4/D-12): a malformed reply is just a message with too few
  /// frames, discarded by dropping it — it can no longer swallow the next reply. The blocking
  /// `/data` write + `cortex.log` parse stay fanned out to the **unchanged** phase-3
  /// [`run_writer`] std-thread pool (size `dispatcher.sink_writers`); the blocking channel sends
  /// to it (and to the done channel) from this single-task runtime are the correct backpressure —
  /// when the disk or DB can't keep up, the loop stops receiving and the workers back off.
  /// **Note:** `zeromq` exposes no TCP-keepalive knob, so `dispatcher.tcp_keepalive_idle_seconds`
  /// no longer applies to the sink PULL socket; keepalive was stability-only (remote-worker NAT
  /// mappings) and the lease reaper remains the correctness net.
  pub fn start(
    &self,
    services_arc: &Arc<server::ServiceCache>,
    sandboxes_arc: &Arc<server::SandboxCache>,
    progress_queue_arc: &Arc<server::InFlightSet>,
    done_tx: &SyncSender<TaskReport>,
    job_limit: Option<usize>,
    dispatch_complete: &Arc<AtomicBool>,
  ) -> Result<(), Box<dyn Error>> {
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

    let address = format!("tcp://0.0.0.0:{}", self.port);

    // A current-thread runtime is right: there is one PULL socket and one receive loop, and
    // blocking it on writer/done-channel backpressure is exactly the flow control we want (stop
    // receiving when the pipeline downstream is full). The writer pool threads drain
    // independently, so a blocking send here always makes progress.
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()?;

    let recv_result: Result<(), Box<dyn Error>> = runtime.block_on(async {
      // Retry the bind briefly (mirrors the ventilator) so a port handover from a
      // restarting dispatcher doesn't crash-loop the sink on EADDRINUSE.
      let mut pull = {
        const BIND_ATTEMPTS: u32 = 15;
        let mut attempt = 1u32;
        loop {
          let mut p = PullSocket::new();
          match p.bind(&address).await {
            Ok(_) => break p,
            Err(e) if attempt < BIND_ATTEMPTS => {
              warn!(
                address = %address,
                attempt,
                max_attempts = BIND_ATTEMPTS,
                error = %e,
                "sink: bind failed; retrying in 500ms (port handover from a restarting dispatcher?)"
              );
              tokio::time::sleep(Duration::from_millis(500)).await;
              attempt += 1;
            },
            Err(e) => {
              return Err(
                io::Error::other(format!(
                  "sink: zeromq bind {address} failed after {BIND_ATTEMPTS} attempts: {e}"
                ))
                .into(),
              );
            },
          }
        }
      };

      let mut sink_job_count: usize = 0;
      // Rate-limited logging for discarded replies (malformed envelopes, unknown task ids). A
      // sustained bad-reply flood must not turn per-message `stderr` writes into a throughput-DoS
      // (KNOWN_ISSUES D-11) — count, don't narrate.
      let mut discard_log = server::RateLimitedLog::new(Duration::from_secs(5));
      // A bounded run (`job_limit = Some(_)`) must be able to stop once the ventilator signals
      // dispatching is done and the in-flight set has drained; a perpetual production run
      // (`job_limit = None`) blocks plainly forever (terminated only by the manager's supervised
      // abort), exactly as before.
      let bounded = job_limit.is_some();

      loop {
        // Fail-fast: a writer thread that died (e.g. a panic in `generate_report`) must bring down
        // the dispatcher, not leave a half-working pipeline. Detect it promptly here (and again via
        // a send error below) so the manager aborts for a supervised restart (CLAUDE.md fail-fast).
        if let Some(idx) = writer_handles.iter().position(JoinHandle::is_finished) {
          return Err(Box::<dyn Error>::from(io::Error::other(format!(
            "sink writer thread {idx} died unexpectedly; aborting for a supervised restart"
          ))));
        }

        // One atomic multipart message: `[identity, service, taskid, ...data]`. No RCVMORE dance —
        // the whole message arrives at once, so a short/malformed reply is just a short frame
        // vector (handled by `parse_reply_envelope`), never a desync of the next reply.
        //
        // Bounded run: poll the recv so we wake to re-check the shared completion signal even when
        // no result is arriving (e.g. the ventilator finished and the last result already landed
        // before `dispatch_complete` flipped). The timeout only ever fires while idle — i.e. with
        // nothing mid-delivery — and `recv()` is cancel-safe, so dropping the timed-out future
        // loses no message (KNOWN_ISSUES D-5).
        let recv_outcome = if bounded {
          match tokio::time::timeout(SINK_TERMINATION_POLL, pull.recv()).await {
            Ok(inner) => inner,
            Err(_elapsed) => {
              if dispatch_complete.load(Ordering::Acquire) && progress_queue_arc.is_empty() {
                info!("sink: dispatching complete and in-flight drained; terminating sink");
                break;
              }
              continue;
            },
          }
        } else {
          pull.recv().await
        };
        let msg = match recv_outcome {
          Ok(m) => m,
          Err(e) => {
            return Err(Box::<dyn Error>::from(io::Error::other(format!(
              "sink: zeromq recv failed: {e}"
            ))));
          },
        };

        let header = match parse_reply_envelope(&msg) {
          Ok(h) => h,
          Err(reason) => {
            if let Some(n) = discard_log.record() {
              warn!(
                discarded = n,
                reason = %reason,
                "sink: discarded malformed reply(ies) (rate-limited)"
              );
            }
            continue;
          },
        };

        sink_job_count += 1;
        let taskid = header.taskid;
        let request_time = chrono::Utc::now();
        trace!(
          job = sink_job_count,
          service = ?header.service,
          worker = ?header.identity,
          task_id = taskid,
          "sink: incoming result"
        );

        if let Some(task_progress) = progress_queue_arc.remove(taskid) {
          let task = task_progress.task;
          match server::get_service(&header.service, services_arc) {
            None => {
              return Err(Box::<dyn Error>::from(io::Error::other(
                "sink: get_service found nothing",
              )));
            },
            Some(service) => {
              if service.id == task.service_id {
                if service.id == 1 {
                  // No payload needed for init — its (empty) data frames are ignored; report
                  // inline, no disk write, so no writer involved.
                  server::send_done(
                    done_tx,
                    TaskReport {
                      task,
                      status: TaskStatus::NoProblem,
                      messages: Vec::new(),
                    },
                  );
                } else {
                  // Derive the result path (cheap, no I/O): `<entry-dir>/<service>.zip`, or a
                  // sandbox-scoped name when this task's corpus is a sandbox (lock-free cache read,
                  // memoised by the ventilator on dispatch — F-6).
                  let sandbox_id = server::get_sandbox_id(task.corpus_id, sandboxes_arc);
                  let recv_path_opt =
                    helpers::result_archive_path(&task.entry, &service.name, sandbox_id);

                  // Hard size cap (disk protection): sum the data frames; an over-cap result is
                  // rejected (Invalid) without being written. The whole multipart message is
                  // already received (ZMQ delivers it atomically — true of libzmq
                  // too), so this is the same disk-protection guarantee as the
                  // streamed check, just computed up front.
                  let data_bytes: usize = msg.iter().skip(3).map(|f| f.len()).sum();
                  let oversized = data_bytes > max_result_bytes;

                  let widx = next_writer;
                  next_writer = (next_writer + 1) % num_writers;
                  match recv_path_opt {
                    Some(recv_path) => {
                      // `Begin` moves the task to the writer; a send error means that writer died →
                      // fail-fast.
                      if writer_txs[widx]
                        .send(WriteCommand::Begin {
                          task: Box::new(task),
                          recv_path,
                        })
                        .is_err()
                      {
                        return Err(Box::<dyn Error>::from(io::Error::other(
                          "sink writer thread died while beginning a result; aborting",
                        )));
                      }
                      if oversized {
                        if writer_txs[widx]
                          .send(WriteCommand::Reject { max_result_bytes })
                          .is_err()
                        {
                          return Err(Box::<dyn Error>::from(io::Error::other(
                            "sink writer thread died while rejecting a result; aborting",
                          )));
                        }
                      } else {
                        for frame in msg.iter().skip(3) {
                          if writer_txs[widx]
                            .send(WriteCommand::Chunk(frame.to_vec()))
                            .is_err()
                          {
                            return Err(Box::<dyn Error>::from(io::Error::other(
                              "sink writer thread died while streaming a result; aborting",
                            )));
                          }
                        }
                        if writer_txs[widx].send(WriteCommand::Commit).is_err() {
                          return Err(Box::<dyn Error>::from(io::Error::other(
                            "sink writer thread died while finishing a result; aborting",
                          )));
                        }
                      }
                    },
                    None => {
                      warn!(
                        task_id = task.id,
                        entry = ?task.entry,
                        "sink: could not derive a result path; leaving Queued"
                      );
                    },
                  }
                }
                // Update worker metadata (non-blocking enqueue to the background writer).
                self
                  .metadata
                  .received(header.identity.clone(), service.id, taskid);
              } else if let Some(n) = discard_log.record() {
                warn!(
                  discarded = n,
                  service = ?header.service,
                  service_id = service.id,
                  task_service_id = task.service_id,
                  task_id = taskid,
                  "sink: discarded reply(ies) [service-id mismatch] (rate-limited)"
                );
              }
            },
          };
        } else if let Some(n) = discard_log.record() {
          // No such in-flight task — drop the message (already fully received; nothing to drain).
          warn!(
            discarded = n,
            task_id = taskid,
            "sink: discarded reply(ies) for unknown task id(s) (rate-limited)"
          );
        }

        let request_duration = (chrono::Utc::now() - request_time).num_milliseconds();
        let total_incoming: usize = msg.iter().skip(3).map(|f| f.len()).sum();
        trace!(
          job = sink_job_count,
          bytes = total_incoming,
          recv_ms = request_duration,
          "sink: received result"
        );

        // Bounded run: terminate once the ventilator has signalled dispatching is done AND every
        // dispatched task has come back (the in-flight set is empty) — the shared completion
        // condition that replaced the old per-thread `sink_job_count >= limit` (KNOWN_ISSUES D-5).
        // Checked here right after a result lands (so the run ends promptly when the last one
        // arrives) and on the recv-timeout poll above (so a sink already blocked when the signal
        // flips still wakes to notice).
        if bounded && dispatch_complete.load(Ordering::Acquire) && progress_queue_arc.is_empty() {
          info!("sink: dispatching complete and in-flight drained; terminating sink");
          break;
        }
      }
      Ok(())
    });

    // Shut the writer pool down cleanly: dropping the senders disconnects each writer's channel; it
    // drains any buffered commands first (so the final `Commit` still reaches finalize —
    // loss-free), then exits. Join them before returning so a `job_limit` run does not race
    // teardown.
    drop(writer_txs);
    for handle in writer_handles {
      let _ = handle.join();
    }
    recv_result
  }
}

#[cfg(test)]
mod tests {
  use super::parse_reply_envelope;
  use bytes::Bytes;
  use zeromq::ZmqMessage;

  /// Build a `ZmqMessage` from frame byte-slices (the test analogue of a received reply).
  fn message(frames: &[&[u8]]) -> ZmqMessage {
    let mut iter = frames.iter();
    let first = iter.next().expect("at least one frame");
    let mut msg = ZmqMessage::from(first.to_vec());
    for f in iter {
      msg.push_back(Bytes::copy_from_slice(f));
    }
    msg
  }

  #[test]
  fn parses_a_well_formed_reply() {
    // [identity, service, taskid, data] — the minimal valid reply (one data frame).
    let msg = message(&[b"worker-7", b"tex_to_html", b"4242", b"<zip bytes>"]);
    let header = parse_reply_envelope(&msg).expect("valid envelope");
    assert_eq!(header.identity, "worker-7");
    assert_eq!(header.service, "tex_to_html");
    assert_eq!(header.taskid, 4242);
  }

  #[test]
  fn rejects_a_reply_with_no_data_frame() {
    // Exactly 3 frames (no data) is the D-12 shape — must be discarded, not accepted. With atomic
    // message delivery this can no longer swallow the next reply (it's just a short frame vector).
    let msg = message(&[b"worker-7", b"tex_to_html", b"4242"]);
    assert!(parse_reply_envelope(&msg).is_err());
  }

  #[test]
  fn rejects_an_empty_or_identity_only_reply() {
    assert!(parse_reply_envelope(&message(&[b""])).is_err());
    assert!(parse_reply_envelope(&message(&[b"worker-7"])).is_err());
    assert!(parse_reply_envelope(&message(&[b"worker-7", b"svc"])).is_err());
  }

  #[test]
  fn defaults_a_non_numeric_taskid_to_minus_one() {
    let msg = message(&[b"w", b"svc", b"not-a-number", b"data"]);
    let header = parse_reply_envelope(&msg).expect("valid frame count");
    assert_eq!(
      header.taskid, -1,
      "an unpar'seable taskid is -1 (unknown), not a panic"
    );
  }

  #[test]
  fn tolerates_non_utf8_header_frames() {
    // A hostile/garbled identity must not panic the parse — it falls back to the default label.
    let msg = message(&[&[0xff, 0xfe], b"svc", b"7", b"data"]);
    let header = parse_reply_envelope(&msg).expect("valid frame count");
    assert_eq!(header.identity, "_worker_");
    assert_eq!(header.taskid, 7);
  }
}
