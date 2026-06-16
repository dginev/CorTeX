// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for historical-data **retention** (`frontend::retention`): the snapshot-stats
//! agent endpoint + the signed-in admin screen, and the dry-run-preview-then-confirm prune. Seeds
//! one OLD per-task snapshot (saved_at in the year 2000, with a valid task FK) so a cutoff prune
//! deletes it while every other test's recent snapshots (saved now) are untouched.

use chrono::NaiveDate;
use cortex::backend::{self, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::models::{Corpus, HistoricalTask, NewCorpus, NewService, Service};
use cortex::schema::{corpora, services, tasks};
use diesel::prelude::*;
use rocket::http::{ContentType, Header, Status};
use rocket::local::blocking::Client;

const CORPUS: &str = "retention-test-corpus";
const SERVICE: &str = "retention_test_svc";

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_retention_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

/// Seeds a throwaway corpus + service + task, then one snapshot dated 2000-01-01 (old) and one
/// dated now (recent), both referencing the task (the historical_tasks FK requires a real task).
fn seed_old_snapshot() {
  let mut db = backend::testdb();
  diesel::delete(corpora::table.filter(corpora::name.eq(CORPUS)))
    .execute(&mut db.connection)
    .ok();
  diesel::delete(services::table.filter(services::name.eq(SERVICE)))
    .execute(&mut db.connection)
    .ok();
  db.add(&NewCorpus {
    name: CORPUS.to_string(),
    path: "/tmp/retention".to_string(),
    complex: false,
    description: String::new(),
  })
  .expect("corpus");
  let corpus = Corpus::find_by_name(CORPUS, &mut db.connection).expect("corpus");
  db.add(&NewService {
    name: SERVICE.to_string(),
    version: 0.1,
    inputformat: "tex".to_string(),
    outputformat: "html".to_string(),
    inputconverter: Some("import".to_string()),
    complex: false,
    description: String::new(),
  })
  .expect("service");
  let service = Service::find_by_name(SERVICE, &mut db.connection).expect("service");

  diesel::sql_query(format!(
    "INSERT INTO tasks (service_id, corpus_id, status, entry) VALUES ({}, {}, 0, '/tmp/retention/x')",
    service.id, corpus.id
  ))
  .execute(&mut db.connection)
  .expect("task");
  let task_id: i64 = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .select(tasks::id)
    .first(&mut db.connection)
    .expect("task id");
  diesel::sql_query(format!(
    "INSERT INTO historical_tasks (task_id, status, saved_at) \
     VALUES ({task_id}, 0, '2000-01-01 00:00:00'), ({task_id}, -1, now())"
  ))
  .execute(&mut db.connection)
  .expect("snapshots");
}

fn cutoff_before_year(year: i32) -> chrono::NaiveDateTime {
  NaiveDate::from_ymd_opt(year, 1, 1)
    .unwrap()
    .and_hms_opt(0, 0, 0)
    .unwrap()
}

fn retention_stats_preview_and_prune() {
  seed_old_snapshot();
  let client = client();

  // The screen is signed-in-only.
  assert!(
    client
      .get("/admin/retention")
      .dispatch()
      .headers()
      .get_one("Location")
      .unwrap_or("")
      .starts_with("/admin/login?next="),
    "the retention screen requires sign-in"
  );

  // The agent stats endpoint is token-gated and reports the snapshot total.
  assert_eq!(
    client.get("/api/historical/stats").dispatch().status(),
    Status::Unauthorized,
    "stats requires a token"
  );
  let response = client
    .get("/api/historical/stats")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let body = response.into_string().expect("json");
  assert!(
    body.contains("snapshot_rows"),
    "stats reports the row total"
  );

  // Sign in, then the screen renders + a cutoff preview shows the dry-run count (>=1, our old
  // snapshot).
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
  let body = client
    .get("/admin/retention")
    .dispatch()
    .into_string()
    .expect("html");
  assert!(
    body.contains("Historical data retention") && body.contains("Per-task snapshot rows"),
    "the retention screen renders with stats"
  );

  // Sanity (model): exactly the seeded old snapshot is older than 2001.
  let mut db = backend::testdb();
  let before_2001 = cutoff_before_year(2001);
  assert!(
    HistoricalTask::count_before(&mut db.connection, before_2001).unwrap() >= 1,
    "the seeded year-2000 snapshot is counted as prunable"
  );

  // Prune via the endpoint (the form the confirmed Delete button submits).
  let response = client
    .post("/admin/retention/prune")
    .header(ContentType::Form)
    .body("before=2001-01-01")
    .dispatch();
  assert_eq!(
    response.status(),
    Status::SeeOther,
    "prune redirects back with the count"
  );
  assert!(
    response
      .headers()
      .get_one("Location")
      .unwrap_or("")
      .starts_with("/admin/retention?pruned="),
    "the redirect carries the pruned count"
  );

  // The old snapshot is gone; recent snapshots (saved now) are untouched.
  assert_eq!(
    HistoricalTask::count_before(&mut db.connection, before_2001).unwrap(),
    0,
    "no pre-2001 snapshots remain after the prune"
  );
  assert!(
    HistoricalTask::count_before(&mut db.connection, cutoff_before_year(3000)).unwrap() > 0,
    "recent snapshots (saved now) survived the prune"
  );
}

/// The agent twin of the prune action (`POST /api/retention/prune?before=`): token-gated,
/// validated, and it removes the same snapshots as the human screen.
fn agent_prune_is_token_gated_and_validated() {
  seed_old_snapshot();
  let client = client();

  // Token-gated: no token -> 401 (no unauthenticated data deletion).
  let response = client
    .post("/api/retention/prune?before=2001-01-01")
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "prune is token-gated"
  );

  // Malformed cutoff -> 400 (not a silent no-op for an agent).
  let response = client
    .post("/api/retention/prune?before=not-a-date")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "malformed cutoff -> 400"
  );

  // Valid prune -> 200 + ack; the year-2000 snapshot is removed.
  let response = client
    .post("/api/retention/prune?before=2001-01-01")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let ack: serde_json::Value = response.into_json().expect("prune ack json");
  assert!(
    ack["pruned"].as_u64().is_some_and(|n| n >= 1),
    "the ack reports the removed snapshot count"
  );
  assert_eq!(ack["before"], "2001-01-01");
  assert_eq!(ack["actor"], "username1", "attributed to the token's owner");

  let mut db = backend::testdb();
  assert_eq!(
    HistoricalTask::count_before(&mut db.connection, cutoff_before_year(2001)).unwrap(),
    0,
    "no pre-2001 snapshots remain after the agent prune"
  );
}

// Custom harness (see KNOWN_ISSUES L-1): run the cases then `_exit(0)`.
fn main() {
  retention_stats_preview_and_prune();
  agent_prune_is_token_gated_and_validated();
  eprintln!("retention_test: all cases passed");
  unsafe { libc::_exit(0) }
}
