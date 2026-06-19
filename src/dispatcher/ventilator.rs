use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use crate::backend;
use crate::dispatcher::server;
use crate::helpers;
use crate::helpers::{TaskProgress, TaskReport};
use crate::models::WorkerMetadataSender;
use std::error::Error;
use tracing::{debug, info, trace, warn};
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
    services_arc: &Arc<server::ServiceCache>,
    sandboxes_arc: &Arc<server::SandboxCache>,
    progress_queue_arc: &Arc<server::InFlightSet>,
    done_tx: &SyncSender<TaskReport>,
    job_limit: Option<usize>,
    dispatch_complete: &Arc<AtomicBool>,
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
    let in_flight_ids: Vec<i64> = progress_queue_arc.ids();
    backend.clear_limbo_tasks_except(&in_flight_ids)?;
    // Ok, let's bind to a port and start broadcasting
    let context = zmq::Context::new();
    let ventilator = context.socket(zmq::ROUTER)?;
    ventilator.set_router_handover(true)?;
    // Keep idle remote-worker connections alive across NAT/firewall idle-timeouts (set before bind
    // so accepted connections inherit it). Correctness is the reaper's job, not keepalive's.
    server::apply_tcp_keepalive(
      &ventilator,
      crate::config::config()
        .dispatcher
        .tcp_keepalive_idle_seconds,
    )?;

    let address = format!("tcp://*:{}", self.port);
    // Propagate a bind failure (e.g. `EADDRINUSE` when the port is still held by a just-restarted
    // dispatcher in TIME_WAIT, or a second instance) instead of an opaque `.unwrap()` panic — same
    // fail-fast (the manager aborts the thread → external restart), but with a diagnosable cause
    // and consistent with every other fallible call here + the sink's bind (KNOWN_ISSUES
    // robustness: no bare `unwrap` on the dispatch path).
    // Retry the bind briefly: on a clean restart the previous dispatcher can still
    // hold the port for a second or two (its graceful-shutdown drain, or socket
    // TIME_WAIT), so a one-shot bind crash-loops the new process on EADDRINUSE — the
    // "Failed to start TaskManager" abort seen restarting the dispatcher (2026-06-17).
    // Retry with backoff; if it still fails, propagate (the manager aborts → external
    // restart) with a diagnosable cause.
    {
      const BIND_ATTEMPTS: u32 = 15;
      let mut attempt = 1u32;
      loop {
        match ventilator.bind(&address) {
          Ok(()) => break,
          Err(e) if attempt < BIND_ATTEMPTS => {
            warn!(
              "ventilator: bind {address} attempt {attempt}/{BIND_ATTEMPTS} failed ({e}); \
               retrying in 500ms (port handover from a restarting dispatcher?)"
            );
            std::thread::sleep(Duration::from_millis(500));
            attempt += 1;
          },
          Err(e) => {
            return Err(
              std::io::Error::other(format!(
                "ventilator: zeromq bind {address} failed after {BIND_ATTEMPTS} attempts: {e}"
              ))
              .into(),
            );
          },
        }
      }
    }
    // Wake the blocking `recv` periodically instead of blocking forever when idle, so the
    // graceful-shutdown flag (checked at the top of the loop) is observed promptly even with no
    // worker traffic. Without this an idle ventilator blocks in `recv` until the supervisor's
    // stop-timeout SIGKILL — the ~120s `systemctl restart` hang (KNOWN_ISSUES D-15). 250ms matches
    // the sink's termination poll; the only cost is one no-op wakeup per quarter-second while idle.
    ventilator.set_rcvtimeo(250)?;
    let mut source_job_count: usize = 0;
    // Count of *fresh* tasks actually leased to a worker (a `retries == 0` lease), as distinct from
    // `source_job_count` which counts every request — including the mock-replies (unknown service,
    // backpressure, momentary-empty-queue). A bounded run (`job_limit = Some(N)`) terminates on N
    // **real dispatches**, not N requests: the unit mismatch — three threads counting `job_limit`
    // in three incompatible units — was the KNOWN_ISSUES D-5 lockstep-termination hang.
    let mut real_dispatched: usize = 0;
    // Reap timed-out in-flight tasks on a cadence rather than only on refetch (KNOWN_ISSUES D-6),
    // so the in-flight set drains even under sustained backpressure (when refetch never runs). The
    // cadence is runtime-configurable (`dispatcher.reap_interval_seconds`, default 60s — well below
    // the lease timeout, so an expired task is recovered promptly without scanning on every
    // request).
    let reap_interval_secs = crate::config::config().dispatcher.reap_interval_seconds;
    let mut last_reap_sec = chrono::Utc::now().timestamp();
    // Rate-limited logging for discarded requests (malformed framing, unknown service names). A
    // sustained malformed-request flood must not turn per-message `stderr` writes into a
    // throughput-DoS (KNOWN_ISSUES D-11) — count, don't narrate.
    let mut discard_log = server::RateLimitedLog::new(Duration::from_secs(5));

    loop {
      // Graceful shutdown (O-1): a SIGTERM/SIGINT set the flag. Stop leasing new work, signal
      // completion (so the sink drains the in-flight set and finalize flushes its last batch), and
      // return cleanly — the manager then takes the drain path instead of restarting us. Reached on
      // the next loop iteration; the recv below polls on a 250ms `RCVTIMEO`, so even a *fully idle*
      // dispatcher (no worker requests to cycle the loop) observes the flag within ~250ms and stops
      // promptly — no waiting out the supervisor's stop timeout (KNOWN_ISSUES D-15).
      if server::shutdown_requested() {
        info!(
          "ventilator: graceful shutdown requested — ceasing to lease; the sink will drain in-flight work"
        );
        dispatch_complete.store(true, Ordering::SeqCst);
        return Ok(real_dispatched);
      }
      let mut identity = zmq::Message::new();
      let mut msg = zmq::Message::new();
      // A worker request is exactly `[identity, service_name]` on the ROUTER: the DEALER worker
      // sends a single service-name frame and ROUTER prepends its identity. Read with strict
      // multipart-framing discipline so a malformed / empty / over-long request cannot desync the
      // message boundary and *permanently shuffle* every later request — the rare "3 adjacent empty
      // messages" failure seen in 08.2025 sandbox testing (KNOWN_ISSUES D-4). The previous code
      // read a second frame unconditionally, so a truncated `[identity]`-only message made it
      // read the *next* request's identity as this request's service (the shuffle), and
      // bailed the whole ventilator on the both-empty case (a restart band-aid). Instead:
      // require the service frame via `RCVMORE` before reading it (never read across a
      // message boundary), drain any unexpected trailing frames to stay aligned, and *skip* a
      // malformed request rather than restarting. This mirrors the sink's `[identity,
      // service, taskid, …]` envelope hardening.
      match ventilator.recv(&mut identity, 0) {
        Ok(()) => {},
        // RCVTIMEO elapsed with no request — loop back to re-check the shutdown flag (D-15: an idle
        // ventilator now stops within ~250ms of SIGTERM instead of hanging the supervisor stop).
        Err(zmq::Error::EAGAIN) => continue,
        Err(e) => return Err(e.into()),
      }
      if !ventilator.get_rcvmore().unwrap_or(false) {
        // `[identity]` with no service frame — truncated. Skipping consumes nothing further, so the
        // next request's frames are left intact (no desync).
        if let Some(n) = discard_log.record() {
          warn!(
            "ventilator: discarded {n} malformed request(s) [latest: truncated, no service frame] (rate-limited)"
          );
        }
        continue;
      }
      ventilator.recv(&mut msg, 0)?;
      // A well-formed request ends at the service frame; drain anything beyond it (an over-long /
      // malformed request) so it can't bleed into the next request — frame-alignment is exactly
      // what D-4 lost.
      while ventilator.get_rcvmore().unwrap_or(false) {
        let mut extra = zmq::Message::new();
        if ventilator.recv(&mut extra, 0).is_err() {
          break;
        }
      }
      let identity_str = identity.as_str().unwrap_or_default().to_string();
      let service_name = msg.as_str().unwrap_or_default().to_string();
      if service_name.is_empty() {
        // Empty service request (e.g. the "3 adjacent empty messages") — skip without desyncing.
        if let Some(n) = discard_log.record() {
          warn!(
            "ventilator: discarded {n} malformed request(s) [latest: empty service from {identity_str:?}] (rate-limited)"
          );
        }
        continue;
      }

      let request_time = chrono::Utc::now();
      source_job_count += 1;
      // Whether this iteration actually leased a task to the worker (vs. a mock-reply). Drives the
      // bounded-run source-drain check at the bottom of the loop.
      let mut dispatched_this_iter = false;
      // Reap timed-out in-flight tasks on a cadence (decoupled from refetch): routes each expired
      // task back to its own service's queue or reports it Fatal, so the in-flight set drains even
      // while saturated (backpressure) — closes the D-6 reaping-coupling residual.
      if request_time.timestamp() - last_reap_sec >= reap_interval_secs {
        last_reap_sec = request_time.timestamp();
        let reaped = server::reap_expired_into(&mut queues, progress_queue_arc, done_tx);
        // Health signal (Arm 8 observability; transport-independent): the in-flight gauge plus the
        // re-lease / dead-letter counts for this reaping pass. Only logged when something actually
        // timed out (the cadence is otherwise quiet), at `info` because a dead-letter is a task we
        // gave up on — an operator-relevant event.
        if reaped.requeued + reaped.dead_lettered > 0 {
          info!(
            in_flight = progress_queue_arc.len(),
            requeued = reaped.requeued,
            dead_lettered = reaped.dead_lettered,
            "dispatcher: reaped timed-out in-flight tasks"
          );
        }
      }
      // Requests for unknown service names will be silently ignored.
      let service_opt = match server::get_sync_service(&service_name, services_arc, &mut backend) {
        Some(s) => Some(s),
        None => {
          // An unknown service name is now handled gracefully — mock-reply so the (mis)configured
          // worker backs off — rather than the old fatal desync (the request framing is robust now,
          // D-4). Rate-limit the log so a flood of bad-service requests can't DoS us (D-11).
          if let Some(n) = discard_log.record() {
            warn!(
              "ventilator: discarded {n} request(s) [latest: unknown service {service_name:?} from {identity_str:?}, mock-replied] (rate-limited)"
            );
          }
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
        if server::in_flight_saturated(progress_queue_arc.len(), self.max_in_flight) {
          debug!(
            in_flight_cap = self.max_in_flight,
            worker = %identity_str,
            "BACKPRESSURE: in-flight set at capacity; mock-replying to worker"
          );
          ventilator.send(identity, SNDMORE)?;
          ventilator.send("0", SNDMORE)?;
          ventilator.send(Vec::new(), 0)?;
          continue;
        }
        let task_queue: &mut Vec<TaskProgress> = queues.entry(service.id).or_default();
        if task_queue.is_empty() {
          debug!(
            "No tasks in task queue for service {:?}, fetching up to {:?} more from backend...",
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
          dispatched_this_iter = true;
          // A `retries == 0` task is entering the pipeline for the first time; a re-leased task
          // (retries > 0, from the reaper requeue) was already counted on its first dispatch, so
          // only fresh leases advance the bounded-run target — keeping `real_dispatched` a true
          // unique-task count that finalize can eventually match.
          if current_task_progress.retries == 0 {
            real_dispatched += 1;
          }
          // Record the dispatch in the in-flight set BEFORE streaming the payload to the worker. A
          // fast worker (e.g. echo) can return its result to the sink before this iteration even
          // finishes; if the task were recorded only *after* the send (as it was), the sink's
          // `pop_progress_task` could miss it and discard the result, stranding the task `Queued`
          // until the ≥1h visibility-timeout reaper — the single-task-loss race that surfaced under
          // higher worker concurrency (KNOWN_ISSUES D-4 / docs/DISPATCHER_BENCH.md 8-worker loss).
          // Recording first also leaves a mid-stream send failure correctly in-flight for the
          // reaper.
          progress_queue_arc.insert(current_task_progress.clone());

          let current_task = current_task_progress.task;
          taskid = current_task.id;
          let serviceid = current_task.service_id;
          // Memoise this task's corpus → sandbox id now, before the payload is sent (so before the
          // result can return), so the sink scopes the result archive without its own DB hit (F-6).
          server::get_sync_sandbox_id(current_task.corpus_id, sandboxes_arc, &mut backend);
          trace!(
            job = source_job_count,
            worker = %identity_str,
            task_id = taskid,
            "ventilator: worker received task"
          );
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
              trace!(
                job = source_job_count,
                task_id = taskid,
                bytes = total_outgoing,
                took_ms = request_duration,
                "ventilator: streamed task payload to worker"
              );
            } else {
              warn!(
                task_id = taskid,
                "ventilator: failed to prepare input stream for task"
              );
              debug!(task_id = taskid, "task details: {current_task:?}");
              taskid = -1;
              ventilator.send(Vec::new(), 0)?;
            }
          }
        } else {
          trace!(
            job = source_job_count,
            worker = %identity_str,
            "ventilator: worker received mock reply"
          );
          ventilator.send("0", SNDMORE)?;
          ventilator.send(Vec::new(), 0)?;
        }
        // Update this worker's metadata (non-blocking enqueue to the background writer)
        self.metadata.dispatched(identity_str, service.id, taskid);
      } else {
        warn!(
          service = %service_name,
          worker = %identity_str,
          "ventilator: request for unknown service"
        );
      }
      if let Some(limit_number) = job_limit {
        // Bounded run terminates on N *real* dispatches (not N requests). Publish the shared
        // completion signal so the sink (which then drains the in-flight set) and finalize (which
        // the manager disconnects) agree on "done" — one condition instead of the three
        // incompatible per-thread counters that used to deadlock (KNOWN_ISSUES D-5).
        if real_dispatched >= limit_number {
          info!(
            "vent: bounded job limit ({limit_number}) reached after {real_dispatched} real dispatch(es); terminating ventilator"
          );
          dispatch_complete.store(true, Ordering::Release);
          return Ok(real_dispatched);
        }
        // Source-drain: this iteration leased nothing (the queue was empty after a refetch) and no
        // task is still in flight, so there is genuinely no more work to dispatch — stop rather
        // than mock-reply forever toward an unreachable limit (the owner-noted "empty-queue
        // mock-replies forever" gap). The in-flight-empty guard prevents terminating while
        // results are still pending; it is exact for the single-service bounded runs
        // `job_limit` is used for (a multi-service bounded run could drain one service
        // early — acceptable, benchmark-only).
        if !dispatched_this_iter && progress_queue_arc.is_empty() {
          info!(
            "vent: source exhausted after {real_dispatched} dispatch(es) (< limit {limit_number}); terminating ventilator"
          );
          dispatch_complete.store(true, Ordering::Release);
          return Ok(real_dispatched);
        }
      }
    }
  }
}
