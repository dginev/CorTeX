use crate::backend;
use crate::dispatcher::server;
use crate::helpers::TaskReport;
use std::error::Error;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Upper bound on how stale the `report_summary` rollup may get while a run is in flight. A single
/// conversion run can take weeks, so an event-only refresh (on drain) is not enough — we also
/// recompute the rollup at least this often during continuous processing.
const REPORT_REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Specifies the binding and operation parameters for a thread that saves finalized tasks to the DB
pub struct Finalize {
  /// the DB address to bind on
  pub backend_address: String,
  /// Maximum number of jobs before manager termination (optional)
  pub job_limit: Option<usize>,
}

impl Finalize {
  /// Start the finalize loop, checking for new completed tasks every second
  pub fn start(&self, done_queue_arc: &Arc<Mutex<Vec<TaskReport>>>) -> Result<(), Box<dyn Error>> {
    let mut backend = backend::from_address(&self.backend_address);
    let mut jobs_count: usize = 0;
    // Whether finalized work has landed since the report rollup was last refreshed, and when that
    // refresh happened — together these drive a "refresh on drain, but at least daily" cadence.
    let mut reports_dirty = false;
    let mut last_report_refresh = Instant::now();
    // Persist every 1 second, if there is something to record
    loop {
      if server::mark_done_arc(&mut backend, done_queue_arc)? {
        // we did some work, on to the next iteration
        jobs_count += 1;
        reports_dirty = true;
        if jobs_count.is_multiple_of(100) {
          println!("-- finalize thread persisted {jobs_count} jobs.");
        }
        // Long runs never drain, so bound report staleness with a periodic refresh.
        if last_report_refresh.elapsed() >= REPORT_REFRESH_INTERVAL {
          refresh_reports(&mut backend);
          reports_dirty = false;
          last_report_refresh = Instant::now();
        }
      } else {
        // The queue just drained: recompute the rollup so the finished work shows up in reports
        // immediately, then idle for a second.
        if reports_dirty {
          refresh_reports(&mut backend);
          reports_dirty = false;
          last_report_refresh = Instant::now();
        }
        thread::sleep(Duration::new(1, 0));
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
