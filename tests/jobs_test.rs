// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the background-job mechanism (see docs/JOB_MODEL.md): a job runs to a terminal
//! state on a background thread, persisting its progress and result.

use cortex::backend::{build_pool, test_db_address};
use cortex::jobs;
use std::time::{Duration, Instant};

#[test]
fn spawn_job_runs_to_succeeded_with_progress() {
  let pool = build_pool(test_db_address(), 4);

  let uuid = jobs::spawn_job(
    pool.clone(),
    "test_job",
    "tester",
    serde_json::json!({ "k": "v" }),
    |progress| {
      progress.step(1, Some(2), "halfway");
      progress.step(2, Some(2), "done");
      Ok(serde_json::json!({ "ok": true }))
    },
  )
  .expect("the job should be spawned");

  let mut connection = pool.get().expect("a connection");
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    let job = jobs::find_job(&mut connection, uuid).expect("the job row exists");
    if job.status == "succeeded" {
      assert_eq!(job.kind, "test_job");
      assert_eq!(job.progress_current, 2);
      assert_eq!(job.result, Some(serde_json::json!({ "ok": true })));
      break;
    }
    assert_ne!(job.status, "failed", "job failed: {}", job.message);
    assert!(
      Instant::now() < deadline,
      "job did not finish (status {})",
      job.status
    );
    std::thread::sleep(Duration::from_millis(20));
  }
}

#[test]
fn spawn_job_marks_a_panicking_body_failed_not_stuck() {
  // A panicking job body (e.g. a DB connection that panics on establish) must not strand the job
  // `running` forever — the worker catches the panic and records a terminal `failed` state.
  let pool = build_pool(test_db_address(), 4);
  let uuid = jobs::spawn_job(
    pool.clone(),
    "panic_job",
    "tester",
    serde_json::json!({}),
    |_| {
      panic!("boom in the job body");
    },
  )
  .expect("the job should be spawned");

  let mut connection = pool.get().expect("a connection");
  let deadline = Instant::now() + Duration::from_secs(5);
  loop {
    let job = jobs::find_job(&mut connection, uuid).expect("the job row exists");
    match job.status.as_str() {
      "failed" => {
        assert!(
          job.message.contains("panicked") && job.message.contains("boom"),
          "the failure message surfaces the panic: {}",
          job.message
        );
        break;
      },
      "succeeded" => panic!("a panicking body must not succeed"),
      _ => {}, // queued / running — keep waiting
    }
    assert!(
      Instant::now() < deadline,
      "panicking job never reached a terminal state (status {})",
      job.status
    );
    std::thread::sleep(Duration::from_millis(20));
  }
}
