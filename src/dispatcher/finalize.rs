use crate::backend;
use crate::dispatcher::server;
use crate::helpers::TaskReport;
use std::collections::HashSet;
use std::error::Error;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Upper bound on how stale the `report_summary` rollup may get while a run is in flight. A single
/// conversion run can take weeks, so an event-only refresh (on drain) is not enough — we also
/// recompute the rollup at least this often during continuous processing. Runtime-configurable via
/// `config().dispatcher.report_refresh_interval_seconds` (the automatic freshness guarantee;
/// default 1h, cheap now that the refresh is non-blocking — `CONCURRENTLY`).
fn report_refresh_interval() -> Duration {
  Duration::from_secs(
    crate::config::config()
      .dispatcher
      .report_refresh_interval_seconds,
  )
}

/// Specifies the binding and operation parameters for a thread that saves finalized tasks to the DB
pub struct Finalize {
  /// the DB address to bind on
  pub backend_address: String,
}

/// Accumulates a finalize batch off the bounded done channel: starting from `first`, keep pulling
/// reports until the batch reaches `batch_size` (N) **or** `flush_window` (T) elapses since `first`
/// landed — whichever fires first. Returns the batch and whether the channel **disconnected**
/// mid-accumulation (all producers gone → shutdown).
///
/// This is the heart of the phase-2 DB coalescing knob: rather than flushing the instant a result
/// lands (one DB write per result under light load), it deliberately groups writes — fewer, bigger
/// transactions — while bounding the wait (and thus report staleness + crash re-work) to T. It is
/// loss-free: an unflushed batch is never *lost*; its tasks remain `Queued` and recover on restart.
/// Pure (no DB, no I/O), so its size-vs-time flush logic is unit-tested directly.
fn accumulate_batch(
  done_rx: &Receiver<TaskReport>,
  first: TaskReport,
  batch_size: usize,
  flush_window: Duration,
) -> (Vec<TaskReport>, bool) {
  let batch_start = Instant::now();
  let mut batch = vec![first];
  let mut disconnected = false;
  while batch.len() < batch_size {
    let elapsed = batch_start.elapsed();
    if elapsed >= flush_window {
      break;
    }
    match done_rx.recv_timeout(flush_window - elapsed) {
      Ok(report) => batch.push(report),
      Err(RecvTimeoutError::Timeout) => break,
      Err(RecvTimeoutError::Disconnected) => {
        disconnected = true;
        break;
      },
    }
  }
  (batch, disconnected)
}

impl Finalize {
  /// Start the finalize loop: block on the bounded done channel for the first report of a batch,
  /// then **accumulate** more via [`accumulate_batch`] until the size (N,
  /// `finalize_batch_size`) or time (T, `finalize_flush_ms`) threshold trips, and persist the
  /// whole batch in one `mark_done` transaction. This is **event-driven** (woken the instant a
  /// result lands), **backpressured** by the bounded channel (not the old `Mutex<Vec>` +
  /// panic-backstop), and **coalesced** so DB write frequency stays bounded under load (phase 2).
  /// The outer `recv_timeout(1s)` preserves the 1s idle cadence for the refresh-on-drain. Shutdown
  /// is driven entirely by `Disconnected` (every producer's done-sender dropped): in a bounded run
  /// the manager drops the last sender once the ventilator + sink have finished, and the finalize
  /// thread then drains the remaining reports and stops (KNOWN_ISSUES D-5 — no per-thread
  /// `job_limit` counter, which was the unit that disagreed with the other two threads).
  pub fn start(&self, done_rx: Receiver<TaskReport>) -> Result<(), Box<dyn Error>> {
    let mut backend = backend::from_address(&self.backend_address);
    let dispatcher = &crate::config::config().dispatcher;
    let batch_size = dispatcher.finalize_batch_size.max(1);
    let flush_window = Duration::from_millis(dispatcher.finalize_flush_ms);
    let mut jobs_count: usize = 0;
    // Whether finalized work has landed since the report rollup was last refreshed, and when that
    // refresh happened — together these drive a "refresh on drain, but at least daily" cadence.
    let mut reports_dirty = false;
    let mut last_report_refresh = Instant::now();
    // Distinct (corpus_id, service_id) scopes whose tasks we've persisted since the last report
    // invalidation. On drain / periodic tick we drop ONLY these scopes' cached report grains (a
    // cheap keyed DELETE), so each repopulates lazily on its next view — never a global scan.
    let mut touched: HashSet<(i32, i32)> = HashSet::new();
    loop {
      match done_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(first) => {
          // Coalesce a batch (up to N reports, or T elapsed) into one DB round-trip.
          let (batch, disconnected) = accumulate_batch(&done_rx, first, batch_size, flush_window);
          let batch_len = batch.len();
          let persist_start = Instant::now();
          server::mark_done_batch(&mut backend, &batch)?;
          jobs_count += 1;
          reports_dirty = true;
          for report in &batch {
            touched.insert((report.task.corpus_id, report.task.service_id));
          }
          // Pipeline health signals (Arm 8 / phase-5 observability; transport-independent): batch
          // size, DB persist latency, and whether the batch hit the size cap. `size_capped` is the
          // backpressure/lag proxy — the bounded done channel already had a full batch queued (std
          // `sync_channel` can't expose its depth), i.e. the DB finalize is the bottleneck and lag
          // is building. At `debug` (one event per batch ⇒ off at the default `info`; enable on
          // demand with `RUST_LOG=cortex=debug`).
          debug!(
            batch = batch_len,
            persist_ms = persist_start.elapsed().as_millis() as u64,
            size_capped = batch_len >= batch_size,
            batches_total = jobs_count,
            "finalize: persisted batch"
          );
          // Long runs may never idle, so bound report staleness with a periodic refresh.
          if last_report_refresh.elapsed() >= report_refresh_interval() {
            settle_touched(&mut backend, &mut touched);
            reports_dirty = false;
            last_report_refresh = Instant::now();
          }
          if disconnected {
            // Producers vanished mid-batch: we persisted what we had; make it visible and stop.
            if reports_dirty {
              settle_touched(&mut backend, &mut touched);
            }
            break;
          }
        },
        Err(RecvTimeoutError::Timeout) => {
          // The queue just idled: close any historical run whose work has drained, recompute the
          // rollup so finished work shows up in reports immediately, then keep waiting.
          if reports_dirty {
            settle_touched(&mut backend, &mut touched);
            reports_dirty = false;
            last_report_refresh = Instant::now();
          }
        },
        Err(RecvTimeoutError::Disconnected) => {
          // Every producer (sink + ventilator) has gone — clean shutdown. Flush and stop.
          if reports_dirty {
            settle_touched(&mut backend, &mut touched);
          }
          break;
        },
      }
    }
    Ok(())
  }
}

/// On drain/idle, settle the `(corpus, service)` scopes touched since the last tick: first close
/// any historical runs whose work has drained (run-completion-on-drain), then drop those scopes'
/// cached report grains so the next report view repopulates. Order matters — closing the run
/// freezes its tallies, and we invalidate the report cache *after* so the next view reads the
/// frozen, completed run rather than the stale open one. Drains `touched`.
fn settle_touched(backend: &mut backend::Backend, touched: &mut HashSet<(i32, i32)>) {
  complete_drained_runs(backend, touched);
  invalidate_touched(backend, touched);
}

/// Run-completion-on-drain: for each scope we've persisted results for since the last tick, close
/// its open historical run if the pair's work is now exhausted (no task left `TODO`, `Queued`, or
/// `Blocked`). This fires the "run ended" event the instant the queue drains, instead of lazily
/// when the next rerun starts — so a finished run stops showing as "ongoing" right away. Read-only
/// over `touched` (the following [`invalidate_touched`] drains it). A per-scope failure is logged
/// and skipped so the finalize thread keeps running; the next idle/periodic tick retries.
fn complete_drained_runs(backend: &mut backend::Backend, touched: &HashSet<(i32, i32)>) {
  for &(corpus_id, service_id) in touched {
    match backend.complete_run_if_drained(corpus_id, service_id) {
      Ok(true) => debug!(corpus_id, service_id, "finalize: closed drained run"),
      Ok(false) => {},
      Err(e) => warn!(
        corpus_id,
        service_id,
        error = ?e,
        "finalize: run-completion check failed (non-fatal)"
      ),
    }
  }
}

/// Drop the cached report grains for the `(corpus, service)` scopes touched since the last
/// invalidation (drains `touched`). DELETE-only and per-scope — the finalize thread does no heavy
/// scan; each scope's report repopulates lazily on its next view. A failure (e.g. a transient lock)
/// must not take down the finalize thread — log it and carry on; the next tick retries.
fn invalidate_touched(backend: &mut backend::Backend, touched: &mut HashSet<(i32, i32)>) {
  for (corpus_id, service_id) in touched.drain() {
    if let Err(e) = backend.invalidate_report_cache(corpus_id, service_id) {
      warn!(
        corpus_id,
        service_id,
        error = ?e,
        "finalize: report-cache invalidate failed (non-fatal)"
      );
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::helpers::TaskStatus;
  use crate::models::Task;
  use std::sync::mpsc::sync_channel;

  fn report(id: i64) -> TaskReport {
    TaskReport {
      task: Task {
        id,
        service_id: 2,
        corpus_id: 1,
        status: 0,
        entry: String::new(),
      },
      status: TaskStatus::NoProblem,
      messages: Vec::new(),
    }
  }

  #[test]
  fn batch_flushes_at_the_size_threshold_without_waiting() {
    // With more than N reports already queued, the batch fills to exactly N and returns at once —
    // it must NOT block for the (long) time window. This is the under-load path: bounded batches,
    // no added latency.
    let (tx, rx) = sync_channel::<TaskReport>(100);
    for id in 1..=5 {
      tx.send(report(id)).unwrap();
    }
    let first = rx.recv().unwrap(); // id 1, as the outer loop would have taken it
    let start = Instant::now();
    let (batch, disconnected) = accumulate_batch(&rx, first, 3, Duration::from_secs(30));
    assert_eq!(batch.len(), 3, "fills to exactly the size threshold N");
    assert!(!disconnected);
    assert!(
      start.elapsed() < Duration::from_secs(1),
      "must return immediately on the size threshold, not wait out the window"
    );
    // The surplus stays queued for the next batch (loss-free hand-off).
    assert_eq!(rx.try_iter().count(), 2);
  }

  #[test]
  fn batch_flushes_at_the_time_threshold_when_under_n() {
    // Under N reports and no more arriving: the batch flushes when the time window T elapses,
    // bounding staleness. (Short T keeps the test fast.)
    let (_tx, rx) = sync_channel::<TaskReport>(100);
    let start = Instant::now();
    let (batch, disconnected) = accumulate_batch(&rx, report(1), 512, Duration::from_millis(60));
    assert_eq!(
      batch.len(),
      1,
      "flushes the lone report at the time threshold"
    );
    assert!(!disconnected);
    assert!(
      start.elapsed() >= Duration::from_millis(60),
      "waited out the time window before flushing"
    );
  }

  #[test]
  fn batch_signals_disconnect_when_producers_drop_mid_accumulation() {
    // All producers gone while we are still under N and within T: accumulation ends early and
    // signals shutdown, with whatever was collected so far still returned (persisted, not lost).
    let (tx, rx) = sync_channel::<TaskReport>(100);
    tx.send(report(2)).unwrap();
    drop(tx);
    let (batch, disconnected) = accumulate_batch(&rx, report(1), 512, Duration::from_secs(30));
    assert_eq!(batch.len(), 2, "the first report plus the one still queued");
    assert!(
      disconnected,
      "a dropped sender ends accumulation as a shutdown"
    );
  }
}
