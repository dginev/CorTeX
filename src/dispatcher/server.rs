use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::{debug, error, warn};

use crate::backend::Backend;
use crate::helpers::{NewTaskMessage, TaskProgress, TaskReport, TaskStatus};
use crate::models::Service;

/// Probe interval (seconds) between TCP keepalive probes once the idle threshold is crossed, and
/// the number of unanswered probes before the OS declares the peer dead — fixed sane values so only
/// the idle threshold needs to be a config knob (see [`crate::config::DispatcherConfig`]). With the
/// defaults a dead worker is detected in roughly `idle + 4×30 s` ≈ a few minutes — far faster than
/// the 1 h lease, while keeping NAT mappings warm.
const KEEPALIVE_PROBE_INTERVAL_SECS: i32 = 30;
/// See [`KEEPALIVE_PROBE_INTERVAL_SECS`].
const KEEPALIVE_PROBE_COUNT: i32 = 4;

/// Applies TCP keepalive to a worker-facing ZMQ socket (the ventilator's ROUTER, the sink's PULL),
/// the "stable" half of a remote worker interface: keepalive keeps idle worker connections alive
/// across NAT/firewall idle-timeouts (an idle mapping is otherwise silently dropped, dropping the
/// worker from the fleet until it reconnects) and lets the OS reap a dead peer's route. It does
/// **not** affect task-recovery correctness — the lease reaper is that net — so `idle_seconds <= 0`
/// safely disables it (OS default). **Set before `bind`** so accepted connections inherit it.
pub fn apply_tcp_keepalive(socket: &zmq::Socket, idle_seconds: i32) -> zmq::Result<()> {
  if idle_seconds <= 0 {
    return Ok(());
  }
  socket.set_tcp_keepalive(1)?;
  socket.set_tcp_keepalive_idle(idle_seconds)?;
  socket.set_tcp_keepalive_intvl(KEEPALIVE_PROBE_INTERVAL_SECS)?;
  socket.set_tcp_keepalive_cnt(KEEPALIVE_PROBE_COUNT)?;
  Ok(())
}

/// **Graceful-shutdown request flag** (O-1, orchestration). Set by the SIGTERM/SIGINT handler the
/// dispatcher binary installs; the ventilator checks it each loop iteration and, when set, signals
/// completion (`dispatch_complete`) and returns — so the manager drains the in-flight set and
/// flushes the finalize batch before exiting, rather than the supervisor hard-killing in-flight
/// work. ONLY a **planned** stop uses this; unexpected failures still fail-fast (panic → abort).
/// Production-only — bounded test runs never install the handler, so this stays `false` and the
/// dispatch path is byte-for-byte unchanged.
pub static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// SIGTERM/SIGINT handler. The only thing it does is an async-signal-safe atomic store — nothing
/// allocating or locking, as `signal(2)` requires.
extern "C" fn handle_shutdown_signal(_sig: libc::c_int) {
  SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

/// Installs SIGTERM + SIGINT handlers that request a graceful drain-and-stop. Call once from the
/// dispatcher binary's `main` (never in bounded test runs).
pub fn install_shutdown_handlers() {
  // SAFETY: `handle_shutdown_signal` is async-signal-safe (a lone atomic store).
  unsafe {
    libc::signal(
      libc::SIGTERM,
      handle_shutdown_signal as *const () as libc::sighandler_t,
    );
    libc::signal(
      libc::SIGINT,
      handle_shutdown_signal as *const () as libc::sighandler_t,
    );
  }
}

/// Whether a graceful shutdown has been requested (set by [`install_shutdown_handlers`]'s handler).
#[must_use]
pub fn shutdown_requested() -> bool { SHUTDOWN_REQUESTED.load(Ordering::SeqCst) }

/// The dispatcher's **service cache**: a `service_name → Option<Service>` memo shared by the
/// ventilator (populated on a cache miss) and the sink (read on every result), so a `Service` row
/// is looked up from the DB at most once per name. Phase 4 of the dispatcher rationalization
/// replaces the old `Arc<Mutex<HashMap>>` with a sharded `DashMap`, so this near-static,
/// read-mostly cache is no longer behind a single global lock contended on every dispatch.
pub type ServiceCache = DashMap<String, Option<Service>>;

/// The dispatcher's **sandbox cache**: a `corpus_id → Option<sandbox_id>` memo, the corpus-keyed
/// twin of [`ServiceCache`]. `Some(id)` means the corpus is a sandbox (Arm 5) whose result archives
/// are name-scoped by its own id; `None` means an ordinary corpus. Sandbox-ness is immutable per
/// corpus (a corpus is born sandbox-or-not and never flips), so a one-time DB lookup per
/// `corpus_id` is correct forever — no invalidation. The ventilator memoises it on dispatch (it
/// holds a `Backend`), so the sink can scope a result's output path with a lock-free read — no
/// per-result DB hit — keeping a sandbox rerun from overwriting the parent's archives (KNOWN_ISSUES
/// F-6).
pub type SandboxCache = DashMap<i32, Option<i32>>;

/// Rate-limited logging for high-frequency, low-value events — the dispatcher's *discarded*
/// messages (malformed replies/requests, unknown service names, unknown task ids). The dispatcher's
/// request and sink loops would otherwise `warn!` one line per skipped message; under a **sustained
/// flood** (a hostile or buggy peer spamming bad frames) even leveled logging serialises a
/// synchronous write per event and *slows the real pipeline* — a self-inflicted throughput-DoS
/// (KNOWN_ISSUES D-11). This aggregates instead: it counts events and signals an
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
/// rationalization (`docs/archive/DISPATCHER_RATIONALIZATION.md`).
pub const DONE_QUEUE_CAPACITY: usize = 10_000;

/// Whether the in-flight set is saturated and the ventilator should apply backpressure (stop
/// leasing new work and mock-reply). Saturation is inclusive: at the threshold we already hold
/// back, keeping the set bounded *below* [`PROGRESS_QUEUE_HARD_LIMIT`].
pub fn in_flight_saturated(in_flight: usize, max_in_flight: usize) -> bool {
  in_flight >= max_in_flight
}

/// The dispatcher's **in-flight (progress) set**: the dispatched-but-unfinished tasks, keyed by
/// task id. Phase 4 of the dispatcher rationalization replaces the contended
/// `Arc<Mutex<HashMap<i64, TaskProgress>>>` with a sharded, lock-free [`DashMap`] — so the
/// ventilator's lease ([`Self::insert`]), the sink's return ([`Self::remove`]), and the reaper
/// sweep ([`Self::take_expired`]) no longer serialise on one global lock — plus an [`AtomicUsize`]
/// size counter so the per-request backpressure check ([`in_flight_saturated`]) reads the size in
/// **O(1)** without locking or scanning the map.
///
/// The counter is maintained in lock-step with the map *here* (the only place either is mutated),
/// so it converges to exactly the map size; a momentary ±1 skew between a map mutation and its
/// atomic update is harmless for backpressure (self-correcting, never a leak). Shared as
/// `Arc<InFlightSet>` across the ventilator / sink / reaper threads.
#[derive(Default)]
pub struct InFlightSet {
  map: DashMap<i64, TaskProgress>,
  len: AtomicUsize,
}

impl InFlightSet {
  /// A new, empty in-flight set.
  pub fn new() -> Self { Self::default() }

  /// Current number of in-flight tasks — an **O(1)** atomic load (the backpressure hot read), not a
  /// map scan.
  pub fn len(&self) -> usize { self.len.load(Ordering::Acquire) }

  /// Whether the in-flight set is empty.
  pub fn is_empty(&self) -> bool { self.len() == 0 }

  /// Record a dispatched task as in-flight (keyed by `task.id`). Re-inserting the same id (a
  /// re-lease) overwrites without double-counting. Preserves the fail-fast hard-limit backstop: if
  /// the set ever exceeds [`PROGRESS_QUEUE_HARD_LIMIT`] (backpressure failed to hold the line) we
  /// panic → process abort → external restart, rather than grow unbounded.
  pub fn insert(&self, progress_task: TaskProgress) {
    let id = progress_task.task.id;
    if self.map.insert(id, progress_task).is_none() {
      self.len.fetch_add(1, Ordering::AcqRel);
    }
    if self.len() > PROGRESS_QUEUE_HARD_LIMIT {
      panic!(
        "Progress queue is too large: {:?} tasks. Stop the ventilator!",
        self.len()
      );
    }
  }

  /// Remove and return the in-flight task with `taskid` (the sink draining a returned result).
  /// Negative (mock-reply) ids are never tracked, so they short-circuit to `None`; removing an
  /// already-gone task is a no-op that never underflows the counter.
  pub fn remove(&self, taskid: i64) -> Option<TaskProgress> {
    if taskid < 0 {
      // Mock ids are to be skipped
      return None;
    }
    let removed = self.map.remove(&taskid).map(|(_, v)| v);
    if removed.is_some() {
      self.len.fetch_sub(1, Ordering::AcqRel);
    }
    removed
  }

  /// Remove and return every in-flight task past its visibility-timeout deadline (the reaper
  /// sweep).
  pub fn take_expired(&self) -> Vec<TaskProgress> {
    let now = chrono::Utc::now().timestamp();
    let expired_keys: Vec<i64> = self
      .map
      .iter()
      .filter(|entry| entry.value().expected_at() < now)
      .map(|entry| *entry.key())
      .collect();
    let mut expired_tasks = Vec::with_capacity(expired_keys.len());
    for key in expired_keys {
      if let Some((_, task_progress)) = self.map.remove(&key) {
        self.len.fetch_sub(1, Ordering::AcqRel);
        expired_tasks.push(task_progress);
      }
    }
    expired_tasks
  }

  /// Snapshot of the currently in-flight task ids — used on a ventilator restart to **exclude**
  /// these from the `Queued → TODO` crash-recovery reset (`clear_limbo_tasks_except`), so tasks the
  /// sink is still processing are not re-leased mid-flight (KNOWN_ISSUES D-4).
  pub fn ids(&self) -> Vec<i64> { self.map.iter().map(|entry| *entry.key()).collect() }
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
  match backend.mark_done(reports) {
    Err(e) => {
      warn!("mark_done attempt failed: {e:?}");
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
          Err(e) => warn!("mark_done retry failed: {e:?}"),
        };
      }
    },
    _ => {
      success = true;
    },
  }
  if !success {
    return Err(String::from(
      "Database ran away during mark_done persisting.",
    ));
  }
  let request_duration = (chrono::Utc::now() - request_time).num_milliseconds();
  debug!(
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
    error!("done channel closed (finalize thread gone); the manager will abort for a restart");
  }
}

/// The maximum number of dispatch retries before a perpetually-incomplete task is given up on.
/// A task re-dispatched this many times that still never returns a result is treated as a hard
/// failure (`Fatal`) rather than retried forever.
///
/// **1** with the short `lease_timeout_seconds` (~180 s, just above the worker's hard per-document
/// timeout): a task whose worker keeps dying is almost always an unprocessable paper (a fresh
/// recycle-clean worker dies on it too), so 2 retries (3 attempts total) catch the rare
/// transient/worker-induced death and then converge to `Fatal` within a single run — the
/// `(retries+1)×180` backoff cumulates to (1+2+3)×180 ≈ 1080 s, well inside a corpus pass —
/// instead of the old 4 retries × 3600 s that stranded the task for hours.
pub const MAX_DISPATCH_RETRIES: i64 = 1;

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

/// Tally of one reaping pass — the dispatcher's re-lease / dead-letter health signal (Arm 8
/// observability). Returned by [`reap_expired_into`] so the ventilator can log it without the
/// reaper itself reaching for tracing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReapSummary {
  /// Timed-out tasks re-queued for another dispatch attempt (retry budget remained).
  pub requeued: usize,
  /// Timed-out tasks given up on and reported `Fatal` (retry budget exhausted) — dead-letters.
  pub dead_lettered: usize,
}

/// Reaps timed-out in-flight tasks and routes each to **its own service's** dispatch queue (a
/// retry) or the done queue (a `Fatal`). Decoupled from the refetch path so the in-flight set
/// drains even under sustained backpressure (KNOWN_ISSUES D-6); routing by `task.service_id` rather
/// than the requesting service fixes the latent cross-service requeue bug. Returns a
/// [`ReapSummary`] of what it did (re-leases vs dead-letters) for the health log.
pub fn reap_expired_into(
  queues: &mut HashMap<i32, Vec<TaskProgress>>,
  in_flight: &InFlightSet,
  done_tx: &SyncSender<TaskReport>,
) -> ReapSummary {
  let mut summary = ReapSummary::default();
  for expired in in_flight.take_expired() {
    match classify_expired(expired) {
      ExpiredOutcome::Requeue(task_progress) => {
        queues
          .entry(task_progress.task.service_id)
          .or_default()
          .push(task_progress);
        summary.requeued += 1;
      },
      ExpiredOutcome::Fatal(report) => {
        send_done(done_tx, report);
        summary.dead_lettered += 1;
      },
    }
  }
  summary
}

/// Memoized getter for a `Service` record from the backend, populating the shared [`ServiceCache`]
/// on a miss. The `or_insert_with` holds only the relevant DashMap *shard* during the one-time DB
/// lookup (vs. the old whole-map `Mutex`), so concurrent lookups of *other* services are unblocked.
pub fn get_sync_service(
  service_name: &str,
  services: &ServiceCache,
  backend: &mut Backend,
) -> Option<Service> {
  services
    .entry(service_name.to_string())
    .or_insert_with(|| Service::find_by_name(service_name, &mut backend.connection).ok())
    .value()
    .clone()
}

/// Getter for a `Service` from the shared [`ServiceCache`], with no DB access (a `None` entry means
/// a prior lookup found no such service).
pub fn get_service(service_name: &str, services: &ServiceCache) -> Option<Service> {
  services
    .get(service_name)
    .and_then(|entry| entry.value().clone())
}

/// Memoised getter for a corpus's sandbox id, populating the shared [`SandboxCache`] from the
/// backend on a miss (the ventilator's DB-holding counterpart to [`get_sandbox_id`]). One lookup
/// per `corpus_id` ever dispatched — bounded by the corpus count, not the task count.
pub fn get_sync_sandbox_id(
  corpus_id: i32,
  sandboxes: &SandboxCache,
  backend: &mut Backend,
) -> Option<i32> {
  *sandboxes
    .entry(corpus_id)
    .or_insert_with(|| {
      crate::models::Corpus::find_by_id(corpus_id, &mut backend.connection)
        .ok()
        .and_then(|corpus| corpus.sandbox_id())
    })
    .value()
}

/// Getter for a corpus's sandbox id from the shared [`SandboxCache`], with no DB access — the
/// sink's read on the result path. An absent entry yields `None` (treat as an ordinary corpus); the
/// ventilator always memoises a task's corpus on dispatch, before its result can return, so the
/// entry is present by the time the sink looks.
pub fn get_sandbox_id(corpus_id: i32, sandboxes: &SandboxCache) -> Option<i32> {
  sandboxes.get(&corpus_id).and_then(|entry| *entry.value())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::DispatcherConfig;
  use crate::models::Task;
  use std::sync::Arc;

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
  fn reap_expired_into_tallies_requeue_and_dead_letter() {
    use std::sync::mpsc::sync_channel;
    // created_at = 0 (epoch) ⇒ every task's deadline is far in the past ⇒ all expired.
    let set = InFlightSet::new();
    set.insert(dummy_progress(1)); // retries 0 → within budget → re-queue
    set.insert(dummy_progress(2)); // retries 0 → within budget → re-queue
    set.insert(TaskProgress {
      retries: MAX_DISPATCH_RETRIES + 1, // budget exhausted → dead-letter Fatal
      ..dummy_progress(3)
    });
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, done_rx) = sync_channel(16);
    let summary = reap_expired_into(&mut queues, &set, &done_tx);
    assert_eq!(summary.requeued, 2, "two within retry budget are re-leased");
    assert_eq!(summary.dead_lettered, 1, "one over budget is dead-lettered");
    assert_eq!(set.len(), 0, "the in-flight set fully drained");
    assert_eq!(
      done_rx.try_iter().count(),
      1,
      "the dead-letter Fatal reached the done channel"
    );
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
  fn inflight_len_tracks_dispatch_and_drain() {
    let set = InFlightSet::new();
    assert_eq!(set.len(), 0);
    set.insert(dummy_progress(1));
    set.insert(dummy_progress(2));
    assert_eq!(set.len(), 2, "two tasks now in flight");
    // The sink draining a returned result shrinks the in-flight set — how backpressure recovers.
    set.remove(1);
    assert_eq!(set.len(), 1);
  }

  #[test]
  fn inflight_count_ignores_duplicate_insert_and_negative_remove() {
    // The O(1) size counter must stay exactly equal to the map size under the edge cases the
    // dispatcher actually hits: a re-leased task (same id inserted again) and the sink's mock-id
    // (negative) results. A drift here would mis-fire backpressure or leak toward the hard bound.
    let set = InFlightSet::new();
    set.insert(dummy_progress(7));
    set.insert(dummy_progress(7)); // same id → overwrite, not a second entry
    assert_eq!(
      set.len(),
      1,
      "re-inserting the same task id does not double-count"
    );
    assert!(
      set.remove(-1).is_none(),
      "negative (mock) ids are never tracked"
    );
    assert_eq!(set.len(), 1);
    assert!(set.remove(7).is_some());
    assert_eq!(set.len(), 0);
    assert!(set.remove(7).is_none(), "removing a gone task is a no-op");
    assert_eq!(
      set.len(),
      0,
      "a redundant remove does not underflow the counter"
    );
  }

  #[test]
  fn inflight_handles_200_concurrent_tasks_with_consistent_count() {
    // Deployment sizing is ~200 concurrent workers; the in-flight set must absorb 200 simultaneous
    // leases from many threads (and then 200 concurrent drains) without losing entries or drifting
    // its O(1) size counter — the property the sharded DashMap + AtomicUsize must guarantee in
    // place of the old global Mutex. (Dispatcher rationalization phase 4.)
    let set = Arc::new(InFlightSet::new());
    let n: i64 = 200;
    let leasers: Vec<_> = (1..=n)
      .map(|id| {
        let set = set.clone();
        thread::spawn(move || set.insert(dummy_progress(id)))
      })
      .collect();
    for h in leasers {
      h.join().unwrap();
    }
    assert_eq!(
      set.len(),
      n as usize,
      "all 200 concurrent leases tracked, counter consistent"
    );
    assert_eq!(
      set.ids().len(),
      n as usize,
      "every id is present in the map"
    );
    let drainers: Vec<_> = (1..=n)
      .map(|id| {
        let set = set.clone();
        thread::spawn(move || set.remove(id))
      })
      .collect();
    let drained = drainers
      .into_iter()
      .filter_map(|h| h.join().unwrap())
      .count();
    assert_eq!(
      drained, n as usize,
      "each of the 200 tasks drained exactly once"
    );
    assert_eq!(
      set.len(),
      0,
      "counter back to zero after a full concurrent drain"
    );
    assert!(set.is_empty());
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
    let progress = InFlightSet::new();
    progress.insert(expired_progress(1, 7, 0)); // retriable, service 7
    progress.insert(expired_progress(2, 9, MAX_DISPATCH_RETRIES + 1)); // fatal, svc 9
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
    assert_eq!(progress.len(), 0);
  }
}
