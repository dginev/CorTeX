// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the services capability: the worker-fleet agent API and its HTML twin.

use cortex::backend::{self, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::models::{NewService, Service};
use cortex::schema::{services, worker_metadata};
use diesel::prelude::*;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;
use serde_json::Value;

const SERVICE_NAME: &str = "fleet_test_svc";
const WORKER_NAME: &str = "fleet-test-worker:1";

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_services_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

/// Clean slate, register a service, and seed one worker with 10 dispatched / 7 returned.
fn seed() -> i32 {
  let mut db = backend::testdb();
  if let Ok(service) = Service::find_by_name(SERVICE_NAME, &mut db.connection) {
    diesel::delete(worker_metadata::table.filter(worker_metadata::service_id.eq(service.id)))
      .execute(&mut db.connection)
      .ok();
  }
  diesel::delete(services::table.filter(services::name.eq(SERVICE_NAME)))
    .execute(&mut db.connection)
    .ok();

  db.add(&NewService {
    name: SERVICE_NAME.to_string(),
    version: 0.1,
    inputformat: "tex".to_string(),
    outputformat: "html".to_string(),
    inputconverter: None,
    complex: true,
    description: "fleet test service".to_string(),
  })
  .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut db.connection).expect("service");

  diesel::sql_query(format!(
    "INSERT INTO worker_metadata \
     (service_id, last_dispatched_task_id, total_dispatched, total_returned, first_seen, \
      time_last_dispatch, name) \
     VALUES ({}, 42, 10, 7, now(), now(), '{WORKER_NAME}')",
    service.id
  ))
  .execute(&mut db.connection)
  .expect("seed worker_metadata");
  service.id
}

/// Signs the tracked client in as an admin (gated screens require the `AdminSession` cookie).
fn sign_in(client: &Client) {
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
}

#[test]
fn worker_fleet_api_and_screen() {
  let service_id = seed();
  let client = client();

  // The /services + /workers screens are admin-only: an unauthenticated browser is bounced to
  // sign-in. After signing in (tracked client carries the cookie) they render.
  let unauth = client.get("/services").dispatch();
  let location = unauth.headers().get_one("Location").unwrap_or("");
  assert!(
    location.starts_with("/admin/login?next="),
    "the services screen requires sign-in (with a return path), got {location}"
  );
  sign_in(&client);

  // --- Agent API: the worker with its tallies + computed in-flight backlog --------------------
  let response = client
    .get(format!("/api/services/{SERVICE_NAME}/workers"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let workers: Value = response.into_json().expect("workers json");
  let workers = workers.as_array().expect("array");
  let worker = workers
    .iter()
    .find(|w| w["name"] == WORKER_NAME)
    .expect("the seeded worker is listed");
  assert_eq!(worker["total_dispatched"], 10);
  assert_eq!(worker["total_returned"], 7);
  assert_eq!(worker["in_flight"], 3, "in_flight = dispatched - returned");
  assert_eq!(worker["last_dispatched_task_id"], 42);
  assert!(
    worker["seconds_since_last_active"].is_number(),
    "the agent twin carries the worker liveness age (parity with the human screen)"
  );
  assert_eq!(
    worker["fresh"], true,
    "a worker last active at now() is fresh"
  );

  // --- HTML twin: the worker-fleet screen renders the worker server-side ----------------------
  let response = client.get(format!("/workers/{SERVICE_NAME}")).dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(body.contains("Workers for"), "renders the fleet screen");
  assert!(
    body.contains(WORKER_NAME),
    "lists the seeded worker server-side"
  );

  // --- Service registry: the agent API lists our service, the HTML screen renders it ----------
  let response = client.get("/api/services").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let services: Value = response.into_json().expect("services json");
  let ours = services
    .as_array()
    .expect("array")
    .iter()
    .find(|s| s["name"] == SERVICE_NAME)
    .expect("the registered service is listed");
  assert_eq!(ours["inputformat"], "tex");
  assert_eq!(ours["outputformat"], "html");

  let response = client.get("/services").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(body.contains("Registered services"), "renders the registry");
  assert!(
    body.contains(SERVICE_NAME),
    "lists the registered service server-side"
  );

  // --- Guards: unknown service is 404 on both surfaces ----------------------------------------
  let response = client
    .get("/api/services/no_such_svc_xyz/workers")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
  let response = client.get("/workers/no_such_svc_xyz").dispatch();
  assert_eq!(response.status(), Status::NotFound);

  // --- Register a service: token-gated; creates a registry entry; 409 on a duplicate -----------
  const NEW_SVC: &str = "registered_via_api_svc";
  const FORM_SVC: &str = "registered_via_form_svc";
  {
    let mut db = backend::testdb();
    for n in [NEW_SVC, FORM_SVC] {
      diesel::delete(services::table.filter(services::name.eq(n)))
        .execute(&mut db.connection)
        .ok();
    }
  }
  let body = serde_json::json!({
    "name": NEW_SVC, "version": 0.2, "inputformat": "tex", "outputformat": "html",
    "inputconverter": "import", "complex": true, "description": "registered in a test",
  });
  // No token -> 401.
  let denied = client
    .post("/api/services")
    .header(ContentType::JSON)
    .body(body.to_string())
    .dispatch();
  assert_eq!(
    denied.status(),
    Status::Unauthorized,
    "register without a token is 401"
  );
  // Valid token -> 201 + the service DTO.
  let created = client
    .post("/api/services?token=token1")
    .header(ContentType::JSON)
    .body(body.to_string())
    .dispatch();
  assert_eq!(
    created.status(),
    Status::Created,
    "register with a token is 201"
  );
  let svc: Value = created.into_json().expect("service json");
  assert_eq!(svc["name"], NEW_SVC);
  assert_eq!(svc["inputconverter"], "import");
  // Duplicate name -> 409.
  let dup = client
    .post("/api/services?token=token1")
    .header(ContentType::JSON)
    .body(body.to_string())
    .dispatch();
  assert_eq!(
    dup.status(),
    Status::Conflict,
    "a duplicate service name is 409"
  );
  // Human form -> 303 redirect to /services; the service is registered.
  let redirected = client
    .post("/services/register")
    .header(ContentType::Form)
    .body(format!(
      "name={FORM_SVC}&version=0.1&inputformat=tex&outputformat=html&complex=true&token=token1"
    ))
    .dispatch();
  assert_eq!(
    redirected.status(),
    Status::SeeOther,
    "the human register form redirects (303)"
  );
  {
    let mut db = backend::testdb();
    assert!(
      Service::find_by_name(FORM_SVC, &mut db.connection).is_ok(),
      "service registered via the human form"
    );
    for n in [NEW_SVC, FORM_SVC] {
      diesel::delete(services::table.filter(services::name.eq(n)))
        .execute(&mut db.connection)
        .ok();
    }
  }

  // --- Regression: a clock-skewed (future) worker timestamp must not panic the screen (F-4) ------
  const SKEWED_WORKER: &str = "fleet-test-worker-future:1";
  {
    let mut db = backend::testdb();
    diesel::sql_query(format!(
      "INSERT INTO worker_metadata \
       (service_id, last_dispatched_task_id, total_dispatched, total_returned, first_seen, \
        time_last_dispatch, name) \
       VALUES ({service_id}, 7, 1, 0, now(), now() + interval '1 hour', '{SKEWED_WORKER}')"
    ))
    .execute(&mut db.connection)
    .expect("seed a future-timestamped worker");
  }
  // The HTML screen used to `.unwrap()`-panic on the future timestamp; now it renders.
  let response = client.get(format!("/workers/{SERVICE_NAME}")).dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "a future-timestamped worker no longer crashes the fleet screen"
  );
  // The agent twin clamps the skewed liveness age to 0 (never negative, never a panic).
  let response = client
    .get(format!("/api/services/{SERVICE_NAME}/workers"))
    .dispatch();
  let workers: Value = response.into_json().expect("workers json");
  let skewed = workers
    .as_array()
    .expect("array")
    .iter()
    .find(|w| w["name"] == SKEWED_WORKER)
    .expect("the skewed worker is listed");
  assert_eq!(
    skewed["seconds_since_last_active"], 0,
    "a future timestamp clamps to 0"
  );

  // cleanup
  let mut db = backend::testdb();
  diesel::delete(worker_metadata::table.filter(worker_metadata::service_id.eq(service_id)))
    .execute(&mut db.connection)
    .ok();
  diesel::delete(services::table.filter(services::name.eq(SERVICE_NAME)))
    .execute(&mut db.connection)
    .ok();
  // Exit before the racy libpq/OpenSSL atexit teardown of the still-live Client (KNOWN_ISSUES L-1).
  unsafe { libc::_exit(0) }
}
