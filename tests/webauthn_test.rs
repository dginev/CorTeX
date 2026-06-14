// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for passkey **enrollment** (`docs/WEBAUTHN_DESIGN.md`): the registration
//! ceremony's `begin` endpoint is gated to a signed-in admin and, when passkeys are enabled,
//! returns a WebAuthn creation challenge; the "Your passkeys" page is signed-in-only and offers
//! enrollment. The full biometric round-trip needs a real authenticator (manual / virtual), so the
//! server-side boundaries are what we assert here.

use cortex::backend::test_db_address;
use cortex::frontend::server::mount_api_with;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  // Enable passkeys for THIS test process before config() is first read. Each harness=false test is
  // its own binary, so the config LazyLock has not loaded yet and these env overrides take effect.
  std::env::set_var("CORTEX_WEBAUTHN__ENABLED", "true");
  std::env::set_var("CORTEX_WEBAUTHN__RP_ID", "localhost");
  std::env::set_var("CORTEX_WEBAUTHN__RP_ORIGIN", "http://localhost:8000");
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_webauthn_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

fn sign_in(client: &Client) {
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
}

fn enrollment_ceremony_boundaries() {
  let client = client();

  // The enrollment ceremony is gated to a signed-in admin.
  let response = client.post("/admin/passkeys/register/begin").dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "enrollment requires a signed-in admin"
  );

  // The management page bounces an anonymous browser to sign-in.
  let response = client.get("/admin/passkeys").dispatch();
  assert!(
    (300..400).contains(&response.status().code),
    "the passkeys page requires sign-in, got {}",
    response.status()
  );

  sign_in(&client);

  // Signed in + passkeys enabled: begin returns a WebAuthn creation challenge.
  let response = client.post("/admin/passkeys/register/begin").dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "begin returns a challenge when enabled and signed in"
  );
  let body = response.into_string().expect("json body");
  assert!(
    body.contains("challenge") && body.contains("publicKey"),
    "the response is a WebAuthn creation challenge, got: {body}"
  );

  // The management page renders with the enroll affordance.
  let response = client.get("/admin/passkeys").dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "signed-in passkeys page renders"
  );
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("Your passkeys"),
    "the management page renders"
  );
  assert!(
    body.contains("enroll-passkey"),
    "the enroll affordance is present when passkeys are enabled"
  );
}

// Custom harness (see KNOWN_ISSUES L-1): run the case then `_exit(0)`.
fn main() {
  enrollment_ceremony_boundaries();
  eprintln!("webauthn_test: all cases passed");
  unsafe { libc::_exit(0) }
}
