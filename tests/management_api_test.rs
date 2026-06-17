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

use cortex::backend::test_db_address;
use cortex::frontend::server::mount_api_with;
use rocket::http::{Accept, ContentType, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  let config_file = std::env::temp_dir().join("cortex_mgmt_api_test.toml");
  Client::tracked(mount_api_with(
    rocket::build(),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

/// Signs the tracked client in as an admin (the `/health` + `/settings` screens require the
/// `AdminSession` cookie; the `/healthz` + `/api/config` twins keep their own guards).
fn sign_in(client: &Client) {
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
}

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
  assert!(body["dispatcher"]["max_in_flight"].is_number());
  // The result-archive size cap (W-1③) is in the contract, so the agent + Settings UI can manage
  // it.
  assert!(body["dispatcher"]["max_result_bytes"].is_number());
  assert!(
    body.get("cache").is_none(),
    "the removed Redis cache config is no longer exposed"
  );
  assert!(body["assets"]["template_dir"].is_string());
  assert!(body["assets"]["public_dir"].is_string());
  // The operator-tunable background-job stall-reap threshold is exposed (so the agent + settings UI
  // can read/manage it, not just hand-edit the file — KNOWN_ISSUES W-4 is fully surfaced).
  assert!(
    body["jobs"]["stale_timeout_seconds"].is_number(),
    "jobs.stale_timeout_seconds must be in the config contract"
  );
  assert!(body["auth"]["rerun_token_count"].is_number());
  // The removed captcha secret is gone from the contract (bot protection is a deployment concern).
  assert!(
    body["auth"].get("captcha_secret_set").is_none(),
    "the removed captcha config is no longer exposed"
  );
  // Passkey (WebAuthn) settings are surfaced (non-secret): enabled flag + relying party.
  assert!(body["webauthn"]["enabled"].is_boolean());
  assert!(body["webauthn"]["rp_id"].is_string());

  // Security contract: secrets are never exposed.
  assert!(
    body["auth"]["rerun_tokens"].is_null(),
    "raw rerun_tokens must not be exposed"
  );
  let db_url = body["database"]["url"].as_str().expect("db url string");
  assert!(
    db_url.contains("***"),
    "db password must be masked, got: {db_url}"
  );
}

fn healthz_reports_ok_when_db_reachable() {
  let client = client();
  // The PUBLIC liveness probe (KNOWN_ISSUES X-1): minimal `{status, database.reachable}` only, no
  // token. It must NOT leak the internal topology that the detailed report carries.
  let response = client.get("/healthz").dispatch();
  assert_eq!(response.status(), Status::Ok);
  let body: serde_json::Value = response.into_json().expect("a JSON body");
  assert_eq!(body["status"], "ok");
  assert_eq!(body["database"]["reachable"], true);
  for leaked in [
    "pool",
    "dispatcher",
    "storage",
    "migrations",
    "remediations",
  ] {
    assert!(
      body.get(leaked).is_none(),
      "the public liveness probe must not expose `{leaked}` (X-1)"
    );
  }

  // The detailed report is the TOKEN-GATED agent twin `/api/health` — no token is a clean 401.
  assert_eq!(
    client.get("/api/health").dispatch().status(),
    Status::Unauthorized,
    "the detailed health report requires a token (X-1)"
  );

  // With a token it serves the full report (the agent twin of the admin `/health` screen).
  let body: serde_json::Value = client
    .get("/api/health")
    .header(rocket::http::Header::new("X-Cortex-Token", "token1"))
    .dispatch()
    .into_json()
    .expect("a JSON body");
  assert_eq!(body["status"], "ok");
  assert_eq!(body["database"]["reachable"], true);
  assert_eq!(body["migrations"]["current"], true);
  // Pool utilization is reported (the load/saturation signal): max ≥ in_use, all fields present.
  let pool = &body["pool"];
  assert!(
    pool["max"].as_u64().expect("pool max") >= 1,
    "pool max is reported"
  );
  assert!(pool["in_use"].is_u64() && pool["idle"].is_u64() && pool["connections"].is_u64());
  assert!(
    pool["in_use"].as_u64().unwrap() <= pool["max"].as_u64().unwrap(),
    "in_use never exceeds max"
  );

  // Dispatcher reachability is probed and reported (informational — does not flip `status`, which
  // is still `ok` here even though no dispatcher runs in the test).
  let dispatcher = &body["dispatcher"];
  assert!(
    dispatcher["reachable"].is_boolean(),
    "dispatcher reachability is reported"
  );
  assert!(
    dispatcher["source_port"].as_u64().expect("source_port") >= 1
      && dispatcher["result_port"].as_u64().expect("result_port") >= 1,
    "dispatcher ports are reported"
  );

  // Shared document-storage reachability is reported (informational — does not flip `status`).
  let storage = &body["storage"];
  assert!(
    storage["corpora_checked"].is_u64(),
    "storage corpora_checked is reported"
  );
  assert!(
    storage["unreadable"].is_array(),
    "storage unreadable list is reported"
  );

  // Remediations: the report carries actionable operator guidance (the runtime twin of `cortex
  // doctor`). No dispatcher runs in the test, so the "dispatcher not listening" hint is present.
  let remediations = body["remediations"]
    .as_array()
    .expect("remediations is an array");
  assert!(
    remediations.iter().any(|hint| hint
      .as_str()
      .unwrap_or("")
      .contains("dispatcher not listening")),
    "an unreachable dispatcher yields an actionable remediation in the JSON twin"
  );

  // The human twin renders the same report as an HTML screen (shared HealthDto). Like the gated
  // `/api/health` above, the `/health` screen is admin-only: unauthenticated → sign-in.
  let unauth = client.get("/health").dispatch();
  assert!(
    unauth
      .headers()
      .get_one("Location")
      .unwrap_or("")
      .starts_with("/admin/login?next="),
    "the health screen requires sign-in with a return path (the /healthz probe stays open)"
  );
  sign_in(&client);
  let response = client.get("/health").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let html = response.into_string().expect("html body");
  assert!(
    html.contains("System health"),
    "the human health screen renders"
  );
  assert!(
    html.contains("Recommended actions"),
    "the health screen renders the actionable remediation block (dispatcher down in the test)"
  );
}

/// An agent's write-validation failures (409 conflict, 422 unprocessable) come back as the same
/// content-negotiated JSON envelope `{ error, status }` as the other error statuses — not Rocket's
/// default page. These two statuses are the common write-failure codes (name clash / bad input) and
/// previously had no catcher.
fn api_write_errors_return_the_json_envelope() {
  let client = client();

  // 409: registering an already-seeded service name (`import`) clashes.
  let conflict = client
    .post("/api/services?token=token1")
    .header(ContentType::JSON)
    .body(r#"{"name":"import","version":0.1,"inputformat":"tex","outputformat":"html","complex":false}"#)
    .dispatch();
  assert_eq!(
    conflict.status(),
    Status::Conflict,
    "a duplicate service name is 409"
  );
  assert_eq!(
    conflict.content_type(),
    Some(ContentType::JSON),
    "an agent's 409 carries a JSON body, not the HTML error page"
  );
  let body: serde_json::Value = conflict.into_json().expect("a JSON error envelope");
  assert_eq!(body["status"], 409);
  assert!(
    body["error"].as_str().is_some_and(|m| !m.is_empty()),
    "the envelope carries an explanatory message"
  );

  // 422: an import whose path is not a readable directory is pre-flighted (no corpus is created).
  let unprocessable = client
    .post("/api/corpora?token=token1")
    .header(ContentType::JSON)
    .body(r#"{"name":"catcher-422-probe-corpus","path":"/no/such/dir/xyz-cortex","complex":false}"#)
    .dispatch();
  assert_eq!(
    unprocessable.status(),
    Status::UnprocessableEntity,
    "an unreadable import path is 422"
  );
  assert_eq!(unprocessable.content_type(), Some(ContentType::JSON));
  let body: serde_json::Value = unprocessable.into_json().expect("a JSON error envelope");
  assert_eq!(body["status"], 422);
}

fn api_index_lists_the_agent_surface() {
  let client = client();
  let response = client.get("/api").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));

  let body: serde_json::Value = response.into_json().expect("a JSON body");
  // The API home points an agent at the full typed contract + the human reference.
  assert_eq!(
    body["openapi"], "/api/openapi.json",
    "the index points at the OpenAPI spec"
  );
  assert_eq!(body["docs"], "/api/docs", "the index points at the docs");
  assert!(
    body["description"]
      .as_str()
      .unwrap_or("")
      .contains("X-Cortex-Token"),
    "the index orients the agent (incl. the auth hint for mutations)"
  );
  let endpoints = body["endpoints"].as_array().expect("endpoints array");
  assert!(!endpoints.is_empty(), "the agent surface is non-empty");
  assert_eq!(
    body["count"].as_u64().unwrap() as usize,
    endpoints.len(),
    "count matches the listed endpoints"
  );
  // Every listed endpoint is part of the agent surface (under /api), and known ones are
  // discoverable.
  assert!(
    endpoints
      .iter()
      .all(|e| e["uri"].as_str().unwrap().starts_with("/api")),
    "the index lists only /api endpoints"
  );
  assert!(
    endpoints
      .iter()
      .any(|e| e["uri"] == "/api/corpora" && e["method"] == "GET"),
    "the corpora-listing endpoint is discoverable, with method + handler name"
  );
}

fn api_status_is_token_gated_agent_twin() {
  // `GET /api/status` is the agent twin of the dashboard's cookie-gated `/admin/status.json`:
  // token-gated, returns the same AdminStatusDto snapshot. A monitoring agent gets system state
  // over `/api` without an admin cookie.
  let client = client();
  assert_eq!(
    client.get("/api/status").dispatch().status(),
    Status::Unauthorized,
    "/api/status without a token is 401 (system status is not public)"
  );
  let body: serde_json::Value = client
    .get("/api/status?token=token1")
    .dispatch()
    .into_json()
    .expect("status json");
  for field in [
    "corpus_count",
    "active_jobs",
    "workers_total",
    "tasks_todo",
    "pool_max",
  ] {
    assert!(
      body[field].is_number(),
      "/api/status snapshot carries `{field}`"
    );
  }
}

fn reindex_is_token_gated() {
  // The online-reindex maintenance trigger is a token-gated write. We assert *only* the 401 path
  // here so the test never spawns a real REINDEX (which `_exit` would interrupt, leaving an
  // invalid index in the test DB); the 202 + job-handle path mirrors the tested refresh endpoint
  // and is smoke-tested against the scratch DB.
  let client = client();
  let response = client.post("/api/maintenance/reindex").dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "reindex without a token is 401 (no unauthenticated maintenance)"
  );
}

fn put_config_is_token_gated() {
  // Rewriting the running configuration (dispatcher ports/sizes, asset dirs, the job stall
  // threshold) is a consequential mutation, so PUT /api/config must require a token exactly like
  // every other agent write — regression guard for the auth hole where it had no Actor guard.
  let client = client();
  let patch = r#"{"jobs":{"stale_timeout_seconds":7200}}"#;
  // No token -> 401 (no unauthenticated config rewrite).
  let denied = client
    .put("/api/config")
    .header(ContentType::JSON)
    .body(patch)
    .dispatch();
  assert_eq!(
    denied.status(),
    Status::Unauthorized,
    "PUT /api/config without a token is 401 (no unauthenticated config rewrite)"
  );
  // A valid token -> 200 with the masked config (the gate admits authenticated writes + still
  // merges).
  let ok = client
    .put("/api/config?token=token1")
    .header(ContentType::JSON)
    .body(patch)
    .dispatch();
  assert_eq!(
    ok.status(),
    Status::Ok,
    "PUT /api/config with a valid token succeeds"
  );
}

fn all_api_writes_reject_untokened_requests() {
  // Class-level regression guard for X-5: PUT /api/config shipped *ungated* and went undetected
  // because no test exercised its write path. EVERY mutating `/api` route must `401` an un-tokened
  // request — the `Actor` request guard runs before the handler body, so a *missing* guard surfaces
  // here as a non-401 status. The routes are auto-discovered from the live table, so a **future**
  // ungated write endpoint fails this test too (no per-endpoint test needed to catch the whole
  // class).
  use rocket::http::{MediaType, Method};
  let client = client();
  let writes: Vec<(Method, String, bool)> = client
    .rocket()
    .routes()
    .filter(|r| matches!(r.method, Method::Post | Method::Put | Method::Delete))
    .filter(|r| r.uri.to_string().starts_with("/api/"))
    .map(|r| {
      let is_json = r.format.as_ref() == Some(&MediaType::JSON);
      (r.method, r.uri.to_string(), is_json)
    })
    .collect();
  assert!(
    writes.len() >= 10,
    "expected to auto-discover the /api write routes, found {}",
    writes.len()
  );
  for (method, uri, is_json) in writes {
    // Concretize: drop the query string, replace each `<param>` / `<param..>` path segment with `x`
    // (every /api write uses string path params, so `x` always matches the route).
    let path = uri
      .split('?')
      .next()
      .unwrap_or("")
      .split('/')
      .map(|seg| if seg.starts_with('<') { "x" } else { seg })
      .collect::<Vec<_>>()
      .join("/");
    let mut req = match method {
      Method::Post => client.post(path.clone()),
      Method::Put => client.put(path.clone()),
      Method::Delete => client.delete(path.clone()),
      _ => unreachable!("filtered to POST/PUT/DELETE above"),
    };
    // A JSON-body route only *matches* with the json content-type; send a minimal body so the
    // request reaches the Actor guard (which runs before the body is parsed — the content is
    // irrelevant to the auth check).
    if is_json {
      req = req.header(ContentType::JSON).body("{}");
    }
    let status = req.dispatch().status();
    assert_eq!(
      status,
      Status::Unauthorized,
      "{method} {path} must be 401 without a token (is its Actor guard missing?), got {status}"
    );
  }
}

fn all_api_deletes_require_matching_confirm() {
  // Destructive-op safety invariant (parallel to the auth guard): every DELETE `/api` route must
  // reject a *tokened* request that lacks a matching `confirm` with 400, so a single accidental or
  // replayed call can't wipe a corpus / service / pair's data. Auto-discovered from the live table,
  // so a future destructive DELETE that forgets the confirm gate fails here too.
  use rocket::http::Method;
  let client = client();
  let deletes: Vec<String> = client
    .rocket()
    .routes()
    .filter(|r| r.method == Method::Delete && r.uri.to_string().starts_with("/api/"))
    .map(|r| r.uri.to_string())
    .collect();
  assert!(
    !deletes.is_empty(),
    "expected to auto-discover the destructive DELETE /api routes"
  );
  for uri in deletes {
    let path = uri
      .split('?')
      .next()
      .unwrap_or("")
      .split('/')
      .map(|seg| if seg.starts_with('<') { "x" } else { seg })
      .collect::<Vec<_>>()
      .join("/");
    // A valid token passes the Actor guard, but with no `confirm` the handler's confirmation check
    // must reject the destructive action with 400 (no accidental wipe on a single tokened call).
    let status = client
      .delete(format!("{path}?token=token1"))
      .dispatch()
      .status();
    assert_eq!(
      status,
      Status::BadRequest,
      "DELETE {path} with a token but no confirm must be 400 (is its confirm gate missing?), got {status}"
    );
  }
}

fn healthz_flags_unreadable_corpus_storage() {
  // A corpus whose configured source directory is missing on disk (a moved/unmounted /data mount)
  // is surfaced in the storage-health section — informational, so it does not flip the status.
  use cortex::models::NewCorpus;
  use cortex::schema::corpora;
  use diesel::prelude::*;
  const PROBE_NAME: &str = "storage_health_probe_corpus";
  const BAD_PATH: &str = "/nonexistent/cortex/storage-health-probe";

  let mut db = cortex::backend::testdb();
  diesel::delete(corpora::table.filter(corpora::name.eq(PROBE_NAME)))
    .execute(&mut db.connection)
    .ok();
  db.add(&NewCorpus {
    name: PROBE_NAME.to_string(),
    path: BAD_PATH.to_string(),
    complex: false,
    description: String::new(),
  })
  .expect("seed a corpus with a missing source path");

  let client = client();
  // Storage detail lives on the token-gated `/api/health` (X-1), not the public liveness probe.
  let body: serde_json::Value = client
    .get("/api/health")
    .header(rocket::http::Header::new("X-Cortex-Token", "token1"))
    .dispatch()
    .into_json()
    .expect("a JSON body");
  assert_eq!(
    body["status"], "ok",
    "a missing corpus path is informational, not a frontend-liveness failure"
  );
  let unreadable = body["storage"]["unreadable"]
    .as_array()
    .expect("unreadable array");
  assert!(
    unreadable
      .iter()
      .any(|c| c["name"] == PROBE_NAME && c["path"] == BAD_PATH),
    "the corpus with a missing source directory is flagged unreadable"
  );

  diesel::delete(corpora::table.filter(corpora::name.eq(PROBE_NAME)))
    .execute(&mut db.connection)
    .ok();
}

fn analyze_is_token_gated() {
  // The planner-statistics refresh is the same token-gated maintenance write as reindex; assert the
  // 401 path (its 202 + job-handle path is structurally identical to the tested reindex endpoint).
  let client = client();
  let response = client.post("/api/maintenance/analyze").dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "analyze without a token is 401 (no unauthenticated maintenance)"
  );
}

fn openapi_spec_and_rapidoc_are_served() {
  // The generated OpenAPI 3 document + the RapiDoc browser page (rocket_okapi, built from the
  // `#[openapi]` agent routes). The documented route must still serve its data through the new
  // mount.
  let client = client();
  let response = client.get("/api/openapi.json").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let spec: serde_json::Value = response.into_json().expect("an OpenAPI JSON document");
  assert!(
    spec["openapi"]
      .as_str()
      .is_some_and(|v| v.starts_with("3.")),
    "an OpenAPI 3.x document, got {:?}",
    spec["openapi"]
  );
  for path in [
    "/api/corpora",
    "/api/services",
    "/api/jobs",
    "/api/runs/{corpus}/{service}",
    "/api/runs/{corpus}/{service}/diff",
    "/api/runs/{corpus}/{service}/tasks",
    "/api/reports/{corpus}/{service}/{severity}",
    "/api/config",
    "/healthz",
    "/api/health",
  ] {
    assert!(
      spec["paths"][path]["get"].is_object(),
      "{path} is documented in the OpenAPI spec"
    );
  }
  // Write endpoints are documented too (a representative POST + DELETE).
  assert!(
    spec["paths"]["/api/services"]["post"].is_object(),
    "the register-service write endpoint is documented"
  );
  assert!(
    spec["paths"]["/api/corpora/{name}"]["delete"].is_object(),
    "the delete-corpus write endpoint is documented"
  );
  // The dataset-export POST must stay on the agent surface — it is mounted by hand in
  // `apidoc::mount` (two places), so a dropped registration would silently break agent discovery.
  assert!(
    spec["paths"]["/api/corpora/{corpus}/services/{service}/export-dataset"]["post"].is_object(),
    "the dataset-export write endpoint is documented"
  );
  // The token-gating is described as a security scheme (the Actor guard's OpenApiFromRequest).
  assert!(
    spec["components"]["securitySchemes"]["CortexToken"].is_object(),
    "the X-Cortex-Token security scheme is documented"
  );
  // The `info` carries an agent quickstart (title + how-to-authenticate + entry points) — an
  // agent's first contact with the API, not rocket_okapi's bare default.
  assert_eq!(
    spec["info"]["title"], "CorTeX agent API",
    "the spec is titled as the agent API"
  );
  let overview = spec["info"]["description"].as_str().unwrap_or_default();
  assert!(
    overview.contains("X-Cortex-Token") && overview.contains("?token="),
    "the overview tells an agent how to authenticate"
  );
  assert!(
    overview.contains("/api/status"),
    "the overview points at a where-to-start endpoint"
  );
  // The documented route still serves (it is mounted via the openapi mechanism now).
  assert_eq!(client.get("/api/corpora").dispatch().status(), Status::Ok);
  // The RapiDoc browser page renders.
  let docs = client.get("/api/docs/index.html").dispatch();
  assert_eq!(docs.status(), Status::Ok, "the RapiDoc docs page renders");
}

// Custom harness (Cargo.toml `harness = false`): run the cases then `_exit(0)`, skipping the racy
// libpq/OpenSSL atexit teardown that SIGSEGVs after assertions pass (KNOWN_ISSUES L-1).
fn main() {
  get_api_config_returns_masked_contract();
  healthz_reports_ok_when_db_reachable();
  api_index_lists_the_agent_surface();
  reindex_is_token_gated();
  api_status_is_token_gated_agent_twin();
  put_config_is_token_gated();
  all_api_writes_reject_untokened_requests();
  all_api_deletes_require_matching_confirm();
  analyze_is_token_gated();
  healthz_flags_unreadable_corpus_storage();
  openapi_spec_and_rapidoc_are_served();
  api_write_errors_return_the_json_envelope();
  eprintln!("management_api_test: all cases passed");
  unsafe { libc::_exit(0) }
}
