// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the **active sessions** admin view (`frontend::sessions`): signed-in admins
//! see who is currently signed in and can revoke an identity's sessions; the agent twin lists them
//! token-gated. Uses the secondary `token2`/`username2` fixture so a revoke never disturbs the many
//! `username1`-based tests running alongside it.

use cortex::backend::test_db_address;
use cortex::frontend::server::mount_api_with;
use rocket::http::{ContentType, Header, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_sessions_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

fn sign_in_as(client: &Client, token: &str) {
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body(format!("token={token}"))
    .dispatch();
}

fn sessions_view_and_revoke() {
  let client = client();

  // The screen is signed-in-only: an anonymous browser is bounced to sign-in (with a return path).
  let response = client.get("/admin/sessions").dispatch();
  assert!(
    response
      .headers()
      .get_one("Location")
      .unwrap_or("")
      .starts_with("/admin/login?next="),
    "the sessions screen requires sign-in"
  );

  // The agent twin is token-gated.
  assert_eq!(
    client.get("/api/sessions").dispatch().status(),
    Status::Unauthorized,
    "listing active sessions requires a token"
  );

  // Sign in as username2 (token2) — this opens a session we will list + revoke in isolation.
  sign_in_as(&client, "token2");

  // The screen renders and shows our own session, marked as this device.
  let response = client.get("/admin/sessions").dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "signed-in sessions screen renders"
  );
  let body = response.into_string().expect("html body");
  assert!(body.contains("Active sessions"), "the screen renders");
  assert!(body.contains("username2"), "our session is listed");
  assert!(
    body.contains("this device"),
    "our own session is marked current"
  );

  // The agent API lists it too (token-gated), without exposing any session id.
  let response = client
    .get("/api/sessions")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let json = response.into_string().expect("json body");
  assert!(
    json.contains("username2"),
    "the agent list includes the session"
  );

  // Revoke all of username2's sessions — that includes our current one, so the next request is
  // unauthenticated again.
  let response = client
    .post("/admin/sessions/revoke?owner=username2")
    .dispatch();
  assert_eq!(
    response.status(),
    Status::SeeOther,
    "revoke redirects back to the screen"
  );
  let after = client.get("/admin/sessions").dispatch();
  assert!(
    after
      .headers()
      .get_one("Location")
      .unwrap_or("")
      .starts_with("/admin/login"),
    "after revoking our own identity, we are signed out"
  );
}

/// The agent twin of the revoke action (`POST /api/sessions/revoke?owner=`): token-gated, audited,
/// idempotent — ends the same sessions the human screen does.
fn agent_revoke_is_token_gated_and_idempotent() {
  let client = client();
  sign_in_as(&client, "token2"); // open a username2 session to revoke in isolation

  // Token-gated: no token -> 401.
  let response = client
    .post("/api/sessions/revoke?owner=username2")
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "revoke is token-gated"
  );

  // With a token -> 200 ack; the open session is ended.
  let response = client
    .post("/api/sessions/revoke?owner=username2")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let ack: serde_json::Value = response.into_json().expect("revoke ack json");
  assert_eq!(ack["owner"], "username2");
  assert!(
    ack["revoked"].as_u64().is_some_and(|n| n >= 1),
    "the open username2 session was ended"
  );
  assert_eq!(ack["actor"], "username1", "attributed to the token's owner");

  // Idempotent: revoking again ends zero (no error, no surprise).
  let response = client
    .post("/api/sessions/revoke?owner=username2")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch();
  let ack: serde_json::Value = response.into_json().expect("revoke ack json");
  assert_eq!(ack["revoked"], 0, "no sessions remain to revoke");
}

// Custom harness (see KNOWN_ISSUES L-1): run the cases then `_exit(0)`.
fn main() {
  sessions_view_and_revoke();
  agent_revoke_is_token_gated_and_idempotent();
  eprintln!("sessions_test: all cases passed");
  unsafe { libc::_exit(0) }
}
