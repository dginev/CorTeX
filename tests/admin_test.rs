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

  // Unauthenticated /admin redirects to the sign-in page.
  let response = client.get("/admin").dispatch();
  assert!(
    is_redirect(response.status().code),
    "unauthenticated /admin redirects, got {}",
    response.status()
  );
  assert_eq!(response.headers().get_one("Location"), Some("/admin/login"));

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

  // A valid token (token1 is configured in the test rerun_tokens) signs in + redirects to /admin.
  let response = client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
  assert_eq!(response.headers().get_one("Location"), Some("/admin"));

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

  // Sign out ends the session; /admin redirects to sign-in again.
  let response = client.post("/admin/logout").dispatch();
  assert_eq!(response.headers().get_one("Location"), Some("/admin/login"));
  assert!(
    is_redirect(client.get("/admin").dispatch().status().code),
    "after sign-out, /admin redirects to the sign-in page"
  );
}

// Custom harness (see KNOWN_ISSUES L-1): run the case then `_exit(0)`.
fn main() {
  admin_requires_sign_in_then_grants_access();
  eprintln!("admin_test: all cases passed");
  unsafe { libc::_exit(0) }
}
