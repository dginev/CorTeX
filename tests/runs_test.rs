// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract tests for the historical-runs API: list a `(corpus, service)`'s runs and the current
//! (open) run, as the agent twin of the human history screen.

use cortex::backend::{self, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::models::{Corpus, NewCorpus, NewService, Service};
use cortex::schema::{corpora, historical_runs, services, tasks};
use diesel::prelude::*;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;
use serde_json::Value;

// URL-safe (no spaces): the name travels in the request path.
const CORPUS_NAME: &str = "runs-api-corpus";
const SERVICE_NAME: &str = "runs_api_svc";

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_runs_api_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

/// Clean slate, then seed a corpus + service with two historical runs (the first auto-completed
/// when the second starts, so exactly one run is left open).
fn seed() {
  let mut backend = backend::testdb();
  if let Ok(existing) = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection) {
    diesel::delete(historical_runs::table.filter(historical_runs::corpus_id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
    diesel::delete(tasks::table.filter(tasks::corpus_id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
    diesel::delete(corpora::table.filter(corpora::id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
  }
  diesel::delete(services::table.filter(services::name.eq(SERVICE_NAME)))
    .execute(&mut backend.connection)
    .ok();

  backend
    .add(&NewCorpus {
      name: CORPUS_NAME.to_string(),
      path: "/tmp/runs-api".to_string(),
      complex: true,
      description: String::new(),
    })
    .expect("add corpus");
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("corpus");
  backend
    .add(&NewService {
      name: SERVICE_NAME.to_string(),
      version: 0.1,
      inputformat: "tex".to_string(),
      outputformat: "html".to_string(),
      inputconverter: Some("import".to_string()),
      complex: true,
      description: String::from("runs api service"),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");

  backend
    .mark_new_run(&corpus, &service, "tester".into(), "first run".into())
    .expect("first run");
  backend
    .mark_new_run(&corpus, &service, "tester".into(), "second run".into())
    .expect("second run");
}

// Both assertions live in one test so the shared seed isn't raced by parallel test threads.
#[test]
fn api_lists_runs_and_reports_current() {
  seed();
  let client = client();

  // --- List: two runs, exactly one still open -------------------------------------------------
  let response = client
    .get(format!("/api/runs/{CORPUS_NAME}/{SERVICE_NAME}"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let runs: Value = response.into_json().expect("a JSON array");
  let runs = runs.as_array().expect("array");

  assert_eq!(runs.len(), 2, "two historical runs were seeded");
  let open: Vec<&Value> = runs.iter().filter(|r| r["completed"] == false).collect();
  let done: Vec<&Value> = runs.iter().filter(|r| r["completed"] == true).collect();
  assert_eq!(open.len(), 1, "exactly one open run");
  assert_eq!(done.len(), 1, "exactly one completed run");

  let current = open[0];
  assert_eq!(current["description"], "second run");
  assert_eq!(current["owner"], "tester");
  assert!(current["id"].is_number(), "runs carry a stable id handle");
  assert_eq!(current["end_time"], Value::Null, "open run has no end_time");
  assert!(
    done[0]["end_time"].is_string(),
    "completed run has an end_time"
  );

  // --- Current: the open run -------------------------------------------------------------------
  let response = client
    .get(format!("/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/current"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let current: Value = response.into_json().expect("a JSON value");
  assert_eq!(current["completed"], false);
  assert_eq!(current["description"], "second run");

  // --- Diff: well-formed even with no saved snapshots ------------------------------------------
  let response = client
    .get(format!("/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/diff"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let diff: Value = response.into_json().expect("diff json");
  assert!(
    diff["available_dates"].is_array(),
    "diff carries the available snapshot dates"
  );
  assert!(
    diff["transitions"].is_array(),
    "diff carries a transition matrix"
  );

  // Guard: a malformed snapshot date is a 400, not a panic (the legacy HTML route panics here).
  let response = client
    .get(format!(
      "/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/diff?previous=not-a-date"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "malformed date -> 400, not a panic"
  );

  // --- Per-task diff: well-formed + paginated; an unknown status filter is a 400 ---------------
  let response = client
    .get(format!(
      "/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?page_size=5"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let tasks: Value = response.into_json().expect("tasks json");
  assert!(tasks.is_array(), "per-task diff is a JSON array");

  let response = client
    .get(format!(
      "/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?previous_status=not-a-status"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "unknown status filter -> 400"
  );

  // --- HTML twin: the human run-history screen renders the same runs server-side ---------------
  let response = client
    .get(format!("/runs/{CORPUS_NAME}/{SERVICE_NAME}"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("Run history"),
    "renders the run-history screen"
  );
  assert!(
    body.contains("second run"),
    "renders the seeded run rows server-side"
  );

  // --- HTML twin: the human task-diff screen (the filter-driven heart of run management) --------
  let response = client
    .get(format!("/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("Task severity changes"),
    "renders the task-diff screen"
  );
  assert!(
    body.contains("select-previous-status") && body.contains("select-current-status"),
    "renders the status-transition filter form"
  );

  // Guard: an unknown status filter is a 400 on the HTML twin too (the legacy route panics here).
  let response = client
    .get(format!(
      "/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?previous_status=not-a-status"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "unknown status filter -> 400 on the HTML screen, not a panic"
  );
  // Guard: a malformed snapshot date is a 400, not a panic.
  let response = client
    .get(format!(
      "/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?previous=not-a-date"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "malformed date -> 400 on the HTML screen, not a panic"
  );

  // --- HTML twin: the diff-summary matrix screen (links into the task drill-down) --------------
  let response = client
    .get(format!("/runs/{CORPUS_NAME}/{SERVICE_NAME}/diff"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("Run differences"),
    "renders the diff-summary matrix screen"
  );
  // This seed has runs but no saved task snapshots, so the matrix gracefully reports nothing to
  // compare (the empty-state robustness path) rather than erroring or showing an empty table.
  assert!(
    body.contains("No saved snapshots yet"),
    "diff matrix degrades gracefully when there are no snapshots to compare"
  );

  // Guard: a malformed snapshot date is a 400 on the matrix screen too (legacy route panics here).
  let response = client
    .get(format!(
      "/runs/{CORPUS_NAME}/{SERVICE_NAME}/diff?previous=not-a-date"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "malformed date -> 400 on the matrix screen, not a panic"
  );
}

#[test]
fn api_runs_is_404_for_unknown_corpus() {
  let client = client();
  // The agent API and its human twin both 404 on an unknown corpus/service.
  let response = client
    .get("/api/runs/no-such-corpus-xyz/no_such_service")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
  let response = client
    .get("/runs/no-such-corpus-xyz/no_such_service")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
  let response = client
    .get("/runs/no-such-corpus-xyz/no_such_service/tasks")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
  let response = client
    .get("/runs/no-such-corpus-xyz/no_such_service/diff")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
}
