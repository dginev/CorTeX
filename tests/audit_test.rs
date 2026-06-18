// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the **accounting** pillar (AAA — `docs/archive/AAA_DESIGN.md`): the audit
//! fairing (`frontend::audit`) records every mutating admin request to the `audit_log`, attributed
//! to the authenticated actor. An authenticated agent mutation leaves a row naming the action,
//! target, actor and outcome; an unauthenticated attempt is *also* recorded — with an empty actor —
//! so a denied write is observable, not silent.

use cortex::backend::{self, test_db_address};
use cortex::frontend::actor::owner_for_token;
use cortex::frontend::server::mount_api_with;
use cortex::models::AuditEntry;
use diesel::prelude::*;
use rocket::http::{Header, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_audit_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

/// The highest audit id currently present (a baseline, since the test DB is shared across runs).
fn latest_id(connection: &mut PgConnection) -> i64 {
  AuditEntry::recent(connection, 1)
    .ok()
    .and_then(|rows| rows.first().map(|row| row.id))
    .unwrap_or(0)
}

/// Polls (the fairing records on a blocking task, off the response path) for the first new audit
/// row on `target` after `after`, up to ~2s.
fn await_row(connection: &mut PgConnection, after: i64, target: &str) -> Option<AuditEntry> {
  for _ in 0..40 {
    let recent = AuditEntry::recent(connection, 50).unwrap_or_default();
    if let Some(entry) = recent
      .into_iter()
      .find(|entry| entry.id > after && entry.target == target)
    {
      return Some(entry);
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
  }
  None
}

fn audit_records_authenticated_mutation() {
  const TARGET: &str = "/api/maintenance/analyze";
  let mut backend = backend::testdb();
  let owner = owner_for_token("token1").expect("token1 maps to an owner in the test config");
  let before = latest_id(&mut backend.connection);

  // An authenticated, side-effect-light mutation: ANALYZE via the agent API (header token → actor).
  let client = client();
  let response = client
    .post(TARGET)
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch();
  assert_eq!(response.status(), Status::Accepted, "analyze returns 202");

  let entry = await_row(&mut backend.connection, before, TARGET)
    .expect("the analyze mutation left an audit_log row");
  assert_eq!(
    entry.action, "analyze",
    "the row records the route name as the action"
  );
  assert_eq!(
    entry.actor, owner,
    "the row attributes the action to token1's owner"
  );
  assert_eq!(
    entry.outcome, "202",
    "the row records the response status as the outcome"
  );

  // An UNauthenticated mutation is still recorded — with an empty actor (a useful security signal).
  let response = client.post(TARGET).dispatch();
  assert_eq!(response.status(), Status::Unauthorized, "no token → 401");
  let denied = await_row(&mut backend.connection, entry.id, TARGET)
    .expect("the unauthenticated attempt is also recorded");
  assert_eq!(
    denied.actor, "",
    "an unauthenticated action has an empty actor"
  );
  assert_eq!(
    denied.outcome, "401",
    "the denied attempt records its 401 outcome"
  );
}

/// The read view (symmetry contract): the agent `GET /api/audit` (token-gated) and the human
/// `GET /admin/audit` screen (signed-in only) both expose the rows the fairing recorded above.
fn audit_read_view() {
  let client = client();

  // Agent API: token-gated. Without a token → 401.
  let response = client.get("/api/audit").dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "reading the audit log requires a token"
  );

  // With a token → 200 + a JSON array; filtered to username1 it includes the analyze action above.
  let response = client
    .get("/api/audit?actor=username1")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok, "token reads the audit log");
  let body = response.into_string().expect("json body");
  assert!(
    body.contains("analyze"),
    "the audit read surfaces the recorded analyze action, got: {body}"
  );

  // Pagination (owner: cap 100 per page): the response is a page object (not a bare array), echoes
  // the page index + size, and a page far beyond the data is empty with has_next=false.
  let page0 = client
    .get("/api/audit?page=0")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch()
    .into_json::<serde_json::Value>()
    .expect("audit page json");
  assert_eq!(page0["page"], 0, "page index echoed");
  assert_eq!(page0["page_size"], 100, "100 rows per page");
  assert!(page0["entries"].is_array(), "entries is the row array");
  let page_far = client
    .get("/api/audit?page=9999")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch()
    .into_json::<serde_json::Value>()
    .expect("audit page json");
  assert_eq!(
    page_far["entries"].as_array().map(|rows| rows.len()),
    Some(0),
    "a page far beyond the data is empty"
  );
  assert_eq!(page_far["has_next"], false, "no older page beyond the data");

  // Human screen: an unauthenticated browser is redirected to the sign-in page.
  let response = client.get("/admin/audit").dispatch();
  assert!(
    (300..400).contains(&response.status().code),
    "unauthenticated /admin/audit redirects, got {}",
    response.status()
  );
  assert!(
    response
      .headers()
      .get_one("Location")
      .unwrap_or("")
      .starts_with("/admin/login?next="),
    "the audit screen requires sign-in (with a return path)"
  );

  // Signed in (tracked client carries the cookie), the screen renders.
  client
    .post("/admin/login")
    .header(rocket::http::ContentType::Form)
    .body("token=token1")
    .dispatch();
  let response = client.get("/admin/audit").dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "signed-in /admin/audit renders"
  );
  let body = response.into_string().expect("html body");
  assert!(body.contains("Audit log"), "the audit screen renders");
}

// Custom harness (see KNOWN_ISSUES L-1): run the cases then `_exit(0)`.
fn main() {
  audit_records_authenticated_mutation();
  audit_read_view();
  eprintln!("audit_test: all cases passed");
  unsafe { libc::_exit(0) }
}
