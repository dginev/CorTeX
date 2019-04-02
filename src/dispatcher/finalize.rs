use crate::backend;
use crate::dispatcher::server;
use crate::helpers::TaskReport;
use std::error::Error;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

/// Specifies the binding and operation parameters for a thread that saves finalized tasks to the DB
pub struct Finalize {
  /// the DB address to bind on
  pub backend_address: String,
  /// Maximum number of jobs before manager termination (optional)
  pub job_limit: Option<usize>,
}

impl Finalize {
  /// Start the finalize loop, checking for new completed tasks every second
  pub fn start(&self, done_queue_arc: &Arc<Mutex<Vec<TaskReport>>>) -> Result<(), Box<Error>> {
    let backend = backend::from_address(&self.backend_address);
    let mut jobs_count: usize = 0;
    // Persist every 1 second, if there is something to record
    loop {
      if server::mark_done_arc(&backend, done_queue_arc)? {
        // we did some work, on to the next iteration
        jobs_count += 1;
        if jobs_count % 100 == 0 {
          println!("-- finalize thread persisted {} jobs.", jobs_count);
        }
      } else {
        // If we have no reports to process, sleep for a second and recheck
        thread::sleep(Duration::new(1, 0));
      }
      if let Some(limit) = self.job_limit {
        if jobs_count >= limit {
          println!(
            "finalize {}: job limit reached, terminating finalize thread...",
            limit
          );
          break;
        }
      }
    }
    Ok(())
  }
}
