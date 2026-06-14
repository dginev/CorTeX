// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract tests for the jobs HTTP surface: poll a job (agent), and the human progress page.

use cortex::backend::{build_pool, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::jobs;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_jobs_api_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

fn api_job_polls_a_spawned_job() {
  let pool = build_pool(test_db_address(), 4);
  let uuid = jobs::spawn_job(
    pool,
    "api_test_job",
    "tester",
    serde_json::json!({}),
    |progress| {
      progress.step(1, Some(1), "done");
      Ok(serde_json::json!({ "done": true }))
    },
  )
  .expect("spawn the job");

  let client = client();
  let path = format!("/api/jobs/{uuid}");
  let mut body = serde_json::Value::Null;
  for _ in 0..200 {
    let response = client.get(path.as_str()).dispatch();
    assert_eq!(response.status(), Status::Ok);
    assert_eq!(response.content_type(), Some(ContentType::JSON));
    body = response.into_json().expect("a JSON body");
    if body["status"] == "succeeded" {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
  }

  // Data contract:
  assert_eq!(body["status"], "succeeded");
  assert_eq!(body["kind"], "api_test_job");
  assert_eq!(body["uuid"], uuid.to_string());
  assert!(body["progress_current"].is_number());
  assert_eq!(body["result"], serde_json::json!({ "done": true }));
}

fn api_job_is_404_for_unknown_uuid() {
  let client = client();
  let response = client
    .get("/api/jobs/00000000-0000-0000-0000-000000000000")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
}

fn job_progress_page_renders_html() {
  let client = client();
  let response = client
    .get("/jobs/00000000-0000-0000-0000-000000000000")
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
}

fn jobs_list_carries_health_and_duration_and_supports_pending() {
  // Spawn a job and let it finish, so the list has a known recent entry.
  let pool = build_pool(test_db_address(), 4);
  let uuid = jobs::spawn_job(
    pool,
    "list_test_job",
    "tester",
    serde_json::json!({}),
    |p| {
      p.step(1, Some(1), "done");
      Ok(serde_json::json!({ "ok": true }))
    },
  )
  .expect("spawn the job");
  // Give the worker thread a moment to reach a terminal state.
  for _ in 0..100 {
    let mut db = cortex::backend::testdb();
    if let Some(job) = jobs::find_job(&mut db.connection, uuid) {
      if job.status == "succeeded" {
        break;
      }
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
  }

  let client = client();

  // The fleet-wide list carries the observability metadata (health + duration) for every job.
  let response = client.get("/api/jobs?limit=100").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let body: serde_json::Value = response.into_json().expect("a JSON array");
  let ours = body
    .as_array()
    .expect("array")
    .iter()
    .find(|j| j["uuid"] == uuid.to_string())
    .expect("our job is listed");
  assert_eq!(ours["health"], "ok", "succeeded -> ok health");
  assert!(
    ours["duration_seconds"].is_number(),
    "duration metadata present"
  );

  // The HTML dashboard renders.
  let response = client.get("/jobs").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  assert!(response
    .into_string()
    .expect("html")
    .contains("Background jobs"));

  // The pending filter excludes our now-terminal job.
  let response = client.get("/api/jobs?active=true&limit=100").dispatch();
  let pending: serde_json::Value = response.into_json().expect("a JSON array");
  assert!(
    !pending
      .as_array()
      .expect("array")
      .iter()
      .any(|j| j["uuid"] == uuid.to_string()),
    "a finished job is not pending"
  );
}

// Custom harness (see KNOWN_ISSUES L-1): run the cases then `_exit(0)`.
fn main() {
  api_job_polls_a_spawned_job();
  api_job_is_404_for_unknown_uuid();
  job_progress_page_renders_html();
  jobs_list_carries_health_and_duration_and_supports_pending();
  eprintln!("jobs_api_test: all cases passed");
  unsafe { libc::_exit(0) }
}
