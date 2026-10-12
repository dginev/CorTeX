use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::{debug, error, warn};

use crate::backend::Backend;
use crate::helpers::{NewTaskMessage, TaskProgress, TaskReport, TaskStatus};
use crate::models::{Service, Task};

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

/// One task id's entry in the [`InFlightSet`].
#[derive(Clone, Debug)]
enum TaskSlot {
  /// Leased to a worker — counted by [`InFlightSet::len`].
  InFlight(Leased),
  /// Out of flight, but a late result may still arrive for it (KNOWN_ISSUES D-26): released by
  /// the reaper (re-queued or dead-lettered), or finished by an older attempt while a newer one
  /// still runs.
  Settling(Settling),
}

/// A leased task and what has come back for it so far.
#[derive(Clone, Debug)]
struct Leased {
  /// the lease
  progress: TaskProgress,
  /// results received for the task that are still with their archive writer: the reaper leaves
  /// the task alone until they are decided ([`InFlightSet::note_received`])
  pending: u32,
  /// the current attempt's worker has answered with something that is not a result — an
  /// unusable archive (D-18) or a retryable cause: only the reaper moves the task on, and a
  /// bounded run's sink does not wait for it ([`InFlightSet::awaiting_replies`])
  answered: bool,
}

impl Leased {
  fn new(progress: TaskProgress) -> Self {
    Leased {
      progress,
      pending: 0,
      answered: false,
    }
  }

  /// The attempt in flight (0-based).
  fn current_attempt(&self) -> usize { self.progress.attempt_workers.len().saturating_sub(1) }
}

/// A task out of flight that may still receive a late result: what the sink needs to accept one.
#[derive(Clone, Debug)]
struct Settling {
  /// the task, for the report
  task: Task,
  /// the worker of each dispatch attempt (see [`TaskProgress::attempt_workers`])
  attempt_workers: Vec<String>,
  /// the newest attempt whose result was accepted, with that result's status, if any
  finished: Option<(usize, TaskStatus)>,
  /// whether the reaper dead-lettered the task (reported its stand-in Fatal)
  dead_lettered: bool,
  /// the last retryable cause reported for the task, carried into its queued retry's lease
  last_cause: Vec<NewTaskMessage>,
  /// when to forget it (unix seconds)
  forget_at: i64,
  /// the first attempt of the task's current run (see [`TaskProgress::run_start`]): results of
  /// earlier attempts belong to an earlier run and never land
  run_start: usize,
  /// the retry count of the expired lease, its lease timeout, and whether its retry is queued in
  /// the ventilator — what a restarted ventilator needs to queue it again
  /// ([`InFlightSet::queued_retries`])
  retries: i64,
  lease_timeout_seconds: i64,
  queued: bool,
}

/// Why an accepted result is late (KNOWN_ISSUES D-26) — the sink logs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LateResult {
  /// An older attempt's result while a newer attempt is in flight (the older lease expired and
  /// the task was re-leased). The newer attempt may still report, and would then replace it.
  OlderAttempt,
  /// The reaper had re-queued the task; it is not re-leased now.
  AfterRequeue,
  /// The reaper had dead-lettered the task; this replaces its stand-in Fatal.
  AfterDeadLetter,
  /// A newer attempt than the one whose result was accepted earlier; this replaces it.
  NewerAttempt,
  /// A usable result of an older attempt replacing a newer attempt's accepted failure ("good
  /// result wins").
  ReplacesFailure,
}

/// What the sink may do with a result, decided by [`InFlightSet::claim_result`].
#[derive(Clone, Debug)]
pub enum ResultClaim {
  /// Report it for `task`: it is the task's newest finished attempt.
  Accept {
    /// the task the result is for
    task: Task,
    /// the attempt it belongs to (0-based)
    attempt: usize,
    /// how many attempts were dispatched
    attempts: usize,
    /// `Some` when the result came after its lease expired
    late: Option<LateResult>,
    /// the earlier accepted result this one replaces (its attempt and status), if any — restored
    /// by [`InFlightSet::note_publish_failed`] if this one cannot be published
    replaced: Option<(usize, TaskStatus)>,
  },
  /// Drop it: a result already accepted stands — a newer attempt's, or a usable one that a newer
  /// attempt's failure (`Fatal`/`Invalid`) must not replace.
  Superseded {
    /// the attempt it belongs to
    attempt: usize,
    /// the attempt whose result stands
    finished: usize,
  },
  /// Drop it: it belongs to an earlier run of the task (an operator re-ran it since — D-28).
  Stale {
    /// the attempt it belongs to
    attempt: usize,
  },
  /// No such task in flight or in its late-result window: drop it.
  Unknown,
}

/// What became of a retryable cause ([`InFlightSet::defer_retryable`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryDecision {
  /// The task goes back to the retry budget, with this cause as its last one: the next reap
  /// re-queues it.
  Deferred,
  /// The cause came from the task's last attempt (its budget is spent): the caller publishes its
  /// archive and reports it as a **stand-in** Fatal for `task` — the DB and the archive on disk
  /// agree, and any attempt's late result still replaces it.
  Final {
    /// the task the stand-in is for
    task: Task,
  },
  /// Nothing to retry: the reason, for the log.
  Dropped(&'static str),
}

/// Whether a result's status is a failure (`Fatal`/`Invalid`), which never replaces an accepted
/// usable result (D-26).
fn is_failure(status: TaskStatus) -> bool {
  matches!(status, TaskStatus::Fatal | TaskStatus::Invalid)
}

/// The attempt a reply from `worker` belongs to: the newest attempt dispatched to that worker, or
/// the newest attempt of all when the worker is not among them (an identity the ventilator did not
/// record — attributing it to the newest attempt means it can never overwrite a newer result).
fn attempt_of(attempt_workers: &[String], worker: &str) -> usize {
  attempt_workers
    .iter()
    .rposition(|w| w == worker)
    .unwrap_or_else(|| attempt_workers.len().saturating_sub(1))
}

/// Whether an expired task has spent its retry budget (the reaper dead-letters it).
fn retries_exhausted(progress: &TaskProgress) -> bool { progress.retries > MAX_DISPATCH_RETRIES }

/// Hard ceiling on the tasks held for late results. Past it a reaped task gets no late-result
/// window (it is re-leased or dead-lettered as before), so the memory stays bounded whatever the
/// reaping rate.
pub const SETTLING_HARD_LIMIT: usize = 50_000;

/// The default late-result window when none is configured (seconds; see
/// [`crate::config::DispatcherConfig::late_result_window_seconds`]).
pub const DEFAULT_LATE_RESULT_WINDOW_SECONDS: i64 = 3600;

/// The dispatcher's **in-flight (progress) set**: the dispatched-but-unfinished tasks, keyed by
/// task id. Phase 4 of the dispatcher rationalization replaces the contended
/// `Arc<Mutex<HashMap<i64, TaskProgress>>>` with a sharded, lock-free [`DashMap`] — so the
/// ventilator's lease ([`Self::lease`]), the sink's return ([`Self::claim_result`]), and the reaper
/// sweep ([`Self::take_expired`]) no longer serialise on one global lock — plus an [`AtomicUsize`]
/// size counter so the per-request backpressure check ([`in_flight_saturated`]) reads the size in
/// **O(1)** without locking or scanning the map.
///
/// It also holds, for [`late_result_window`](Self::with_late_result_window) seconds, each task the
/// reaper released (re-queued or dead-lettered), with the worker of every attempt, so the sink can
/// accept a result whose lease expired unless a result that must stand already landed
/// (KNOWN_ISSUES D-26). Those entries are not in flight: [`Self::len`], [`Self::is_empty`] and
/// [`Self::ids`] count only leased tasks. Every transition of one task id happens under its map
/// shard's lock, so a lease, a result and a reap of the same task cannot interleave.
///
/// The counters are maintained in lock-step with the map *here* (the only place any is mutated),
/// so they converge to exactly the map's state; a momentary ±1 skew between a map mutation and its
/// atomic update is harmless for backpressure (self-correcting, never a leak). Shared as
/// `Arc<InFlightSet>` across the ventilator / sink / reaper threads.
pub struct InFlightSet {
  map: DashMap<i64, TaskSlot>,
  len: AtomicUsize,
  answered: AtomicUsize,
  settling: AtomicUsize,
  late_result_window: i64,
}

impl Default for InFlightSet {
  fn default() -> Self { Self::with_late_result_window(DEFAULT_LATE_RESULT_WINDOW_SECONDS) }
}

impl InFlightSet {
  /// A new, empty in-flight set with the default late-result window.
  pub fn new() -> Self { Self::default() }

  /// A new, empty in-flight set that keeps reaped tasks for late results for `seconds` (`<= 0`:
  /// no late results are accepted).
  pub fn with_late_result_window(seconds: i64) -> Self {
    InFlightSet {
      map: DashMap::new(),
      len: AtomicUsize::new(0),
      answered: AtomicUsize::new(0),
      settling: AtomicUsize::new(0),
      late_result_window: seconds,
    }
  }

  /// Current number of in-flight tasks — an **O(1)** atomic load (the backpressure hot read), not a
  /// map scan.
  pub fn len(&self) -> usize { self.len.load(Ordering::Acquire) }

  /// Whether the in-flight set is empty.
  pub fn is_empty(&self) -> bool { self.len() == 0 }

  /// Number of in-flight tasks still awaiting a reply: [`Self::len`] less those whose current
  /// attempt answered with an unusable result or a retryable cause, which only the reaper moves
  /// on. A bounded run's sink stops once dispatching is done and this is zero.
  pub fn awaiting_replies(&self) -> usize {
    self
      .len()
      .saturating_sub(self.answered.load(Ordering::Acquire))
  }

  /// Number of tasks held out of flight for a late result.
  pub fn settling_len(&self) -> usize { self.settling.load(Ordering::Acquire) }

  /// Fail fast past [`PROGRESS_QUEUE_HARD_LIMIT`] (backpressure failed to hold the line): panic →
  /// process abort → external restart, rather than grow unbounded.
  fn check_hard_limit(&self) {
    if self.len() > PROGRESS_QUEUE_HARD_LIMIT {
      panic!(
        "Progress queue is too large: {:?} tasks. Stop the ventilator!",
        self.len()
      );
    }
  }

  /// Account for a slot that leaves the set or is replaced.
  fn note_left(&self, slot: &TaskSlot) {
    match slot {
      TaskSlot::InFlight(leased) => {
        self.len.fetch_sub(1, Ordering::AcqRel);
        if leased.answered {
          self.answered.fetch_sub(1, Ordering::AcqRel);
        }
      },
      TaskSlot::Settling(_) => {
        self.settling.fetch_sub(1, Ordering::AcqRel);
      },
    }
  }

  /// Account for a slot that enters the set.
  fn note_entered(&self, slot: &TaskSlot) {
    match slot {
      TaskSlot::InFlight(leased) => {
        self.len.fetch_add(1, Ordering::AcqRel);
        if leased.answered {
          self.answered.fetch_add(1, Ordering::AcqRel);
        }
      },
      TaskSlot::Settling(_) => {
        self.settling.fetch_add(1, Ordering::AcqRel);
      },
    }
  }

  /// Put `slot` at `id`, keeping the counters in step.
  fn put(&self, id: i64, slot: TaskSlot) {
    self.note_entered(&slot);
    if let Some(previous) = self.map.insert(id, slot) {
      self.note_left(&previous);
    }
  }

  /// Record a dispatched task as in-flight (keyed by `task.id`), unconditionally. Re-inserting the
  /// same id overwrites without double-counting. The ventilator leases through [`Self::lease`].
  pub fn insert(&self, progress_task: TaskProgress) {
    self.put(
      progress_task.task.id,
      TaskSlot::InFlight(Leased::new(progress_task)),
    );
    self.check_hard_limit();
  }

  /// Lease a task: record it in flight, unless it is a re-queued retry that a late result already
  /// finished while it waited in the dispatch queue (KNOWN_ISSUES D-26) — then `false`, and the
  /// caller drops it instead of converting it again. A first attempt (`retries == 0`: a fresh
  /// fetch, e.g. an operator's rerun of the task) is always leased; a held task's earlier
  /// attempts become its older attempts. A retry takes the last retryable cause reported while
  /// it was queued.
  pub fn lease(&self, progress_task: TaskProgress) -> bool {
    use dashmap::mapref::entry::Entry;
    let retry = progress_task.retries > 0;
    let mut progress_task = progress_task;
    let leased = match self.map.entry(progress_task.task.id) {
      Entry::Occupied(mut slot) => {
        let refused = match slot.get() {
          TaskSlot::Settling(held) if retry => {
            if held.finished.is_none() && !held.last_cause.is_empty() {
              progress_task.last_cause = held.last_cause.clone();
            }
            held.finished.is_some()
          },
          // A fresh fetch of a task the set still holds — in flight or held — is an operator's
          // rerun: a new run (D-28). Its earlier attempts stay on record as an earlier run's, so
          // their late results are told apart and never land in it.
          held if !retry => {
            let earlier = match held {
              TaskSlot::InFlight(leased) => &leased.progress.attempt_workers,
              TaskSlot::Settling(settling) => &settling.attempt_workers,
            };
            let mut workers = earlier.clone();
            progress_task.run_start = workers.len();
            workers.append(&mut progress_task.attempt_workers);
            progress_task.attempt_workers = workers;
            false
          },
          // A retry over an in-flight slot whose run began after the retry's: the retry belongs to
          // an earlier run, which a rerun superseded — never let it take the new run's slot.
          TaskSlot::InFlight(current) if retry => {
            current.progress.run_start > progress_task.run_start
          },
          _ => false,
        };
        if refused {
          false
        } else {
          let slot_value = TaskSlot::InFlight(Leased::new(progress_task));
          self.note_entered(&slot_value);
          let previous = slot.insert(slot_value);
          self.note_left(&previous);
          true
        }
      },
      Entry::Vacant(slot) => {
        let slot_value = TaskSlot::InFlight(Leased::new(progress_task));
        self.note_entered(&slot_value);
        slot.insert(slot_value);
        true
      },
    };
    self.check_hard_limit();
    leased
  }

  /// Remove and return the in-flight task with `taskid`. Negative (mock-reply) ids are never
  /// tracked, so they short-circuit to `None`; removing an already-gone task is a no-op that never
  /// underflows the counter, and a task held for a late result is left alone.
  pub fn remove(&self, taskid: i64) -> Option<TaskProgress> {
    if taskid < 0 {
      // Mock ids are to be skipped
      return None;
    }
    let (_, slot) = self
      .map
      .remove_if(&taskid, |_, slot| matches!(slot, TaskSlot::InFlight(_)))?;
    self.note_left(&slot);
    match slot {
      TaskSlot::InFlight(leased) => Some(leased.progress),
      TaskSlot::Settling(_) => None,
    }
  }

  /// The task a result for `taskid` would be reported against — in flight or held for a late
  /// result — without claiming it. The sink checks the result's service and output path against
  /// it first, so an unplaceable result leaves the task exactly where it was (the reaper's).
  pub fn task(&self, taskid: i64) -> Option<Task> {
    if taskid < 0 {
      return None;
    }
    self.map.get(&taskid).map(|slot| match slot.value() {
      TaskSlot::InFlight(leased) => leased.progress.task.clone(),
      TaskSlot::Settling(settling) => settling.task.clone(),
    })
  }

  /// A result for in-flight task `taskid` was received and handed to its archive writer: the
  /// reaper leaves the task alone until the writer decides the result ([`Self::claim_result`],
  /// [`Self::defer_retryable`], [`Self::note_unusable_reply`]), so a result that arrives at the
  /// deadline is not raced by a re-lease.
  pub fn note_received(&self, taskid: i64) {
    if let Some(mut slot) = self.map.get_mut(&taskid)
      && let TaskSlot::InFlight(leased) = slot.value_mut()
    {
      leased.pending += 1;
    }
  }

  /// The worker `worker` replied for in-flight task `taskid` with a result that could not be used
  /// (empty, unreadable, unwritable, unplaceable — D-18): the task stays in flight for the reaper
  /// (retry, then dead-letter). If the reply is the current attempt's, the task awaits no reply.
  /// `received` says whether the reply was recorded by [`Self::note_received`] (an unplaceable
  /// reply never was).
  pub fn note_unusable_reply(&self, taskid: i64, worker: &str, received: bool) {
    if let Some(mut slot) = self.map.get_mut(&taskid)
      && let TaskSlot::InFlight(leased) = slot.value_mut()
    {
      if received {
        leased.pending = leased.pending.saturating_sub(1);
      }
      if attempt_of(&leased.progress.attempt_workers, worker) == leased.current_attempt()
        && !leased.answered
      {
        leased.answered = true;
        self.answered.fetch_add(1, Ordering::AcqRel);
      }
    }
  }

  /// The worker `worker` reported a **retryable** cause for task `taskid` (the harness's
  /// `Fatal:timeout:wallclock`, `Fatal:oom:shed`, `Fatal:crash:unrecorded`, tagged
  /// `Info:cortex:retryable`): not a result to record, but the task's retry budget's business.
  /// For the attempt in flight with budget left, the cause becomes the task's last cause and its
  /// lease is due now: the next reap re-queues it ([`RetryDecision::Deferred`]). On the task's last
  /// attempt the cause is its final record ([`RetryDecision::Final`]): the caller publishes it as a
  /// stand-in, held like an accepted failure. For a task re-queued meanwhile, the newest attempt's
  /// cause rides along to its retry. Otherwise — an older attempt's cause, a task whose result
  /// already landed, or one already dead-lettered — it is dropped.
  pub fn defer_retryable(
    &self,
    taskid: i64,
    worker: &str,
    cause: Vec<NewTaskMessage>,
  ) -> RetryDecision {
    use dashmap::mapref::entry::Entry;
    let Entry::Occupied(mut slot) = self.map.entry(taskid) else {
      return RetryDecision::Dropped("the task is no longer in flight or held");
    };
    match slot.get_mut() {
      TaskSlot::InFlight(leased) => {
        leased.pending = leased.pending.saturating_sub(1);
        if attempt_of(&leased.progress.attempt_workers, worker) != leased.current_attempt() {
          return RetryDecision::Dropped("an older attempt's cause; a newer attempt is running");
        }
        if retries_exhausted(&leased.progress) {
          // The last attempt: the cause is the task's stand-in, accepted as a failure and held for
          // a late usable result ("good result wins"), so a later publish failure restores it —
          // or, with no window or the hold cap reached, taken out of flight like any final result.
          let task = leased.progress.task.clone();
          if self.late_result_window > 0 && self.settling_len() < SETTLING_HARD_LIMIT {
            let held = TaskSlot::Settling(Settling {
              task: task.clone(),
              attempt_workers: leased.progress.attempt_workers.clone(),
              finished: Some((leased.current_attempt(), TaskStatus::Fatal)),
              dead_lettered: true,
              last_cause: Vec::new(),
              forget_at: chrono::Utc::now().timestamp() + self.late_result_window,
              run_start: leased.progress.run_start,
              retries: leased.progress.retries,
              lease_timeout_seconds: leased.progress.lease_timeout_seconds,
              queued: false,
            });
            self.note_entered(&held);
            let previous = slot.insert(held);
            self.note_left(&previous);
          } else {
            // Removed through the entry, under its lock: the reaper cannot dead-letter it between.
            let (_, previous) = slot.remove_entry();
            self.note_left(&previous);
          }
          return RetryDecision::Final { task };
        }
        leased.progress.last_cause = cause;
        // Due now: `expected_at` is measured from the attempt's dispatch.
        leased.progress.created_at = 0;
        if !leased.answered {
          leased.answered = true;
          self.answered.fetch_add(1, Ordering::AcqRel);
        }
        RetryDecision::Deferred
      },
      TaskSlot::Settling(held) => {
        if held.finished.is_some() {
          RetryDecision::Dropped("a result for the task already landed")
        } else if held.dead_lettered {
          RetryDecision::Dropped("the task was already dead-lettered")
        } else if attempt_of(&held.attempt_workers, worker)
          != held.attempt_workers.len().saturating_sub(1)
        {
          RetryDecision::Dropped("an older attempt's cause; the newest attempt's stands")
        } else {
          held.last_cause = cause;
          RetryDecision::Deferred
        }
      },
    }
  }

  /// Decide a result with status `status` for `taskid` from the worker `worker` (KNOWN_ISSUES
  /// D-26): accept it unless a result that must stand already landed — a newer attempt's, or a
  /// usable one that this failure (`Fatal`/`Invalid`) must not replace. An accepted result takes
  /// the task out of flight; if it belongs to an older attempt than the one in flight, the task is
  /// held so the newer attempt's result can still replace it, and a result for a re-queued task
  /// keeps the queued retry from being leased ([`Self::lease`]).
  pub fn claim_result(&self, taskid: i64, worker: &str, status: TaskStatus) -> ResultClaim {
    use dashmap::mapref::entry::Entry;
    if taskid < 0 {
      return ResultClaim::Unknown;
    }
    let now = chrono::Utc::now().timestamp();
    let Entry::Occupied(mut slot) = self.map.entry(taskid) else {
      return ResultClaim::Unknown;
    };
    let held = match slot.get_mut() {
      TaskSlot::InFlight(leased) => {
        let attempts = leased.progress.attempt_workers.len().max(1);
        let attempt = attempt_of(&leased.progress.attempt_workers, worker);
        if attempt < leased.progress.run_start {
          // The slot stays in flight for the new run: release the receipt this result took, or
          // the reaper would never take the new lease (`take_expired` needs `pending == 0`).
          leased.pending = leased.pending.saturating_sub(1);
          return ResultClaim::Stale { attempt };
        }
        let task = leased.progress.task.clone();
        let older = attempt + 1 < attempts;
        // Held for the late-result window when a later result may still replace this one: a newer
        // attempt is running, or this one is a failure that any usable result replaces ("good
        // result wins", user ruling 2026-10-12).
        // A single-attempt failure has nothing that could replace it: not held (the hold cap is
        // for tasks that may still get a better result).
        let hold = ((older || (is_failure(status) && attempts > 1))
          && self.late_result_window > 0
          && self.settling_len() < SETTLING_HARD_LIMIT)
          .then(|| {
            TaskSlot::Settling(Settling {
              task: task.clone(),
              attempt_workers: leased.progress.attempt_workers.clone(),
              finished: Some((attempt, status)),
              dead_lettered: false,
              last_cause: Vec::new(),
              forget_at: now + self.late_result_window,
              run_start: leased.progress.run_start,
              retries: leased.progress.retries,
              lease_timeout_seconds: leased.progress.lease_timeout_seconds,
              queued: false,
            })
          });
        let claim = ResultClaim::Accept {
          task,
          attempt,
          attempts,
          late: older.then_some(LateResult::OlderAttempt),
          replaced: None,
        };
        (hold, claim)
      },
      TaskSlot::Settling(settling) => {
        let attempts = settling.attempt_workers.len().max(1);
        let attempt = attempt_of(&settling.attempt_workers, worker);
        if attempt < settling.run_start {
          return ResultClaim::Stale { attempt };
        }
        // A usable result replaces a failure and a failure never replaces a usable result,
        // whatever attempts produced them ("good result wins"); between results of the same kind,
        // the newer attempt's wins.
        let late = match settling.finished {
          Some((finished, finished_status)) => {
            let replaces = match (is_failure(finished_status), is_failure(status)) {
              (true, false) => true,
              (false, true) => false,
              _ => attempt > finished,
            };
            if !replaces {
              return ResultClaim::Superseded { attempt, finished };
            }
            if attempt > finished {
              LateResult::NewerAttempt
            } else {
              LateResult::ReplacesFailure
            }
          },
          None if settling.dead_lettered => LateResult::AfterDeadLetter,
          None => LateResult::AfterRequeue,
        };
        let replaced = settling.finished;
        settling.finished = Some((attempt, status));
        settling.forget_at = now + self.late_result_window;
        return ResultClaim::Accept {
          task: settling.task.clone(),
          attempt,
          attempts,
          late: Some(late),
          replaced,
        };
      },
    };
    let (hold, claim) = held;
    match hold {
      Some(settling) => {
        self.note_entered(&settling);
        let previous = slot.insert(settling);
        self.note_left(&previous);
      },
      None => {
        let (_, previous) = slot.remove_entry();
        self.note_left(&previous);
      },
    }
    claim
  }

  /// The accepted result for held task `taskid` could not be published: restore the result it
  /// replaced (`replaced`, from [`ResultClaim::Accept`]), or, when it replaced none, leave the
  /// task open for any later result — a publish failure never costs the task a good result.
  pub fn note_publish_failed(&self, taskid: i64, replaced: Option<(usize, TaskStatus)>) {
    if let Some(mut slot) = self.map.get_mut(&taskid)
      && let TaskSlot::Settling(held) = slot.value_mut()
    {
      held.finished = replaced;
    }
  }

  /// Take every in-flight task past its visibility-timeout deadline (the reaper sweep), except one
  /// whose received result is still with its writer. Each one stays in the set, out of flight, for
  /// the late-result window (unless the window is off or [`SETTLING_HARD_LIMIT`] is reached): a
  /// result of any of its attempts may still land there.
  pub fn take_expired(&self) -> Vec<TaskProgress> {
    let now = chrono::Utc::now().timestamp();
    let due = |slot: &TaskSlot| {
      matches!(slot, TaskSlot::InFlight(leased)
        if leased.pending == 0 && leased.progress.expected_at() < now)
    };
    let expired_keys: Vec<i64> = self
      .map
      .iter()
      .filter(|entry| due(entry.value()))
      .map(|entry| *entry.key())
      .collect();
    let mut expired_tasks = Vec::with_capacity(expired_keys.len());
    for key in expired_keys {
      use dashmap::mapref::entry::Entry;
      let Entry::Occupied(mut slot) = self.map.entry(key) else {
        continue;
      };
      // Claimed, re-leased or received since the scan?
      let progress = match slot.get() {
        TaskSlot::InFlight(leased) if due(slot.get()) => leased.progress.clone(),
        _ => continue,
      };
      if self.late_result_window > 0 && self.settling_len() < SETTLING_HARD_LIMIT {
        let held = TaskSlot::Settling(Settling {
          task: progress.task.clone(),
          attempt_workers: progress.attempt_workers.clone(),
          finished: None,
          dead_lettered: retries_exhausted(&progress),
          last_cause: progress.last_cause.clone(),
          forget_at: now + self.late_result_window,
          run_start: progress.run_start,
          retries: progress.retries,
          lease_timeout_seconds: progress.lease_timeout_seconds,
          // A retry the reaper is about to queue (`classify_expired` requeues what it does not
          // dead-letter).
          queued: !retries_exhausted(&progress),
        });
        self.note_entered(&held);
        let previous = slot.insert(held);
        self.note_left(&previous);
      } else {
        let (_, previous) = slot.remove_entry();
        self.note_left(&previous);
      }
      expired_tasks.push(progress);
    }
    expired_tasks
  }

  /// Forget the tasks whose late-result window has passed. Returns how many. Run on the reaper's
  /// cadence.
  pub fn forget_settled(&self) -> usize {
    let now = chrono::Utc::now().timestamp();
    let mut forgotten = 0;
    self.map.retain(|_, slot| match slot {
      TaskSlot::Settling(settling) if settling.forget_at <= now => {
        self.settling.fetch_sub(1, Ordering::AcqRel);
        forgotten += 1;
        false
      },
      _ => true,
    });
    forgotten
  }

  /// The retries the reaper queued in the ventilator that no lease or late result has taken yet,
  /// rebuilt for a **restarted** ventilator (its queue died with it) to queue again rather than
  /// reset to `TODO`: a reset task would be re-fetched as a new run, and its earlier attempts'
  /// late results dropped as stale (D-28). Due at once (`created_at` 0).
  pub fn queued_retries(&self) -> Vec<TaskProgress> {
    self
      .map
      .iter()
      .filter_map(|entry| match entry.value() {
        TaskSlot::Settling(held) if held.queued && held.finished.is_none() => Some(TaskProgress {
          task: held.task.clone(),
          created_at: 0,
          retries: held.retries + 1,
          lease_timeout_seconds: held.lease_timeout_seconds,
          attempt_workers: held.attempt_workers.clone(),
          last_cause: held.last_cause.clone(),
          run_start: held.run_start,
        }),
        _ => None,
      })
      .collect()
  }

  /// Whether `worker` worked an attempt of `taskid` that the set still holds — so a fresh fetch
  /// leased to it would start a new run whose results could not be told from the old run's.
  pub fn worked_earlier_run(&self, taskid: i64, worker: &str) -> bool {
    self.map.get(&taskid).is_some_and(|slot| {
      let workers = match slot.value() {
        TaskSlot::InFlight(leased) => &leased.progress.attempt_workers,
        TaskSlot::Settling(held) => &held.attempt_workers,
      };
      workers.iter().any(|w| w == worker)
    })
  }

  /// The task ids a restarted ventilator must not reset to `TODO`: every task in flight (its
  /// result may still come) and every task held for late results — a queued retry (queued again,
  /// [`Self::queued_retries`]), or one whose report (a late result, a stand-in) may not be written
  /// yet: a reset row is no longer at the report's lease mark, so finalize would drop it (D-28).
  pub fn kept_on_restart(&self) -> Vec<i64> { self.map.iter().map(|entry| *entry.key()).collect() }

  /// Snapshot of the currently in-flight task ids — used on a ventilator restart to **exclude**
  /// these from the `Queued → TODO` crash-recovery reset (`clear_limbo_tasks_except`), so tasks the
  /// sink is still processing are not re-leased mid-flight (KNOWN_ISSUES D-4).
  pub fn ids(&self) -> Vec<i64> {
    self
      .map
      .iter()
      .filter(|entry| matches!(entry.value(), TaskSlot::InFlight(_)))
      .map(|entry| *entry.key())
      .collect()
  }
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
      warn!(error = ?e, batch = reports.len(), "mark_done attempt failed");
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
          Err(e) => warn!(error = ?e, attempt = retries, "mark_done retry failed"),
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
    count = reports.len(),
    took_ms = request_duration,
    "mark_done: persisted batch to DB"
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
/// **1** with the short `lease_timeout_seconds` (~240 s, just above the worker's 180 s per-document
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
/// [`MAX_DISPATCH_RETRIES`] dispatches) or give up and report it `Fatal` — a stand-in that any
/// attempt's late result replaces. Its message is the last retryable cause reported for the task
/// (e.g. the harness's `Fatal:timeout:wallclock`), or `cortex` / `never_completed_with_retries`
/// when no attempt reported one.
pub fn classify_expired(expired: TaskProgress) -> ExpiredOutcome {
  if retries_exhausted(&expired) {
    let task_id = expired.task.id;
    let messages = if expired.last_cause.is_empty() {
      vec![NewTaskMessage::new(
        task_id,
        "fatal",
        "cortex".to_string(),
        crate::helpers::NEVER_COMPLETED_WHAT.to_string(),
        String::new(),
      )]
    } else {
      expired.last_cause
    };
    ExpiredOutcome::Fatal(TaskReport {
      task: expired.task,
      status: TaskStatus::Fatal,
      messages,
      stand_in: true,
    })
  } else {
    ExpiredOutcome::Requeue(TaskProgress {
      task: expired.task,
      // Re-stamped when the retry is leased (D-27).
      created_at: expired.created_at,
      retries: expired.retries + 1,
      // D-17: a re-lease keeps the per-task lease captured at first dispatch.
      lease_timeout_seconds: expired.lease_timeout_seconds,
      // D-26: the workers of the earlier attempts, to attribute their late results.
      attempt_workers: expired.attempt_workers,
      // The last retryable cause, for the dead-letter if the retries run out.
      last_cause: expired.last_cause,
      // A retry stays in its run.
      run_start: expired.run_start,
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
  /// Reaped tasks whose late-result window passed this pass (KNOWN_ISSUES D-26).
  pub forgotten: usize,
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
  let mut summary = ReapSummary {
    forgotten: in_flight.forget_settled(),
    ..ReapSummary::default()
  };
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
///
/// Fails closed: a failed lookup is **not** cached and yields `None` (unknown), so the sink derives
/// no result path and hands the task back to the in-flight set, where the reaper retries it
/// (re-attempting this lookup) or dead-letters it.
/// Memoising a failure as "ordinary" would route a sandbox's results over the parent's published
/// archives for the dispatcher's lifetime.
pub fn get_sync_sandbox_id(
  corpus_id: i32,
  sandboxes: &SandboxCache,
  backend: &mut Backend,
) -> Option<Option<i32>> {
  if let Some(known) = get_sandbox_id(corpus_id, sandboxes) {
    return Some(known);
  }
  match crate::models::Corpus::find_by_id(corpus_id, &mut backend.connection) {
    Ok(corpus) => {
      let sandbox_id = corpus.sandbox_id();
      sandboxes.insert(corpus_id, sandbox_id);
      Some(sandbox_id)
    },
    Err(error) => {
      warn!(corpus_id, %error, "ventilator: corpus sandbox lookup failed; left unknown");
      None
    },
  }
}

/// Getter for a corpus's sandbox id from the shared [`SandboxCache`], with no DB access — the
/// sink's read on the result path. `Some(None)` is an ordinary corpus, `Some(Some(id))` a sandbox,
/// and `None` **unknown** (the lookup failed): the sink then writes nothing and hands the task
/// back to the reaper, never guessing the parent's archive name.
pub fn get_sandbox_id(corpus_id: i32, sandboxes: &SandboxCache) -> Option<Option<i32>> {
  sandboxes.get(&corpus_id).map(|entry| *entry.value())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::DispatcherConfig;
  use crate::models::Task;
  use std::sync::Arc;

  fn dummy_progress(id: i64) -> TaskProgress {
    TaskProgress {
      run_start: 0,
      last_cause: Vec::new(),
      task: Task {
        id,
        service_id: 1,
        corpus_id: 1,
        status: 0,
        entry: String::new(),
      },
      created_at: 0,
      retries: 0,
      lease_timeout_seconds: 3600,
      attempt_workers: Vec::new(),
    }
  }

  #[test]
  fn reaper_honors_per_service_lease_timeouts() {
    // D-17: two tasks both dispatched 100 s ago — one on a fast service (60 s lease → expired) and
    // one on a slow service (3600 s lease → still valid). `take_expired` must reap only the
    // short-lease task, so one dispatcher serves fast and slow workers without false-reaping the
    // slow one mid-conversion.
    let now = chrono::Utc::now().timestamp();
    let set = InFlightSet::new();
    let mut short = dummy_progress(1);
    short.created_at = now - 100;
    short.lease_timeout_seconds = 60; // expected_at = now - 40 → expired
    set.insert(short);
    let mut long = dummy_progress(2);
    long.created_at = now - 100;
    long.lease_timeout_seconds = 3600; // expected_at = now + 3500 → still leased
    set.insert(long);
    let expired = set.take_expired();
    assert_eq!(expired.len(), 1, "only the short-lease task expired");
    assert_eq!(
      expired[0].task.id, 1,
      "the short-lease (fast-service) task is reaped; the slow service's long lease is honored"
    );
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

  #[test]
  fn a_failed_sandbox_lookup_is_not_cached_as_an_ordinary_corpus() {
    // A corpus id the DB can't resolve stands in for any lookup failure. Caching it as "ordinary"
    // would route a sandbox's results to the parent's published `<service>.zip` for the
    // dispatcher's lifetime.
    let sandboxes = SandboxCache::new();
    let mut backend = crate::backend::testdb();
    let _ = get_sync_sandbox_id(i32::MAX, &sandboxes, &mut backend);
    assert!(
      !sandboxes.contains_key(&i32::MAX),
      "an unresolved corpus must stay uncached (unknown), not be memoised as ordinary"
    );
  }

  #[test]
  fn the_sink_distinguishes_an_unknown_corpus_from_an_ordinary_one() {
    // Unknown (the ventilator's lookup failed) must not read as "ordinary": that would write a
    // sandbox's result over the parent's published archive. `None` = unknown, so the sink derives
    // no path and hands the task back to the reaper.
    let sandboxes = SandboxCache::new();
    sandboxes.insert(1, None);
    sandboxes.insert(2, Some(2));
    assert_eq!(get_sandbox_id(1, &sandboxes), Some(None), "cached ordinary");
    assert_eq!(
      get_sandbox_id(2, &sandboxes),
      Some(Some(2)),
      "cached sandbox"
    );
    assert_eq!(get_sandbox_id(3, &sandboxes), None, "unknown");
  }

  /// A task leased to the workers `workers` in turn (its current attempt is the last), dispatched
  /// long ago so the reaper takes it.
  fn leased_to(id: i64, workers: &[&str]) -> TaskProgress {
    TaskProgress {
      retries: workers.len() as i64 - 1,
      attempt_workers: workers.iter().map(|w| w.to_string()).collect(),
      ..dummy_progress(id)
    }
  }

  fn accepted(claim: ResultClaim) -> (usize, Option<LateResult>) {
    match claim {
      ResultClaim::Accept { attempt, late, .. } => (attempt, late),
      other => panic!("expected the result to be accepted, got {other:?}"),
    }
  }

  #[test]
  fn the_lease_clock_starts_at_dispatch_not_at_fetch() {
    // D-27: a task fetched into the dispatch queue long ago (created_at = epoch) must get a full
    // lease from the moment it is leased, not be reaped as already expired while it converts.
    let set = InFlightSet::new();
    let mut queue = vec![dummy_progress(1)];
    let leased = crate::dispatcher::ventilator::lease_next(&mut queue, &set, "w0")
      .expect("the queued task is leased");
    let now = chrono::Utc::now().timestamp();
    assert!(leased.created_at >= now - 1, "stamped at dispatch");
    assert_eq!(leased.attempt_workers, vec!["w0".to_string()]);
    assert_eq!(set.len(), 1);
    assert!(
      set.take_expired().is_empty(),
      "a just-leased task is not expired"
    );
  }

  #[test]
  fn a_result_after_a_requeue_is_accepted_and_the_retry_is_not_leased() {
    // The lease expired and the reaper re-queued the task; then attempt 0's result arrives.
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0"]));
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(4);
    assert_eq!(reap_expired_into(&mut queues, &set, &done_tx).requeued, 1);
    assert_eq!((set.len(), set.settling_len()), (0, 1));
    assert_eq!(
      accepted(set.claim_result(1, "w0", TaskStatus::NoProblem)),
      (0, Some(LateResult::AfterRequeue))
    );
    // The queued retry is dropped rather than converted again.
    let queue = queues.get_mut(&1).expect("re-queued to its service");
    assert!(crate::dispatcher::ventilator::lease_next(queue, &set, "w1").is_none());
    assert!(queue.is_empty());
    assert_eq!(set.len(), 0);
    assert_eq!(done_rx.try_iter().count(), 0, "no dead-letter");
  }

  #[test]
  fn a_result_after_a_dead_letter_replaces_never_completed_and_stands() {
    // Three attempts, all expired: the reaper reports never_completed. Then attempt 2's result
    // arrives (accepted: it replaces the Fatal), and after it attempt 0's (superseded).
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0", "w1", "w2"]));
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(4);
    assert_eq!(
      reap_expired_into(&mut queues, &set, &done_tx).dead_lettered,
      1
    );
    let dead_letter: Vec<TaskReport> = done_rx.try_iter().collect();
    assert_eq!(dead_letter.len(), 1);
    assert_eq!(
      accepted(set.claim_result(1, "w2", TaskStatus::NoProblem)),
      (2, Some(LateResult::AfterDeadLetter))
    );
    match set.claim_result(1, "w0", TaskStatus::NoProblem) {
      ResultClaim::Superseded { attempt, finished } => assert_eq!((attempt, finished), (0, 2)),
      other => panic!("an older attempt must not overwrite a newer result, got {other:?}"),
    }
    // A repeat of the accepted attempt is not accepted twice.
    assert!(matches!(
      set.claim_result(1, "w2", TaskStatus::NoProblem),
      ResultClaim::Superseded { .. }
    ));
  }

  #[test]
  fn an_older_attempt_finishing_first_is_replaced_by_the_newer_one() {
    // Attempt 0 expired, the task was re-leased to w1 (in flight), then w0's late result arrives:
    // accepted, and the task is held so w1's result can still land — and replace it.
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0"]));
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, _done_rx) = std::sync::mpsc::sync_channel(4);
    reap_expired_into(&mut queues, &set, &done_tx);
    let queue = queues.get_mut(&1).unwrap();
    crate::dispatcher::ventilator::lease_next(queue, &set, "w1").expect("re-leased");
    assert_eq!((set.len(), set.settling_len()), (1, 0));
    assert_eq!(
      accepted(set.claim_result(1, "w0", TaskStatus::NoProblem)),
      (0, Some(LateResult::OlderAttempt))
    );
    assert_eq!((set.len(), set.settling_len()), (0, 1));
    assert_eq!(
      accepted(set.claim_result(1, "w1", TaskStatus::NoProblem)),
      (1, Some(LateResult::NewerAttempt))
    );
    assert!(matches!(
      set.claim_result(1, "w0", TaskStatus::NoProblem),
      ResultClaim::Superseded { .. }
    ));
  }

  #[test]
  fn a_fresh_fetch_of_a_finished_task_is_leased() {
    // An operator re-runs a task whose late result was accepted, within the window: the fresh
    // fetch (a first attempt) is leased; only the old queued retry is refused.
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0"]));
    let _ = set.take_expired();
    accepted(set.claim_result(1, "w0", TaskStatus::NoProblem));
    let mut retry = leased_to(1, &["w0", "w1"]);
    retry.created_at = chrono::Utc::now().timestamp();
    assert!(!set.lease(retry), "the queued retry is refused");
    let mut fresh = leased_to(1, &["w2"]);
    fresh.created_at = chrono::Utc::now().timestamp();
    assert!(set.lease(fresh), "a rerun's fresh lease is not");
    assert_eq!((set.len(), set.settling_len()), (1, 0));
    // The old run's attempt stays on record, so its late result is told apart and dropped (D-28).
    assert!(matches!(
      set.claim_result(1, "w0", TaskStatus::NoProblem),
      ResultClaim::Stale { attempt: 0 }
    ));
    assert_eq!(
      accepted(set.claim_result(1, "w2", TaskStatus::NoProblem)).0,
      1
    );
  }

  #[test]
  fn a_newer_attempt_finishing_first_is_never_overwritten() {
    // The in-flight attempt 1 (w1) reports first; attempt 0's late result is then dropped.
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0", "w1"]));
    assert_eq!(
      accepted(set.claim_result(1, "w1", TaskStatus::NoProblem)),
      (1, None)
    );
    assert_eq!((set.len(), set.settling_len()), (0, 0));
    assert!(matches!(
      set.claim_result(1, "w0", TaskStatus::NoProblem),
      ResultClaim::Unknown
    ));
  }

  #[test]
  fn two_attempts_on_one_worker_attribute_to_the_newer() {
    // A harness slot keeps its identity across respawns, so one worker can hold two attempts of a
    // task; its result belongs to the newer one.
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0", "w1", "w0"]));
    assert_eq!(
      accepted(set.claim_result(1, "w0", TaskStatus::NoProblem)),
      (2, None)
    );
  }

  #[test]
  fn without_a_late_result_window_reaped_tasks_are_forgotten() {
    let set = InFlightSet::with_late_result_window(0);
    set.insert(leased_to(1, &["w0"]));
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, _done_rx) = std::sync::mpsc::sync_channel(4);
    reap_expired_into(&mut queues, &set, &done_tx);
    assert_eq!(set.settling_len(), 0);
    assert!(matches!(
      set.claim_result(1, "w0", TaskStatus::NoProblem),
      ResultClaim::Unknown
    ));
  }

  #[test]
  fn a_passed_late_result_window_is_forgotten_on_the_reap() {
    let set = InFlightSet::new();
    set.map.insert(
      1,
      TaskSlot::Settling(Settling {
        task: dummy_progress(1).task,
        attempt_workers: vec!["w0".into()],
        finished: None,
        dead_lettered: true,
        last_cause: Vec::new(),
        forget_at: 0,
        run_start: 0,
        retries: 2,
        lease_timeout_seconds: 240,
        queued: false,
      }),
    );
    set.settling.fetch_add(1, Ordering::AcqRel);
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, _done_rx) = std::sync::mpsc::sync_channel(4);
    assert_eq!(reap_expired_into(&mut queues, &set, &done_tx).forgotten, 1);
    assert_eq!(set.settling_len(), 0);
    assert!(matches!(
      set.claim_result(1, "w0", TaskStatus::NoProblem),
      ResultClaim::Unknown
    ));
  }

  #[test]
  fn an_unusable_reply_leaves_the_task_in_flight_but_answered() {
    // D-18: an empty / unreadable result is no verdict — the reaper still retries the task — but
    // the task awaits no reply, so a bounded run's sink can finish.
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0"])
    });
    assert_eq!((set.len(), set.awaiting_replies()), (1, 1));
    set.note_unusable_reply(1, "w0", true);
    set.note_unusable_reply(1, "w0", true);
    assert_eq!((set.len(), set.awaiting_replies()), (1, 0));
    assert_eq!(set.ids(), vec![1]);
    // A usable reply after all is still accepted.
    assert_eq!(
      accepted(set.claim_result(1, "w0", TaskStatus::NoProblem)),
      (0, None)
    );
    assert_eq!((set.len(), set.awaiting_replies()), (0, 0));
    // And the reaper still takes an answered task.
    set.insert(leased_to(2, &["w0"]));
    set.note_unusable_reply(2, "w0", true);
    assert_eq!(set.take_expired().len(), 1);
    assert_eq!(
      (set.len(), set.awaiting_replies(), set.settling_len()),
      (0, 0, 1)
    );
  }

  #[test]
  fn held_tasks_are_not_in_flight() {
    // Backpressure, the sink's drain check and the limbo reset see only leased tasks.
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0"]));
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(2, &["w0"])
    });
    let _ = set.take_expired();
    assert_eq!(set.len(), 1);
    assert_eq!(set.ids(), vec![2]);
    assert!(
      set.remove(1).is_none(),
      "a held task is not removed as in flight"
    );
    assert!(set.task(1).is_some(), "but a result can still find it");
  }

  /// `(category, what)` of each message, for comparing causes.
  fn whats(messages: &[NewTaskMessage]) -> Vec<(String, String)> {
    use crate::models::LogRecord;
    messages
      .iter()
      .map(|m| (m.category().to_string(), m.what().to_string()))
      .collect()
  }

  /// The messages of a harness's retryable timeout report for task `id`.
  fn timeout_cause(id: i64) -> Vec<NewTaskMessage> {
    vec![NewTaskMessage::new(
      id,
      "fatal",
      "timeout".to_string(),
      "wallclock".to_string(),
      "the watchdog ended the worker".to_string(),
    )]
  }

  #[test]
  fn a_retryable_cause_retries_and_a_success_then_stands() {
    // Retry, then cause (user ruling 2026-10-12): the harness reports attempt 0 timed out; the
    // task goes back to its budget, is re-leased, and attempt 1's result is the one recorded.
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0"])
    });
    set.note_received(1);
    assert!(
      set.take_expired().is_empty(),
      "not due, and a result is with its writer"
    );
    assert_eq!(
      set.defer_retryable(1, "w0", timeout_cause(1)),
      RetryDecision::Deferred
    );
    assert_eq!(
      set.awaiting_replies(),
      0,
      "answered: only the reaper moves it on"
    );
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(4);
    assert_eq!(
      reap_expired_into(&mut queues, &set, &done_tx).requeued,
      1,
      "due at the next reap"
    );
    let queue = queues.get_mut(&1).unwrap();
    let retry = crate::dispatcher::ventilator::lease_next(queue, &set, "w1").expect("re-leased");
    assert_eq!(
      whats(&retry.last_cause),
      whats(&timeout_cause(1)),
      "the cause rides along"
    );
    assert_eq!(
      accepted(set.claim_result(1, "w1", TaskStatus::Warning)),
      (1, None)
    );
    assert_eq!(done_rx.try_iter().count(), 0);
  }

  #[test]
  fn a_retryable_cause_on_the_last_attempt_is_its_stand_in() {
    // The budget is spent: the cause is the task's final record, a stand-in (published with its
    // archive by the writer) that a real late result still replaces.
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0", "w1", "w2"])
    });
    assert!(matches!(
      set.defer_retryable(1, "w2", timeout_cause(1)),
      RetryDecision::Final { .. }
    ));
    assert_eq!(
      (set.len(), set.awaiting_replies(), set.settling_len()),
      (0, 0, 1)
    );
    assert_eq!(
      accepted(set.claim_result(1, "w0", TaskStatus::NoProblem)),
      (0, Some(LateResult::ReplacesFailure))
    );
  }

  #[test]
  fn a_last_attempt_stand_in_is_restored_after_a_failed_publish() {
    // The stand-in is an accepted failure: a later good result replaces it, and if that one cannot
    // be published, the stand-in (its archive on disk) is what stands again.
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0", "w1", "w2"])
    });
    assert!(matches!(
      set.defer_retryable(1, "w2", timeout_cause(1)),
      RetryDecision::Final { .. }
    ));
    let replaced = match set.claim_result(1, "w0", TaskStatus::NoProblem) {
      ResultClaim::Accept { late, replaced, .. } => {
        assert_eq!(late, Some(LateResult::ReplacesFailure));
        replaced
      },
      other => panic!("{other:?}"),
    };
    assert_eq!(replaced, Some((2, TaskStatus::Fatal)));
    set.note_publish_failed(1, replaced);
    // The stand-in stands: another failure cannot replace it, a usable result still can.
    assert!(matches!(
      set.claim_result(1, "w1", TaskStatus::Fatal),
      ResultClaim::Superseded { finished: 2, .. }
    ));
    assert_eq!(
      accepted(set.claim_result(1, "w1", TaskStatus::Warning)),
      (1, Some(LateResult::ReplacesFailure))
    );
  }

  #[test]
  fn without_a_window_a_last_attempt_stand_in_leaves_the_set() {
    let set = InFlightSet::with_late_result_window(0);
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0", "w1", "w2"])
    });
    assert!(matches!(
      set.defer_retryable(1, "w2", timeout_cause(1)),
      RetryDecision::Final { .. }
    ));
    assert_eq!((set.len(), set.settling_len()), (0, 0));
  }

  #[test]
  fn an_earlier_runs_result_never_lands_in_a_rerun_of_a_held_task() {
    // D-28: the reaper released the task (held); an operator re-ran it and the ventilator fetched
    // it afresh (retries 0) while still held. The old run's late result is stale — even though
    // the claim would carry the new lease's mark — and the new run's own Fatal stands.
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0"]));
    assert_eq!(set.take_expired().len(), 1);
    let mut fresh = vec![dummy_progress(1)];
    let leased =
      crate::dispatcher::ventilator::lease_next(&mut fresh, &set, "w1").expect("leased afresh");
    assert_eq!(leased.retries, 0);
    set.note_received(1);
    assert!(matches!(
      set.claim_result(1, "w0", TaskStatus::NoProblem),
      ResultClaim::Stale { attempt: 0 }
    ));
    assert_eq!(
      accepted(set.claim_result(1, "w1", TaskStatus::Fatal)),
      (1, None)
    );
  }

  #[test]
  fn an_earlier_runs_result_never_lands_in_a_rerun_of_an_in_flight_task() {
    // The same, re-fetched while the old attempt is still in flight.
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0"])
    });
    let mut fresh = vec![dummy_progress(1)];
    crate::dispatcher::ventilator::lease_next(&mut fresh, &set, "w1").expect("leased afresh");
    // Each old-run report is received (the sink holds the reaper off) before it is decided.
    set.note_received(1);
    assert!(matches!(
      set.defer_retryable(1, "w0", timeout_cause(1)),
      RetryDecision::Dropped(_)
    ));
    set.note_received(1);
    assert!(matches!(
      set.claim_result(1, "w0", TaskStatus::NoProblem),
      ResultClaim::Stale { attempt: 0 }
    ));
    // Neither left a receipt behind: once the new lease is due, the reaper takes it.
    if let Some(mut slot) = set.map.get_mut(&1)
      && let TaskSlot::InFlight(leased) = slot.value_mut()
    {
      assert_eq!(leased.pending, 0, "no receipt leaked");
      leased.progress.created_at = 0;
    }
    assert_eq!(
      set.take_expired().len(),
      1,
      "the new run's lease is reaped when due"
    );
    assert_eq!(
      accepted(set.claim_result(1, "w1", TaskStatus::Warning)),
      (1, Some(LateResult::AfterRequeue))
    );
  }

  #[test]
  fn a_retry_of_an_earlier_run_never_takes_a_new_runs_slot() {
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0"])
    });
    let mut fresh = vec![dummy_progress(1)];
    crate::dispatcher::ventilator::lease_next(&mut fresh, &set, "w1").expect("leased afresh");
    // A retry of the old run (run_start 0, retries 1) shows up: refused.
    let mut old_retry = leased_to(1, &["w0", "w2"]);
    old_retry.created_at = chrono::Utc::now().timestamp();
    assert!(!set.lease(old_retry));
    assert_eq!(
      accepted(set.claim_result(1, "w1", TaskStatus::NoProblem)),
      (1, None)
    );
  }

  #[test]
  fn a_rerun_is_not_handed_to_a_worker_of_the_earlier_run() {
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0"]));
    let _ = set.take_expired();
    let mut queue = vec![dummy_progress(1)];
    assert!(
      crate::dispatcher::ventilator::lease_next(&mut queue, &set, "w0").is_none(),
      "w0 worked the earlier run"
    );
    assert_eq!(queue.len(), 1, "the task stays queued for another worker");
    let leased = crate::dispatcher::ventilator::lease_next(&mut queue, &set, "w1").expect("leased");
    assert_eq!(
      leased.attempt_workers.last().map(String::as_str),
      Some("w1")
    );
  }

  #[test]
  fn a_restarted_ventilator_keeps_every_held_task() {
    // In flight, re-queued, dead-lettered, and finished late: none is reset to TODO.
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0"])
    });
    set.insert(leased_to(2, &["w0"]));
    set.insert(leased_to(3, &["w0", "w1", "w2"]));
    set.insert(leased_to(4, &["w0"]));
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, _done_rx) = std::sync::mpsc::sync_channel(4);
    reap_expired_into(&mut queues, &set, &done_tx);
    accepted(set.claim_result(4, "w0", TaskStatus::NoProblem));
    let mut kept = set.kept_on_restart();
    kept.sort_unstable();
    assert_eq!(kept, vec![1, 2, 3, 4]);
    assert_eq!(set.queued_retries().len(), 1, "only task 2 is queued again");
  }

  #[test]
  fn a_restarted_ventilator_queues_the_held_retries_again() {
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0"]));
    set.insert(leased_to(2, &["w0", "w1", "w2"])); // exhausted: dead-lettered, not queued
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, _done_rx) = std::sync::mpsc::sync_channel(4);
    reap_expired_into(&mut queues, &set, &done_tx);
    let again = set.queued_retries();
    assert_eq!(again.len(), 1);
    assert_eq!((again[0].task.id, again[0].retries), (1, 1));
    assert_eq!(again[0].attempt_workers, vec!["w0".to_string()]);
    // A late result that finished it takes it off the list.
    accepted(set.claim_result(1, "w0", TaskStatus::NoProblem));
    assert!(set.queued_retries().is_empty());
  }

  #[test]
  fn a_single_attempt_failure_is_not_held() {
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0"])
    });
    assert_eq!(
      accepted(set.claim_result(1, "w0", TaskStatus::Fatal)),
      (0, None)
    );
    assert_eq!(set.settling_len(), 0, "nothing could replace it");
  }

  #[test]
  fn an_unanswered_last_attempt_dead_letters_with_the_last_cause() {
    // Attempt 1 reports a timeout (budget remains: deferred, re-queued); attempt 2 never answers:
    // the dead-letter carries attempt 1's cause, not never_completed_with_retries.
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0", "w1"])
    });
    assert_eq!(
      set.defer_retryable(1, "w1", timeout_cause(1)),
      RetryDecision::Deferred
    );
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(4);
    assert_eq!(reap_expired_into(&mut queues, &set, &done_tx).requeued, 1);
    let queue = queues.get_mut(&1).unwrap();
    crate::dispatcher::ventilator::lease_next(queue, &set, "w2").expect("re-leased");
    let mut last = set.remove(1).expect("in flight");
    last.created_at = 0;
    set.insert(last);
    assert_eq!(
      reap_expired_into(&mut queues, &set, &done_tx).dead_lettered,
      1
    );
    let reports: Vec<TaskReport> = done_rx.try_iter().collect();
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert!(report.stand_in && report.status == TaskStatus::Fatal);
    assert_eq!(
      whats(&report.messages),
      whats(&timeout_cause(1)),
      "the last cause, not never_completed_with_retries"
    );
    assert_eq!(
      accepted(set.claim_result(1, "w0", TaskStatus::NoProblem)),
      (0, Some(LateResult::AfterDeadLetter))
    );
  }

  #[test]
  fn a_good_result_replaces_a_newer_attempts_crash() {
    // Good result wins (user ruling 2026-10-12): attempt 1 (w1, in flight) crashes first — a Fatal
    // recorded at once, but held — then attempt 0's late good result replaces it.
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0", "w1"])
    });
    assert_eq!(
      accepted(set.claim_result(1, "w1", TaskStatus::Fatal)),
      (1, None)
    );
    assert_eq!(
      set.settling_len(),
      1,
      "a failure is held for a better result"
    );
    match set.claim_result(1, "w0", TaskStatus::Warning) {
      ResultClaim::Accept {
        attempt,
        late,
        replaced,
        ..
      } => {
        assert_eq!(attempt, 0);
        assert_eq!(late, Some(LateResult::ReplacesFailure));
        assert_eq!(replaced, Some((1, TaskStatus::Fatal)));
      },
      other => panic!("the good result must replace the crash, got {other:?}"),
    }
    // ...and stands against a later failure.
    assert!(matches!(
      set.claim_result(1, "w1", TaskStatus::Fatal),
      ResultClaim::Superseded { .. }
    ));
  }

  #[test]
  fn a_publish_failure_restores_the_result_it_replaced() {
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0", "w1"])
    });
    accepted(set.claim_result(1, "w0", TaskStatus::NoProblem)); // older, held
    let replaced = match set.claim_result(1, "w1", TaskStatus::Warning) {
      ResultClaim::Accept { replaced, .. } => replaced,
      other => panic!("{other:?}"),
    };
    assert_eq!(replaced, Some((0, TaskStatus::NoProblem)));
    set.note_publish_failed(1, replaced);
    // Attempt 0's good result stands again: a failure cannot replace it.
    assert!(matches!(
      set.claim_result(1, "w1", TaskStatus::Fatal),
      ResultClaim::Superseded { finished: 0, .. }
    ));
  }

  #[test]
  fn of_two_retryable_causes_the_newest_attempts_stands() {
    let shed = |id: i64| {
      vec![NewTaskMessage::new(
        id,
        "fatal",
        "oom".to_string(),
        "shed".to_string(),
        String::new(),
      )]
    };
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0", "w1"]));
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, _done_rx) = std::sync::mpsc::sync_channel(4);
    assert_eq!(reap_expired_into(&mut queues, &set, &done_tx).requeued, 1);
    assert!(matches!(
      set.defer_retryable(1, "w0", timeout_cause(1)),
      RetryDecision::Dropped(_)
    ));
    assert_eq!(
      set.defer_retryable(1, "w1", shed(1)),
      RetryDecision::Deferred
    );
    assert!(matches!(
      set.defer_retryable(1, "w0", timeout_cause(1)),
      RetryDecision::Dropped(_)
    ));
    let queue = queues.get_mut(&1).unwrap();
    crate::dispatcher::ventilator::lease_next(queue, &set, "w2").expect("re-leased");
    let retry = set.remove(1).expect("in flight");
    assert_eq!(whats(&retry.last_cause), whats(&shed(1)));
  }

  #[test]
  fn a_late_good_result_is_not_replaced_by_a_newer_attempts_failure() {
    // Attempt 0 (w0) is late but good while attempt 1 (w1) runs; then attempt 1 reports a
    // timeout (retryable) or crashes (a Fatal result): the good result stands either way.
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0", "w1"])
    });
    assert_eq!(
      accepted(set.claim_result(1, "w0", TaskStatus::Warning)),
      (0, Some(LateResult::OlderAttempt))
    );
    assert!(matches!(
      set.defer_retryable(1, "w1", timeout_cause(1)),
      RetryDecision::Dropped(_)
    ));
    assert!(matches!(
      set.claim_result(1, "w1", TaskStatus::Fatal),
      ResultClaim::Superseded {
        attempt: 1,
        finished: 0,
      }
    ));
    // A newer attempt's usable result still replaces it.
    assert_eq!(
      accepted(set.claim_result(1, "w1", TaskStatus::NoProblem)),
      (1, Some(LateResult::NewerAttempt))
    );
  }

  #[test]
  fn an_older_attempts_retryable_cause_is_dropped_while_a_newer_runs() {
    let set = InFlightSet::new();
    set.insert(TaskProgress {
      created_at: chrono::Utc::now().timestamp(),
      ..leased_to(1, &["w0", "w1"])
    });
    assert!(matches!(
      set.defer_retryable(1, "w0", timeout_cause(1)),
      RetryDecision::Dropped(_)
    ));
    assert!(
      set.take_expired().is_empty(),
      "the newer attempt keeps its lease"
    );
    // An older attempt's unusable reply does not mark the task answered either.
    set.note_unusable_reply(1, "w0", true);
    assert_eq!(set.awaiting_replies(), 1);
  }

  #[test]
  fn a_retryable_cause_after_a_requeue_rides_to_the_retry() {
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0"]));
    let mut queues: HashMap<i32, Vec<TaskProgress>> = HashMap::new();
    let (done_tx, _done_rx) = std::sync::mpsc::sync_channel(4);
    reap_expired_into(&mut queues, &set, &done_tx);
    assert_eq!(
      set.defer_retryable(1, "w0", timeout_cause(1)),
      RetryDecision::Deferred
    );
    let queue = queues.get_mut(&1).unwrap();
    crate::dispatcher::ventilator::lease_next(queue, &set, "w1").expect("re-leased");
    let retry = set.remove(1).expect("in flight");
    assert_eq!(whats(&retry.last_cause), whats(&timeout_cause(1)));
  }

  #[test]
  fn a_received_result_holds_off_the_reaper() {
    let set = InFlightSet::new();
    set.insert(leased_to(1, &["w0"]));
    set.note_received(1);
    assert!(
      set.take_expired().is_empty(),
      "its writer is deciding the result"
    );
    set.note_unusable_reply(1, "w0", true);
    assert_eq!(
      set.take_expired().len(),
      1,
      "an unusable result leaves it to the reaper"
    );
  }
}
