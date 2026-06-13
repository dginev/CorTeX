// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! High-level contract tests for the management/health HTTP surface.
//!
//! These hold the *shape* of the interfaces and the happy-path data contracts that the agent API
//! and the human UI both depend on — not the internals.

use cortex::frontend::server::mount_management;
use rocket::http::{Accept, ContentType, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  Client::tracked(mount_management(rocket::build())).expect("a valid rocket instance")
}

#[test]
fn get_api_config_returns_masked_contract() {
  let client = client();
  let response = client.get("/api/config").header(Accept::JSON).dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));

  let body: serde_json::Value = response.into_json().expect("a JSON body");

  // Data contract the API + UI depend on:
  assert!(body["database"]["url"].is_string());
  assert!(body["dispatcher"]["source_port"].is_number());
  assert!(body["dispatcher"]["result_port"].is_number());
  assert!(body["dispatcher"]["queue_size"].is_number());
  assert!(body["cache"]["redis_url"].is_string());
  assert!(body["assets"]["template_dir"].is_string());
  assert!(body["assets"]["public_dir"].is_string());
  assert!(body["auth"]["rerun_token_count"].is_number());
  assert!(body["auth"]["captcha_secret_set"].is_boolean());

  // Security contract: secrets are never exposed.
  assert!(
    body["auth"]["rerun_tokens"].is_null(),
    "raw rerun_tokens must not be exposed"
  );
  assert!(
    body["auth"]["captcha_secret"].is_null(),
    "captcha_secret must not be exposed"
  );
  let db_url = body["database"]["url"].as_str().expect("db url string");
  assert!(
    db_url.contains("***"),
    "db password must be masked, got: {db_url}"
  );
}

#[test]
fn healthz_reports_ok_when_db_reachable() {
  let client = client();
  let response = client.get("/healthz").dispatch();
  assert_eq!(response.status(), Status::Ok);

  let body: serde_json::Value = response.into_json().expect("a JSON body");
  assert_eq!(body["status"], "ok");
  assert_eq!(body["database"]["reachable"], true);
  assert_eq!(body["migrations"]["current"], true);
}
