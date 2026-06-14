use crate::backend;
use crate::dispatcher::server;
use crate::helpers::TaskReport;
use std::error::Error;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

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
  /// Maximum number of jobs before manager termination (optional)
  pub job_limit: Option<usize>,
}

impl Finalize {
  /// Start the finalize loop: block on the bounded done channel, drain everything currently queued
  /// into one batch, and persist it (the existing batched-persist, now **event-driven** — woken the
  /// instant a result lands instead of a 1s poll — and **backpressured** by the bounded channel
  /// rather than the old `Mutex<Vec>` + panic-backstop). `recv_timeout(1s)` preserves the 1s idle
  /// cadence for the `job_limit` check + the refresh-on-drain. `Disconnected` (all producers gone)
  /// is a clean shutdown. `jobs_count` counts **drains** (batches), as before, so `job_limit`
  /// semantics are unchanged.
  pub fn start(&self, done_rx: Receiver<TaskReport>) -> Result<(), Box<dyn Error>> {
    let mut backend = backend::from_address(&self.backend_address);
    let mut jobs_count: usize = 0;
    // Whether finalized work has landed since the report rollup was last refreshed, and when that
    // refresh happened — together these drive a "refresh on drain, but at least daily" cadence.
    let mut reports_dirty = false;
    let mut last_report_refresh = Instant::now();
    loop {
      match done_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(first) => {
          // Drain everything currently queued into one batch — amortizes the DB round-trip.
          let mut batch = vec![first];
          while let Ok(report) = done_rx.try_recv() {
            batch.push(report);
          }
          server::mark_done_batch(&mut backend, &batch)?;
          jobs_count += 1;
          reports_dirty = true;
          if jobs_count.is_multiple_of(100) {
            println!("-- finalize thread persisted {jobs_count} batches.");
          }
          // Long runs may never idle, so bound report staleness with a periodic refresh.
          if last_report_refresh.elapsed() >= report_refresh_interval() {
            refresh_reports(&mut backend);
            reports_dirty = false;
            last_report_refresh = Instant::now();
          }
        },
        Err(RecvTimeoutError::Timeout) => {
          // The queue just drained: recompute the rollup so finished work shows up in reports
          // immediately, then keep waiting.
          if reports_dirty {
            refresh_reports(&mut backend);
            reports_dirty = false;
            last_report_refresh = Instant::now();
          }
        },
        Err(RecvTimeoutError::Disconnected) => {
          // Every producer (sink + ventilator) has gone — clean shutdown. Flush and stop.
          if reports_dirty {
            refresh_reports(&mut backend);
          }
          break;
        },
      }
      if let Some(limit) = self.job_limit {
        if jobs_count >= limit {
          println!("finalize {limit}: job limit reached, terminating finalize thread...");
          // Make the final batch visible before we stop.
          if reports_dirty {
            refresh_reports(&mut backend);
          }
          break;
        }
      }
    }
    Ok(())
  }
}

/// Best-effort refresh of the `report_summary` rollup. A failure here (e.g. a transient lock) must
/// not take down the finalize thread — log it and carry on; the next drain or daily tick retries.
fn refresh_reports(backend: &mut backend::Backend) {
  if let Err(e) = backend.refresh_report_summary() {
    eprintln!("-- finalize: report_summary refresh failed (non-fatal): {e:?}");
  }
}
