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

#[test]
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

#[test]
fn api_job_is_404_for_unknown_uuid() {
  let client = client();
  let response = client
    .get("/api/jobs/00000000-0000-0000-0000-000000000000")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
}

#[test]
fn job_progress_page_renders_html() {
  let client = client();
  let response = client
    .get("/jobs/00000000-0000-0000-0000-000000000000")
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
}
