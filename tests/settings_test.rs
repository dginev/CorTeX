// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract tests for the Settings page (the HTML twin of `/api/config`) and the config write path.
//! High level: they hold the interface shape and the happy-path data contract, including the
//! security contract that secrets are neither shown nor persisted.

use cortex::backend::test_db_address;
use cortex::frontend::server::mount_api_with;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;
use std::path::PathBuf;

fn temp_config_path(tag: &str) -> PathBuf {
  let mut path = std::env::temp_dir();
  path.push(format!("cortex_settings_test_{tag}.toml"));
  let _ = std::fs::remove_file(&path);
  path
}

fn client(config_file: PathBuf) -> Client {
  // The builder attaches the template fairing; we only point it at the repo templates.
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let rocket = mount_api_with(rocket::custom(figment), config_file, test_db_address());
  Client::tracked(rocket).expect("a valid rocket instance")
}

/// Signs the tracked client in as an admin (the `/settings` screen requires the `AdminSession`
/// cookie; the `/api/config` + `POST /settings` write paths keep their own token guards).
fn sign_in(client: &Client) {
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
}

fn settings_page_renders_masked_html() {
  let client = client(temp_config_path("read"));
  // Admin-only: unauthenticated → redirect to sign-in (with a return path).
  let unauth = client.get("/settings").dispatch();
  assert!(
    unauth
      .headers()
      .get_one("Location")
      .unwrap_or("")
      .starts_with("/admin/login?next="),
    "the settings screen requires sign-in"
  );
  sign_in(&client);
  let response = client.get("/settings").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));

  let body = response.into_string().expect("an html body");
  assert!(
    body.contains("***"),
    "the masked db password should be shown"
  );
  assert!(
    !body.contains("rerun_tokens"),
    "raw tokens must not leak into the page"
  );
}

fn put_api_config_merges_and_persists() {
  let path = temp_config_path("put");
  let client = client(path.clone());
  // Token-gated since X-5 (rewriting the running config is a consequential mutation), so the agent
  // PUT carries a token exactly like the management_api_test gate asserts.
  let response = client
    .put("/api/config?token=token1")
    .header(ContentType::JSON)
    .body(r#"{"dispatcher":{"queue_size":4242},"jobs":{"stale_timeout_seconds":12345}}"#)
    .dispatch();
  assert_eq!(response.status(), Status::Ok);

  let body: serde_json::Value = response.into_json().expect("a JSON body");
  assert_eq!(body["dispatcher"]["queue_size"], 4242);
  assert_eq!(
    body["jobs"]["stale_timeout_seconds"], 12345,
    "the jobs stall-reap threshold merges + is returned (agent write path)"
  );

  let written = std::fs::read_to_string(&path).expect("config file written");
  assert!(
    written.contains("4242"),
    "persisted toml must contain the new value"
  );
  assert!(
    written.contains("12345"),
    "the jobs stall-reap threshold is persisted"
  );
  assert!(
    !written.contains("rerun_tokens"),
    "secrets must not be written to the config file"
  );
}

fn post_settings_form_persists_and_redirects() {
  let path = temp_config_path("post");
  let client = client(path.clone());
  // post_settings is now gated by the AdminSession cookie (the Settings screen is signed-in-only).
  sign_in(&client);
  let form = "dispatcher_source_port=51695&dispatcher_result_port=51696\
              &dispatcher_queue_size=4242&dispatcher_message_size=100000\
              &dispatcher_max_in_flight=5000&dispatcher_max_result_bytes=1234567\
              &dispatcher_report_refresh_interval_seconds=7200\
              &jobs_stale_timeout_seconds=10800\
              &assets_template_dir=templates&assets_public_dir=public";
  let response = client
    .post("/settings")
    .header(ContentType::Form)
    .body(form)
    .dispatch();

  assert!(
    (300..400).contains(&response.status().code),
    "a form save should redirect, got {}",
    response.status()
  );
  // The post-redirect-get carries `?saved=true` so the reloaded screen flashes a save confirmation.
  assert_eq!(
    response.headers().get_one("Location"),
    Some("/settings?saved=true")
  );

  let written = std::fs::read_to_string(&path).expect("config file written");
  assert!(written.contains("4242"));
  assert!(
    written.contains("7200"),
    "the report-refresh interval is editable + persisted"
  );
  assert!(
    written.contains("10800"),
    "the jobs stall-reap threshold is editable from the form + persisted"
  );
  assert!(
    written.contains("1234567"),
    "the dispatcher max-result-bytes cap is editable from the form + persisted"
  );
}

// Custom harness (Cargo.toml `harness = false`): run the cases then `_exit(0)` to skip the racy
// libpq/OpenSSL atexit teardown that SIGSEGVs after assertions pass (KNOWN_ISSUES L-1). A panic in
// any case aborts non-zero, so real failures still fail CI.
fn main() {
  settings_page_renders_masked_html();
  put_api_config_merges_and_persists();
  post_settings_form_persists_and_redirects();
  eprintln!("settings_test: all cases passed");
  unsafe { libc::_exit(0) }
}
