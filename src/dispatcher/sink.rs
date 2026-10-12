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

/// How often the sink wakes from `recv()` to re-check the shared completion signal, so it can
/// terminate once the ventilator has finished dispatching and the in-flight set has drained. The
/// ventilator sets that signal (`dispatch_complete`) both when a **bounded** run hits its limit
/// (KNOWN_ISSUES D-5) and on a **graceful SIGTERM/SIGINT** (O-1) — so **every** run, production
/// included, polls on this cadence. A production sink that instead blocked plainly in `recv()` with
/// an idle fleet never noticed the shutdown and hung the supervisor's stop the full timeout →
/// SIGKILL on every restart/deploy (KNOWN_ISSUES D-16). `recv()` is cancel-safe — its `FairQueue`
/// buffers messages behind a `Mutex` and the `recv` future holds no message — so a timed-out poll
/// loses nothing.
const SINK_TERMINATION_POLL: Duration = Duration::from_millis(250);

/// A unit of work streamed from the sink's receive loop to an archive-writer thread (dispatcher
/// rationalization phase 3 — D-7 sink fan-out). The receive loop owns the ZMQ socket and reads each
/// result's frames; rather than block on the `/data` write itself, it hands a writer the task, then
/// the received chunks, then a commit/reject — so receiving the next result is no longer hostage to
/// the current one's disk latency.
enum WriteCommand {
  /// Begin a new result file for `task` at `recv_path`.
  Begin {
    /// the task this result is for, as the sink found it (the writer claims it on commit)
    task: Box<Task>,
    /// the identity of the worker that sent the result (its dispatch attempt, D-26)
    worker: String,
    /// `<entry-dir>/<service>.zip` — where the result archive is published
    recv_path: PathBuf,
  },
  /// Append a received data chunk to the in-progress result file. The bytes are owned/moved here
  /// and dropped right after the write, so the per-job footprint stays O(chunk), never the whole
  /// archive.
  Chunk(Vec<u8>),
  /// Close the in-progress file, parse its `cortex.log` into a report, claim it for its task,
  /// publish it, and hand the report to finalize.
  Commit,
  /// Reject the in-progress (over-cap) result: remove the partial file and, if it claims its task,
  /// report the task `Invalid` (`result_too_large`).
  Reject {
    /// the configured cap, for the rejection message
    max_result_bytes: usize,
  },
}

/// One result being written: the task it is for (as the sink found it), the worker that sent it,
/// where it goes, the temporary file it is written to first, and that file (`None` once a create
/// or write failed).
struct InProgress {
  task: Task,
  worker: String,
  recv_path: PathBuf,
  part_path: PathBuf,
  file: Option<File>,
}

/// The temporary path a result is written to before it is validated and claimed: beside its final
/// `recv_path`, so the rename that publishes it stays on one filesystem.
fn part_path(recv_path: &std::path::Path) -> PathBuf {
  let mut name = recv_path.file_name().unwrap_or_default().to_os_string();
  name.push(".part");
  recv_path.with_file_name(name)
}

/// The result a claimed result replaced (its attempt and status), if any.
type Replaced = Option<(usize, TaskStatus)>;

/// Claim a validated result with status `status` for its task (KNOWN_ISSUES D-26):
/// `Some((task, replaced))` when it is the result that stands — the caller publishes and reports
/// it — and `None` when it must be dropped (a result that must stand already landed, or the task
/// is gone), logged here.
fn claim_validated(
  in_flight: &server::InFlightSet,
  task_id: i64,
  worker: &str,
  status: TaskStatus,
) -> Option<(Task, Replaced)> {
  match in_flight.claim_result(task_id, worker, status) {
    server::ResultClaim::Accept {
      task,
      attempt,
      attempts,
      late,
      replaced,
    } => {
      if let Some(late) = late {
        info!(
          task_id,
          worker,
          attempt = attempt + 1,
          attempts,
          late = ?late,
          "sink writer: accepted a result that arrived after its lease expired (D-26)"
        );
      }
      Some((task, replaced))
    },
    server::ResultClaim::Superseded { attempt, finished } => {
      warn!(
        task_id,
        worker,
        attempt = attempt + 1,
        finished_attempt = finished + 1,
        "sink writer: dropped a result superseded by one that stands (D-26)"
      );
      None
    },
    server::ResultClaim::Stale { attempt } => {
      warn!(
        task_id,
        worker,
        attempt = attempt + 1,
        "sink writer: dropped a result of an earlier run of the task (re-run since, D-28)"
      );
      None
    },
    server::ResultClaim::Unknown => {
      warn!(
        task_id,
        worker, "sink writer: dropped a result whose task is no longer in flight or held"
      );
      None
    },
  }
}

/// Publish the claimed result `report` for `task`: rename its archive from `part_path` over
/// `recv_path`, and return the report to finalize. If the rename fails, the report must not
/// disagree with the archive on disk:
/// * when the result replaced an earlier accepted one (`replaced`), that one's archive and DB row
///   stand — nothing is reported, and the task's accepted result is restored
///   ([`server::InFlightSet::note_publish_failed`]): a publish failure never costs the task a good
///   result;
/// * otherwise the stale archive (an earlier run's, if any) is removed and a stand-in Fatal
///   `cortex/archive_publish_failed` is reported; while the task is held for late results (it had
///   more than one attempt), any later result of it still replaces the stand-in.
fn publish(
  in_flight: &server::InFlightSet,
  task: Task,
  mut report: TaskReport,
  part_path: &std::path::Path,
  recv_path: &std::path::Path,
  replaced: Replaced,
) -> Option<TaskReport> {
  let task_id = task.id;
  if let Err(e) = std::fs::rename(part_path, recv_path) {
    std::fs::remove_file(part_path).ok();
    in_flight.note_publish_failed(task_id, replaced);
    if replaced.is_some() {
      warn!(
        task_id,
        error = ?e,
        "sink writer: could not publish the result archive; the earlier accepted result stands"
      );
      return None;
    }
    warn!(
      task_id,
      error = ?e,
      "sink writer: could not publish the result archive; reporting a stand-in Fatal"
    );
    std::fs::remove_file(recv_path).ok();
    report = TaskReport {
      messages: vec![NewTaskMessage::new(
        task_id,
        "fatal",
        "cortex".to_string(),
        "archive_publish_failed".to_string(),
        format!("the result archive could not be published: {e}"),
      )],
      status: TaskStatus::Fatal,
      stand_in: true,
      ..report
    };
  }
  report.task = task;
  Some(report)
}

/// One archive-writer thread. It drains [`WriteCommand`]s for the tasks the receive loop assigns
/// it, performing the blocking `/data` write + `cortex.log` parse + finalize hand-off **off** the
/// receive loop. Per-result ordering is guaranteed because the receive loop sends a result's
/// `Begin → Chunk* → Commit|Reject` contiguously to one writer's FIFO channel, and per-task
/// ordering because every result of a task goes to the same writer ([`writer_for`]); fan-out is
/// across *different* tasks. The writer exits when its channel disconnects (the sink shutting down
/// on `job_limit`), after draining any buffered commands (so the last result's `Commit` still
/// reaches finalize — loss-free). A panic here is surfaced by the receive loop (its liveness check
/// / a send error) → the manager aborts the dispatcher (fail-fast preserved).
///
/// A result is written to a temporary file, parsed, and only then **claimed** for its task
/// ([`server::InFlightSet::claim_result`]); only a claimed result replaces the task's archive (a
/// rename) and is reported. So an empty, unreadable or unwritable result claims nothing: the task
/// stays in flight for the reaper (or held for another late result) — never stranded `Queued`
/// (KNOWN_ISSUES D-24) — and a published archive is never truncated by a result that fails or
/// loses to a newer attempt's (D-26).
fn run_writer(
  rx: &Receiver<WriteCommand>,
  done_tx: &SyncSender<TaskReport>,
  in_flight: &server::InFlightSet,
) {
  let mut current: Option<InProgress> = None;
  while let Ok(cmd) = rx.recv() {
    match cmd {
      WriteCommand::Begin {
        task,
        worker,
        recv_path,
      } => {
        let part_path = part_path(&recv_path);
        let file = match File::create(&part_path) {
          Ok(f) => Some(f),
          Err(e) => {
            warn!(
              task_id = task.id,
              path = ?part_path,
              error = ?e,
              "sink writer: File::create failed; result dropped, task left to the reaper"
            );
            None
          },
        };
        current = Some(InProgress {
          task: *task,
          worker,
          recv_path,
          part_path,
          file,
        });
      },
      WriteCommand::Chunk(bytes) => {
        if let Some(job) = current.as_mut()
          && let Some(file) = job.file.as_mut()
          && let Err(e) = file.write_all(&bytes)
        {
          warn!(
            task_id = job.task.id,
            error = ?e,
            "sink writer: write to result file failed; abandoning result"
          );
          // Stop writing; the (now-partial) file makes the result untrustworthy, so Commit
          // skips it and the task is left to the reaper.
          job.file = None;
        }
        // `bytes` dropped here — tight deallocation, O(chunk) resident.
      },
      WriteCommand::Commit => {
        let Some(job) = current.take() else {
          continue;
        };
        let task_id = job.task.id;
        let Some(file) = job.file else {
          std::fs::remove_file(&job.part_path).ok();
          in_flight.note_unusable_reply(task_id, &job.worker, true);
          warn!(
            task_id,
            "sink writer: no result file (create/write failed); result dropped, task left to the reaper"
          );
          continue;
        };
        // On disk before it is parsed and published, so a published archive is a complete one.
        if let Err(e) = file.sync_data() {
          warn!(task_id, error = ?e, "sink writer: could not sync the result file");
        }
        drop(file); // close before generate_report reads the archive back
        match helpers::generate_report(job.task, &job.part_path) {
          // A retryable cause (the worker harness's timeout / shed / unrecorded death), not a
          // result: back to the task's retry budget, its archive not published.
          Some(report) if report.is_retryable() => {
            let status = report.status;
            let cause = report.into_cause();
            match in_flight.defer_retryable(task_id, &job.worker, cause.clone()) {
              server::RetryDecision::Deferred => {
                // Not published while the task is retried.
                std::fs::remove_file(&job.part_path).ok();
                info!(
                  task_id,
                  worker = %job.worker,
                  "sink writer: a retryable cause; the task goes back to its retry budget"
                );
              },
              // The last attempt's cause: published, and reported as the task's stand-in.
              server::RetryDecision::Final { task } => {
                info!(
                  task_id,
                  worker = %job.worker,
                  "sink writer: a retryable cause on the last attempt; reported as a stand-in"
                );
                let stand_in = TaskReport {
                  task: task.clone(),
                  status,
                  messages: cause,
                  stand_in: true,
                };
                if let Some(report) = publish(
                  in_flight,
                  task,
                  stand_in,
                  &job.part_path,
                  &job.recv_path,
                  None,
                ) {
                  server::send_done(done_tx, report);
                }
              },
              server::RetryDecision::Dropped(why) => {
                std::fs::remove_file(&job.part_path).ok();
                info!(
                  task_id,
                  worker = %job.worker,
                  why,
                  "sink writer: a retryable cause dropped"
                );
              },
            }
          },
          Some(report) => match claim_validated(in_flight, task_id, &job.worker, report.status) {
            Some((task, replaced)) => {
              if let Some(report) = publish(
                in_flight,
                task,
                report,
                &job.part_path,
                &job.recv_path,
                replaced,
              ) {
                server::send_done(done_tx, report);
              }
            },
            None => {
              std::fs::remove_file(&job.part_path).ok();
            },
          },
          // Unreadable / empty result archive (0-byte, truncated, no `cortex.log`) — an
          // infrastructure failure, not a verdict. Claim nothing, so the lease reaper recovers
          // the task: retry, then dead-letter with a message after MAX_DISPATCH_RETRIES — never
          // a silent terminal Fatal (D-18). Drop the artifact so a retry writes a clean file.
          None => {
            std::fs::remove_file(&job.part_path).ok();
            in_flight.note_unusable_reply(task_id, &job.worker, true);
            warn!(
              task_id,
              "sink writer: empty/unreadable result archive; result dropped, task left to the reaper (D-18)"
            );
          },
        }
      },
      WriteCommand::Reject { max_result_bytes } => {
        let Some(job) = current.take() else {
          continue;
        };
        drop(job.file);
        std::fs::remove_file(&job.part_path).ok();
        let taskid = job.task.id;
        warn!(
          task_id = taskid,
          max_result_bytes, "sink: result exceeded byte cap — rejected (Invalid)"
        );
        if let Some((task, _)) =
          claim_validated(in_flight, taskid, &job.worker, TaskStatus::Invalid)
        {
          let report = TaskReport {
            stand_in: false,
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

/// The archive writer that handles every result of task `taskid`: one per task, so results for the
/// same task (two attempts' late results, D-26) are written to its single result path one after
/// the other, in arrival order — never by two writers at once, which corrupted the archive.
fn writer_for(taskid: i64, num_writers: usize) -> usize {
  taskid
    .unsigned_abs()
    .checked_rem(num_writers as u64)
    .unwrap_or(0) as usize
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
  /// Termination is driven entirely by the shared `dispatch_complete` signal (set by the ventilator
  /// on a bounded run's end **or** a graceful shutdown), not by `_job_limit` — the sink stops once
  /// dispatching is done and the in-flight set has drained.
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
    _job_limit: Option<usize>,
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
      let writer_in_flight = Arc::clone(progress_queue_arc);
      writer_txs.push(tx);
      writer_handles.push(thread::spawn(move || {
        run_writer(&rx, &writer_done_tx, &writer_in_flight)
      }));
    }

    let address = format!("tcp://0.0.0.0:{}", self.port);

    // A current-thread runtime is right: there is one PULL socket and one receive loop, and
    // blocking it on writer/done-channel backpressure is exactly the flow control we want (stop
    // receiving when the pipeline downstream is full). The writer pool threads drain
    // independently, so a blocking send here always makes progress.
    let runtime = tokio::runtime::Builder::new_current_thread()
      .enable_all()
      .build()?;

    let recv_result: Result<(), Box<dyn Error>> = runtime.block_on(async {
      // Retry the bind (mirrors the ventilator) so a port handover from a restarting
      // dispatcher doesn't crash-loop the sink on EADDRINUSE. The window must OUTLAST TCP
      // TIME_WAIT (~60s = tcp_fin_timeout), not just a few seconds — see the ventilator's
      // note (observed 2026-07-01: the old 7.5s window crash-looped the sink through the
      // full 60s TIME_WAIT until systemd's start-limit gave up).
      let mut pull = {
        // 150 * 500ms = 75s > tcp_fin_timeout (60s) TIME_WAIT, with margin.
        const BIND_ATTEMPTS: u32 = 150;
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
        // Always poll the recv on a short timeout (every run, production included) so the sink
        // wakes to re-check the shared completion signal even when idle —
        // `dispatch_complete` is set by the ventilator both at a bounded run's end (D-5)
        // and on a graceful SIGTERM/SIGINT (O-1). The timeout only fires while idle
        // (nothing mid-delivery), and `recv()` is cancel-safe, so a dropped timed-out
        // future loses no message (KNOWN_ISSUES D-5 + the D-16 idle-shutdown
        // hang this closes: a production sink no longer blocks past the shutdown signal).
        let recv_outcome = match tokio::time::timeout(SINK_TERMINATION_POLL, pull.recv()).await {
          Ok(inner) => inner,
          Err(_elapsed) => {
            if dispatch_complete.load(Ordering::Acquire)
              && progress_queue_arc.awaiting_replies() == 0
            {
              info!("sink: dispatching complete and in-flight drained; terminating sink");
              break;
            }
            continue;
          },
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

        // Look the task up without claiming it: a result for the wrong service, or one with no
        // output path, leaves the task exactly where it was — in flight for the reaper, or held for
        // a late result — rather than stranding it.
        if let Some(task) = progress_queue_arc.task(taskid) {
          match server::get_service(&header.service, services_arc) {
            None => {
              return Err(Box::<dyn Error>::from(io::Error::other(
                "sink: get_service found nothing",
              )));
            },
            Some(service) => {
              if service.id == task.service_id {
                // Where the result goes: `Some(None)` for init (no payload — its empty data frames
                // are ignored, the report is inline), `Some(Some(path))` for a result archive:
                // `<entry-dir>/<service>.zip`, or a sandbox-scoped name when this task's corpus is
                // a sandbox (lock-free cache read, memoised by the ventilator on
                // dispatch — F-6). An unknown corpus yields no path (fail closed):
                // never guess the parent's published archive name — the result is
                // dropped and the task left to the reaper, which retries it
                // (re-dispatch re-runs the sandbox lookup) or dead-letters it.
                let placement = if service.id == 1 {
                  Some(None)
                } else {
                  let recv_path_opt = server::get_sandbox_id(task.corpus_id, sandboxes_arc)
                    .and_then(|sandbox_id| {
                      helpers::result_archive_path(&task.entry, &service.name, sandbox_id)
                    });
                  if recv_path_opt.is_none() {
                    progress_queue_arc.note_unusable_reply(task.id, &header.identity, false);
                    warn!(
                      task_id = task.id,
                      corpus_id = task.corpus_id,
                      entry = ?task.entry,
                      "sink: could not derive a result path (bad entry or unknown corpus); \
                       result dropped, task left to the reaper"
                    );
                  }
                  recv_path_opt.map(Some)
                };
                match placement {
                  // Init: no archive to validate — claim and report inline.
                  Some(None) => {
                    if let server::ResultClaim::Accept { task, .. } = progress_queue_arc
                      .claim_result(taskid, &header.identity, TaskStatus::NoProblem)
                    {
                      server::send_done(
                        done_tx,
                        TaskReport {
                          stand_in: false,
                          task,
                          status: TaskStatus::NoProblem,
                          messages: Vec::new(),
                        },
                      );
                    }
                  },
                  // A result archive: the writer writes and parses it, then claims it for the task
                  // — accepted unless a newer attempt's result already landed (D-26) — so a
                  // result that turns out empty or unreadable claims nothing.
                  Some(Some(recv_path)) => {
                    // Hard size cap (disk protection): sum the data frames; an over-cap result is
                    // rejected (Invalid) without being written. The whole multipart message is
                    // already received (ZMQ delivers it atomically — true of libzmq too), so this
                    // is the same disk-protection guarantee as the streamed check, just computed
                    // up front.
                    let data_bytes: usize = msg.iter().skip(3).map(|f| f.len()).sum();
                    let oversized = data_bytes > max_result_bytes;
                    // A task's results all go to the same writer, so two attempts' results for
                    // one task (D-26) are written, claimed and reported in arrival order.
                    let widx = writer_for(taskid, num_writers);
                    // The reaper leaves the task alone while its writer decides the result.
                    progress_queue_arc.note_received(taskid);
                    // A send error means that writer died → fail-fast.
                    if writer_txs[widx]
                      .send(WriteCommand::Begin {
                        task: Box::new(task.clone()),
                        worker: header.identity.clone(),
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
                  None => {},
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
          // No such task in flight or in its late-result window — drop the message (already fully
          // received; nothing to drain).
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

        // Terminate once the ventilator has signalled dispatching is done AND every dispatched task
        // has come back (the in-flight set is empty) — the shared completion condition that
        // replaced the old per-thread `sink_job_count >= limit` (KNOWN_ISSUES D-5), set on
        // a bounded run's end or a graceful shutdown (D-16). Checked here right after a
        // result lands (so the run ends promptly when the last one arrives) and on the
        // recv-timeout poll above (so a sink already blocked when the signal flips still
        // wakes to notice).
        if dispatch_complete.load(Ordering::Acquire) && progress_queue_arc.awaiting_replies() == 0 {
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
  fn a_tasks_results_share_one_writer() {
    use super::writer_for;
    for taskid in [0_i64, 1, 7, 4242, i64::from(i32::MAX) + 3] {
      assert_eq!(writer_for(taskid, 4), writer_for(taskid, 4));
      assert!(writer_for(taskid, 4) < 4);
      assert_eq!(writer_for(taskid, 1), 0);
    }
    assert_eq!(
      writer_for(-1, 4),
      1,
      "never out of range, even for a bad id"
    );
    assert_eq!(writer_for(9, 0), 0, "no writers configured is not a panic");
    let spread: std::collections::HashSet<usize> = (0..8).map(|t| writer_for(t, 4)).collect();
    assert_eq!(
      spread.len(),
      4,
      "different tasks fan out across the writers"
    );
  }

  /// A held task (attempt 0's usable result accepted, attempt 1 in flight) and a publish that will
  /// fail: the result's final path is a non-empty directory, so the rename cannot replace it.
  fn failing_publish(
    tag: &str,
  ) -> (
    std::path::PathBuf,
    crate::dispatcher::server::InFlightSet,
    std::path::PathBuf,
    std::path::PathBuf,
  ) {
    use crate::helpers::TaskProgress;
    use crate::models::Task;
    let dir =
      std::env::temp_dir().join(format!("cortex_sink_publish_{tag}_{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let recv = dir.join("svc.zip");
    std::fs::create_dir(&recv).unwrap();
    std::fs::write(recv.join("keep"), b"x").unwrap();
    let part = dir.join("svc.zip.part");
    std::fs::write(&part, b"new archive").unwrap();
    let set = crate::dispatcher::server::InFlightSet::new();
    set.insert(TaskProgress {
      run_start: 0,
      task: Task {
        id: 1,
        service_id: 3,
        corpus_id: 1,
        status: 7,
        entry: String::new(),
      },
      created_at: chrono::Utc::now().timestamp(),
      retries: 1,
      lease_timeout_seconds: 240,
      attempt_workers: vec!["w0".into(), "w1".into()],
      last_cause: Vec::new(),
    });
    (dir, set, recv, part)
  }

  fn report_for(
    task: crate::models::Task,
    status: crate::helpers::TaskStatus,
  ) -> crate::helpers::TaskReport {
    crate::helpers::TaskReport {
      task,
      status,
      messages: Vec::new(),
      stand_in: false,
    }
  }

  #[test]
  fn a_failed_publish_keeps_the_result_it_would_have_replaced() {
    use crate::dispatcher::server::ResultClaim;
    use crate::helpers::TaskStatus;
    let (dir, set, recv, part) = failing_publish("kept");
    assert!(matches!(
      set.claim_result(1, "w0", TaskStatus::NoProblem),
      ResultClaim::Accept { .. }
    ));
    let ResultClaim::Accept { task, replaced, .. } = set.claim_result(1, "w1", TaskStatus::Warning)
    else {
      panic!("the newer attempt's result is accepted");
    };
    let report = report_for(task.clone(), TaskStatus::Warning);
    assert!(
      super::publish(&set, task, report, &part, &recv, replaced).is_none(),
      "nothing reported: attempt 0's result and archive stand"
    );
    assert!(
      recv.join("keep").exists(),
      "the earlier archive is not removed"
    );
    assert!(!part.exists());
    assert!(matches!(
      set.claim_result(1, "w1", TaskStatus::Fatal),
      ResultClaim::Superseded { finished: 0, .. }
    ));
    std::fs::remove_dir_all(dir).ok();
  }

  #[test]
  fn a_failed_publish_of_a_first_result_reports_a_stand_in() {
    use crate::dispatcher::server::ResultClaim;
    use crate::helpers::TaskStatus;
    let (dir, set, recv, part) = failing_publish("stand_in");
    // Here the final path holds a stale archive (an earlier run's file), and the rename fails
    // because the written result is gone.
    std::fs::remove_dir_all(&recv).unwrap();
    std::fs::write(&recv, b"stale archive").unwrap();
    std::fs::remove_file(&part).unwrap();
    let ResultClaim::Accept { task, replaced, .. } = set.claim_result(1, "w1", TaskStatus::Fatal)
    else {
      panic!("accepted");
    };
    let report = report_for(task.clone(), TaskStatus::Fatal);
    let stand_in = super::publish(&set, task, report, &part, &recv, replaced).expect("a report");
    assert!(stand_in.stand_in && stand_in.status == TaskStatus::Fatal);
    assert!(
      !recv.exists(),
      "the stale archive is removed, so disk and DB agree"
    );
    // Any later result can still land.
    assert!(matches!(
      set.claim_result(1, "w1", TaskStatus::Fatal),
      ResultClaim::Accept { .. }
    ));
    std::fs::remove_dir_all(dir).ok();
  }

  #[test]
  fn a_result_is_written_beside_its_archive_first() {
    let part = super::part_path(std::path::Path::new("/data/x/doc/svc.zip"));
    assert_eq!(part, std::path::PathBuf::from("/data/x/doc/svc.zip.part"));
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
