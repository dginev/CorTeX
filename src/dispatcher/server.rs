use std::collections::HashMap;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::backend::Backend;
use crate::helpers::{NewTaskMessage, TaskProgress, TaskReport, TaskStatus};
use crate::models::Service;

/// Rate-limited logging for high-frequency, low-value events — the dispatcher's *discarded*
/// messages (malformed replies/requests, unknown service names, unknown task ids). The dispatcher's
/// request and sink loops `eprintln!`/`println!` one line per skipped message; under a **sustained
/// flood** (a hostile or buggy peer spamming bad frames) those synchronous, locked
/// `stderr`/`stdout` writes serialise and *slow the real pipeline* — a self-inflicted
/// throughput-DoS (KNOWN_ISSUES D-11). This aggregates instead: it counts events and signals an
/// emit at most once per `interval` (plus the very first event, so a problem is visible
/// immediately), carrying the suppressed count. So a flood of any size costs **O(1) log I/O per
/// interval**, not O(flood) — *counted, not narrated* — while a genuine trickle is still surfaced.
/// Per-thread (no sharing/locking); the clock read per event is a cheap monotonic `Instant::now()`.
pub struct RateLimitedLog {
  events_since_emit: u64,
  last_emit: Option<Instant>,
  interval: Duration,
}

impl RateLimitedLog {
  /// A new aggregator that emits at most once per `interval` (after an immediate first emit).
  pub fn new(interval: Duration) -> Self {
    RateLimitedLog {
      events_since_emit: 0,
      last_emit: None,
      interval,
    }
  }

  /// Records one event. Returns `Some(count)` — the number of events since the last emit, inclusive
  /// — when a summary line is due (the first event always, then at most once per `interval`),
  /// resetting the counter; otherwise `None` (suppress; just counted).
  pub fn record(&mut self) -> Option<u64> {
    self.events_since_emit += 1;
    let due = match self.last_emit {
      None => true,
      Some(at) => at.elapsed() >= self.interval,
    };
    if due {
      let count = self.events_since_emit;
      self.events_since_emit = 0;
      self.last_emit = Some(Instant::now());
      Some(count)
    } else {
      None
    }
  }
}

/// Hard ceiling on the in-flight (progress) set. Reaching it means backpressure
/// ([`crate::config::DispatcherConfig::max_in_flight`]) failed to hold the line, so we fail fast
/// (panic → process abort → external restart) rather than exhaust memory — the dispatcher's
/// intentional fail-fast design. Backpressure must engage *below* this (asserted in tests).
pub const PROGRESS_QUEUE_HARD_LIMIT: usize = 10_000;
/// Capacity of the bounded done (results-pending-persist) channel between the producers (sink +
/// ventilator reaper) and the finalize thread. A **full** channel *blocks* the producer — that *is*
/// the backpressure (a slow DB makes the sink wait, which backs up the ZMQ PULL, which backs up the
/// workers), replacing the old `DONE_QUEUE_HARD_LIMIT` panic-then-OOM backstop with a graceful,
/// loss-free hand-off (a bounded channel blocks rather than drops). Phase 1 of the dispatcher
/// rationalization (`docs/DISPATCHER_RATIONALIZATION.md`).
pub const DONE_QUEUE_CAPACITY: usize = 10_000;

/// Whether the in-flight set is saturated and the ventilator should apply backpressure (stop
/// leasing new work and mock-reply). Saturation is inclusive: at the threshold we already hold
/// back, keeping the set bounded *below* [`PROGRESS_QUEUE_HARD_LIMIT`].
pub fn in_flight_saturated(in_flight: usize, max_in_flight: usize) -> bool {
  in_flight >= max_in_flight
}

/// Current size of the in-flight (progress) set — the count of dispatched-but-unfinished tasks.
pub fn progress_queue_len<S: ::std::hash::BuildHasher>(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress, S>>>,
) -> usize {
  progress_queue_arc
    .lock()
    .expect("Failed to obtain Mutex lock in progress_queue_len")
    .len()
}

/// Persists a **batch** of finished reports to the Task store, with bounded retry on a transient DB
/// failure. The finalize thread drains the batch off the bounded done channel and hands it here in
/// one `mark_done` call (amortizing the round-trip). On exhausted retries it returns `Err`, which
/// the finalize thread propagates into a panic → the manager aborts the whole dispatcher — the
/// intended fail-fast on a DB runaway (the channel design replaces the old mutex-poisoning that
/// achieved the same propagation). A crash here loses nothing: the tasks remain `Queued` and
/// recover on restart.
pub fn mark_done_batch(backend: &mut Backend, reports: &[TaskReport]) -> Result<(), String> {
  if reports.is_empty() {
    return Ok(());
  }
  let request_time = chrono::Utc::now();
  let mut success = false;
  if let Err(e) = backend.mark_done(reports) {
    println!("-- mark_done attempt failed: {e:?}");
    // DB persist failed, retry
    let mut retries = 0;
    while retries < 3 {
      thread::sleep(Duration::new(2, 0)); // wait 2 seconds before retrying, in case this is latency related
      retries += 1;
      match backend.mark_done(reports) {
        Ok(_) => {
          success = true;
          break;
        },
        Err(e) => println!("-- mark_done retry failed: {e:?}"),
      };
    }
  } else {
    success = true;
  }
  if !success {
    return Err(String::from(
      "Database ran away during mark_done persisting.",
    ));
  }
  let request_duration = (chrono::Utc::now() - request_time).num_milliseconds();
  println!(
    "finalize: reporting {} tasks to DB took {request_duration}ms.",
    reports.len()
  );
  Ok(())
}

/// Hands a finished report off to the finalize thread over the bounded done channel. A full channel
/// **blocks** the producer (backpressure — see [`DONE_QUEUE_CAPACITY`]). An `Err` means the
/// finalize receiver is gone (the thread died) — the report can't be persisted, but its task stays
/// `Queued` and is recovered on restart, and the manager's supervision aborts the dispatcher, so we
/// only log.
pub fn send_done(done_tx: &SyncSender<TaskReport>, report: TaskReport) {
  if done_tx.send(report).is_err() {
    eprintln!(
      "-- done channel closed (finalize thread gone); the manager will abort for a restart"
    );
  }
}

/// Check for, remove and return any expired tasks from the progress queue
pub fn timeout_progress_tasks<S: ::std::hash::BuildHasher>(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress, S>>>,
) -> Vec<TaskProgress> {
  let mut progress_queue = progress_queue_arc
    .lock()
    .expect("Failed to obtain Mutex lock in timeout_progress_tasks");
  let now = chrono::Utc::now().timestamp();
  let expired_keys = progress_queue
    .iter()
    .filter(|&(_, v)| v.expected_at() < now)
    .map(|(k, _)| *k)
    .collect::<Vec<_>>();
  let mut expired_tasks = Vec::new();
  for key in expired_keys {
    if let Some(task_progress) = progress_queue.remove(&key) {
      expired_tasks.push(task_progress);
    }
  }
  expired_tasks
}

/// Pops the next task from the progress queue
pub fn pop_progress_task<S: ::std::hash::BuildHasher>(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress, S>>>,
  taskid: i64,
) -> Option<TaskProgress> {
  if taskid < 0 {
    // Mock ids are to be skipped
    return None;
  }
  let mut progress_queue = progress_queue_arc
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in pop_progress_task"));
  progress_queue.remove(&taskid)
}

/// Pushes a new task on the progress queue
pub fn push_progress_task<S: ::std::hash::BuildHasher>(
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress, S>>>,
  progress_task: TaskProgress,
) {
  let mut progress_queue = progress_queue_arc
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in push_progress_task"));
  // Fail-fast backstop if backpressure (max_in_flight) ever fails to hold the line; see
  // PROGRESS_QUEUE_HARD_LIMIT. A workaround for the inability to catch thread panic!() calls.
  if progress_queue.len() > PROGRESS_QUEUE_HARD_LIMIT {
    panic!(
      "Progress queue is too large: {:?} tasks. Stop the ventilator!",
      progress_queue.len()
    );
  }
  progress_queue.insert(progress_task.task.id, progress_task);
}

/// The maximum number of dispatch retries before a perpetually-incomplete task is given up on.
/// A task re-dispatched this many times that still never returns a result is treated as a hard
/// failure (`Fatal`) rather than retried forever.
pub const MAX_DISPATCH_RETRIES: i64 = 4;

/// The fate of a timed-out in-flight task, decided by [`classify_expired`].
pub enum ExpiredOutcome {
  /// Re-dispatch the task — retry budget remains; its retry count is incremented.
  Requeue(TaskProgress),
  /// Give up — retry budget exhausted; report the task `Fatal`.
  Fatal(TaskReport),
}

/// Decides what to do with an in-flight task that timed out: retry it (until
/// [`MAX_DISPATCH_RETRIES`] dispatches) or give up and report it `Fatal`.
pub fn classify_expired(expired: TaskProgress) -> ExpiredOutcome {
  if expired.retries > MAX_DISPATCH_RETRIES {
    let task_id = expired.task.id;
    ExpiredOutcome::Fatal(TaskReport {
      task: expired.task,
      status: TaskStatus::Fatal,
      messages: vec![NewTaskMessage::new(
        task_id,
        "fatal",
        "cortex".to_string(),
        "never_completed_with_retries".to_string(),
        String::new(),
      )],
    })
  } else {
    ExpiredOutcome::Requeue(TaskProgress {
      task: expired.task,
      created_at: expired.created_at,
      retries: expired.retries + 1,
    })
  }
}

/// Reaps timed-out in-flight tasks and routes each to **its own service's** dispatch queue (a
/// retry) or the done queue (a `Fatal`). Decoupled from the refetch path so the in-flight set
/// drains even under sustained backpressure (KNOWN_ISSUES D-6); routing by `task.service_id` rather
/// than the requesting service fixes the latent cross-service requeue bug.
pub fn reap_expired_into(
  queues: &mut HashMap<i32, Vec<TaskProgress>>,
  progress_queue_arc: &Arc<Mutex<HashMap<i64, TaskProgress>>>,
  done_tx: &SyncSender<TaskReport>,
) {
  for expired in timeout_progress_tasks(progress_queue_arc) {
    match classify_expired(expired) {
      ExpiredOutcome::Requeue(task_progress) => queues
        .entry(task_progress.task.service_id)
        .or_default()
        .push(task_progress),
      ExpiredOutcome::Fatal(report) => send_done(done_tx, report),
    }
  }
}

/// Memoized getter for a `Service` record from the backend
pub fn get_sync_service<S: ::std::hash::BuildHasher>(
  service_name: &str,
  services: &Arc<Mutex<HashMap<String, Option<Service>, S>>>,
  backend: &mut Backend,
) -> Option<Service> {
  let mut services = services
    .lock()
    .unwrap_or_else(|_| panic!("Failed to obtain Mutex lock in get_sync_services"));
  services
    .entry(service_name.to_string())
    .or_insert_with(|| Service::find_by_name(service_name, &mut backend.connection).ok())
    .clone()
}

/// Getter for a `Service` stored inside an `Arc<Mutex<HashMap>`, with no DB access
pub fn get_service<S: ::std::hash::BuildHasher>(
  service_name: &str,
  services: &Arc<Mutex<HashMap<String, Option<Service>, S>>>,
) -> Option<Service> {
  let services = services
    .lock()
    .expect("Failed to obtain Mutex lock in get_service");
  let service = services.get(service_name);
  match service {
    None => None, // TODO: Should we panic? Can we recover?
    Some(service) => service.clone(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::DispatcherConfig;
  use crate::models::Task;

  fn dummy_progress(id: i64) -> TaskProgress {
    TaskProgress {
      task: Task {
        id,
        service_id: 1,
        corpus_id: 1,
        status: 0,
        entry: String::new(),
      },
      created_at: 0,
      retries: 0,
    }
  }

  #[test]
  fn in_flight_saturation_is_inclusive_at_the_threshold() {
    assert!(!in_flight_saturated(0, 5));
    assert!(!in_flight_saturated(4, 5));
    assert!(
      in_flight_saturated(5, 5),
      "at the threshold we already apply backpressure"
    );
    assert!(in_flight_saturated(6, 5));
  }

  #[test]
  fn backpressure_engages_below_the_hard_panic_bound() {
    // If the configured backpressure threshold ever reached the hard limit, backpressure would be
    // dead code and the in-flight set could still grow to the panic — the very crash D-6 exists to
    // prevent. Guard that invariant so a future config change can't silently reintroduce it.
    assert!(
      DispatcherConfig::default().max_in_flight < PROGRESS_QUEUE_HARD_LIMIT,
      "backpressure must engage before the hard panic bound"
    );
  }

  #[test]
  fn progress_queue_len_tracks_dispatch_and_drain() {
    let queue = Arc::new(Mutex::new(HashMap::new()));
    assert_eq!(progress_queue_len(&queue), 0);
    push_progress_task(&queue, dummy_progress(1));
    push_progress_task(&queue, dummy_progress(2));
    assert_eq!(progress_queue_len(&queue), 2, "two tasks now in flight");
    // The sink draining a returned result shrinks the in-flight set — how backpressure recovers.
    pop_progress_task(&queue, 1);
    assert_eq!(progress_queue_len(&queue), 1);
  }

  fn expired_progress(id: i64, service_id: i32, retries: i64) -> TaskProgress {
    let mut tp = dummy_progress(id);
    tp.task.service_id = service_id;
    tp.created_at = 0; // expected_at = (retries+1)*3600, far in the past -> always expired
    tp.retries = retries;
    tp
  }

  #[test]
  fn classify_expired_retries_then_gives_up() {
    // Budget remains -> requeue with the retry count incremented.
    match classify_expired(expired_progress(1, 3, MAX_DISPATCH_RETRIES)) {
      ExpiredOutcome::Requeue(tp) => assert_eq!(tp.retries, MAX_DISPATCH_RETRIES + 1),
      ExpiredOutcome::Fatal(_) => panic!("still within retry budget -> should requeue"),
    }
    // Budget exhausted -> Fatal.
    match classify_expired(expired_progress(2, 3, MAX_DISPATCH_RETRIES + 1)) {
      ExpiredOutcome::Fatal(report) => assert_eq!(report.task.id, 2),
      ExpiredOutcome::Requeue(_) => panic!("retry budget exhausted -> should be Fatal"),
    }
  }

  #[test]
  fn rate_limited_log_emits_first_then_throttles_then_summarizes() {
    let mut log = RateLimitedLog::new(Duration::from_millis(40));
    // The first event always emits, so a problem is visible immediately.
    assert_eq!(log.record(), Some(1));
    // Subsequent events within the interval are suppressed (counted, not narrated).
    assert_eq!(log.record(), None);
    assert_eq!(log.record(), None);
    // After the interval, the next event emits a summary carrying the suppressed count
    // (the two suppressed above + this one).
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(log.record(), Some(3));
    // ...and the counter resets for the next window.
    assert_eq!(log.record(), None);
  }

  #[test]
  fn reap_routes_to_own_service_and_drains() {
    let progress = Arc::new(Mutex::new(HashMap::new()));
    push_progress_task(&progress, expired_progress(1, 7, 0)); // retriable, service 7
    push_progress_task(&progress, expired_progress(2, 9, MAX_DISPATCH_RETRIES + 1)); // fatal, svc 9
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<TaskReport>(10);

    reap_expired_into(&mut queues, &progress, &done_tx);

    // The retriable task is requeued to ITS OWN service (7), not some requester's queue, retry++.
    assert_eq!(queues.get(&7).map(Vec::len), Some(1));
    assert_eq!(queues[&7][0].retries, 1);
    assert!(
      !queues.contains_key(&9),
      "the exhausted task is not requeued"
    );
    // The exhausted task is reported Fatal on the done channel.
    let reports: Vec<TaskReport> = done_rx.try_iter().collect();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].task.id, 2);
    // The in-flight set is fully drained.
    assert_eq!(progress_queue_len(&progress), 0);
  }
}
