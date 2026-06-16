// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the signed-in admin web UI: the `/admin` dashboard is gated by the lightweight
//! token cookie session — unauthenticated browsers are bounced to the sign-in page; a valid rerun
//! token signs in and unlocks the consolidated admin actions; sign-out ends the session.

use cortex::backend::test_db_address;
use cortex::frontend::server::mount_api_with;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  // `tracked` so the session cookie set at sign-in carries to later requests (a real browser).
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_admin_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

fn is_redirect(code: u16) -> bool { (300..400).contains(&code) }

fn admin_requires_sign_in_then_grants_access() {
  let client = client();

  // Unauthenticated /admin redirects to the sign-in page, carrying a `?next=` back to where it was.
  let response = client.get("/admin").dispatch();
  assert!(
    is_redirect(response.status().code),
    "unauthenticated /admin redirects, got {}",
    response.status()
  );
  assert_eq!(
    response.headers().get_one("Location"),
    Some("/admin/login?next=%2Fadmin"),
    "the redirect carries a next= back to the requested screen"
  );

  // A gated screen with a path + query is preserved in `next` too.
  let response = client.get("/admin/audit?actor=alice").dispatch();
  let location = response.headers().get_one("Location").unwrap_or("");
  assert!(
    location.starts_with("/admin/login?next=") && location.contains("audit"),
    "the deep destination is preserved in next=, got {location}"
  );

  // The sign-in page renders.
  let response = client.get("/admin/login").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));

  // A bad token bounces back to the sign-in page, flagged.
  let response = client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=not-a-real-token")
    .dispatch();
  assert_eq!(
    response.headers().get_one("Location"),
    Some("/admin/login?bad=true"),
    "a bad token is rejected"
  );
  // ...and it did NOT grant access.
  assert!(
    is_redirect(client.get("/admin").dispatch().status().code),
    "a rejected sign-in does not unlock /admin"
  );

  // A valid token signs in and returns to the `next` destination it was sent to sign-in from.
  let response = client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1&next=%2Fadmin%2Faudit")
    .dispatch();
  assert_eq!(
    response.headers().get_one("Location"),
    Some("/admin/audit"),
    "a successful sign-in returns to next="
  );

  // Security: the session cookie carries an opaque server-side session id, NOT the raw token.
  let cookie_value = client
    .cookies()
    .get("cortex_admin")
    .map(|cookie| cookie.value().to_string());
  assert!(cookie_value.is_some(), "a session cookie is set on sign-in");
  assert_ne!(
    cookie_value.as_deref(),
    Some("token1"),
    "the cookie is a session id, never the credential itself"
  );

  // Now /admin is accessible (the tracked client carries the session cookie).
  let response = client.get("/admin").dispatch();
  assert_eq!(response.status(), Status::Ok, "signed-in /admin renders");
  let body = response.into_string().expect("html body");
  assert!(body.contains("Admin dashboard"), "the dashboard renders");
  assert!(
    body.contains("Add a corpus"),
    "the add-corpus action is consolidated on the admin dashboard"
  );
  // The live ops console's status cards + the live-refresh indicator render (at-a-glance state).
  assert!(
    body.contains("active jobs")
      && body.contains("workers")
      && body.contains("failed (24h)")
      && body.contains("Last run"),
    "the dashboard shows the live ops status strip (jobs / workers / failures / last run)"
  );
  assert!(
    body.contains("live-dot") && body.contains("/admin/status.json"),
    "the dashboard wires up the live poll of /admin/status.json"
  );

  // The historical-runs management screen renders for a signed-in admin (covers admin-runs.html).
  let response = client.get("/admin/runs").dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "signed-in /admin/runs renders the run-management screen"
  );
  assert!(
    response
      .into_string()
      .expect("html")
      .contains("Historical runs"),
    "the historical-runs management screen renders"
  );

  // Sign out ends the session; /admin redirects to sign-in again.
  let response = client.post("/admin/logout").dispatch();
  assert_eq!(response.headers().get_one("Location"), Some("/admin/login"));
  assert!(
    is_redirect(client.get("/admin").dispatch().status().code),
    "after sign-out, /admin redirects to the sign-in page"
  );
}

// The live ops console's poll feed (`/admin/status.json`) is cookie-gated and returns the snapshot
// DTO as JSON for a signed-in admin.
fn admin_status_feed_is_cookie_gated_and_returns_the_snapshot() {
  let client = client();

  // Unauthenticated XHR → 401 (not an HTML redirect — the page keeps its last-good values).
  let response = client.get("/admin/status.json").dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "the status feed is closed to anonymous callers"
  );

  // Sign in, then the feed returns JSON with the snapshot fields.
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
  let response = client.get("/admin/status.json").dispatch();
  assert_eq!(response.status(), Status::Ok, "signed-in feed is readable");
  assert_eq!(
    response.content_type(),
    Some(ContentType::JSON),
    "the feed is JSON"
  );
  let body: serde_json::Value = response.into_json().expect("the feed is valid JSON");
  for field in [
    "corpus_count",
    "active_jobs",
    "active_sessions",
    "workers_total",
    "workers_in_flight",
    "tasks_todo",
    "jobs_failed_recent",
    "pool_in_use",
    "pool_max",
  ] {
    assert!(
      body.get(field).is_some(),
      "the snapshot DTO carries `{field}`"
    );
  }
  // The pending-conversion backlog is a valid non-negative count (the human twin of the
  // `cortex_tasks_todo` /metrics gauge).
  assert!(
    body["tasks_todo"].as_i64().is_some_and(|todo| todo >= 0),
    "tasks_todo is a non-negative backlog count"
  );
}

// Custom harness (see KNOWN_ISSUES L-1): run the cases then `_exit(0)`.
fn main() {
  admin_requires_sign_in_then_grants_access();
  admin_status_feed_is_cookie_gated_and_returns_the_snapshot();
  eprintln!("admin_test: all cases passed");
  unsafe { libc::_exit(0) }
}
