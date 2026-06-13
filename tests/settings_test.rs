// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract tests for the Settings page (the HTML twin of `/api/config`) and the config write path.
//! High level: they hold the interface shape and the happy-path data contract, including the
//! security contract that secrets are neither shown nor persisted.

use cortex::frontend::server::mount_management_with;
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
  let rocket = mount_management_with(rocket::custom(figment), config_file);
  Client::tracked(rocket).expect("a valid rocket instance")
}

#[test]
fn settings_page_renders_masked_html() {
  let client = client(temp_config_path("read"));
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

#[test]
fn put_api_config_merges_and_persists() {
  let path = temp_config_path("put");
  let client = client(path.clone());
  let response = client
    .put("/api/config")
    .header(ContentType::JSON)
    .body(r#"{"dispatcher":{"queue_size":4242}}"#)
    .dispatch();
  assert_eq!(response.status(), Status::Ok);

  let body: serde_json::Value = response.into_json().expect("a JSON body");
  assert_eq!(body["dispatcher"]["queue_size"], 4242);

  let written = std::fs::read_to_string(&path).expect("config file written");
  assert!(
    written.contains("4242"),
    "persisted toml must contain the new value"
  );
  assert!(
    !written.contains("rerun_tokens"),
    "secrets must not be written to the config file"
  );
}

#[test]
fn post_settings_form_persists_and_redirects() {
  let path = temp_config_path("post");
  let client = client(path.clone());
  let form = "dispatcher_source_port=51695&dispatcher_result_port=51696\
              &dispatcher_queue_size=4242&dispatcher_message_size=100000\
              &cache_redis_url=redis://127.0.0.1/&cache_required=false\
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
  assert_eq!(response.headers().get_one("Location"), Some("/settings"));

  let written = std::fs::read_to_string(&path).expect("config file written");
  assert!(written.contains("4242"));
}
