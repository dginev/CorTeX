// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the reports API: the typed, rollup-backed category and `what` reports — the
//! agent twin of the severity-report / category-report screens.

use cortex::backend::{self, test_db_address, DOCUMENT_MESSAGE_CAP};
use cortex::frontend::server::mount_api_with;
use cortex::models::{Corpus, NewCorpus, NewService, Service};
use cortex::schema::{corpora, log_errors, log_infos, log_warnings, services, tasks};
use diesel::prelude::*;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;
use serde_json::Value;

const CORPUS_NAME: &str = "reports-api-corpus";
const SERVICE_NAME: &str = "reports_api_svc";
const WARNING: i32 = -2;

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_reports_api_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

fn add_task(conn: &mut PgConnection, entry: &str, service_id: i32, corpus_id: i32) -> i64 {
  diesel::insert_into(tasks::table)
    .values((
      tasks::entry.eq(entry),
      tasks::service_id.eq(service_id),
      tasks::corpus_id.eq(corpus_id),
      tasks::status.eq(WARNING),
    ))
    .returning(tasks::id)
    .get_result(conn)
    .expect("insert task")
}

fn add_warning(conn: &mut PgConnection, task_id: i64, category: &str, what: &str) {
  diesel::insert_into(log_warnings::table)
    .values((
      log_warnings::task_id.eq(task_id),
      log_warnings::category.eq(category),
      log_warnings::what.eq(what),
      log_warnings::details.eq(""),
    ))
    .execute(conn)
    .expect("insert log_warning");
}

/// Clean slate, seed warnings (math: 2 tasks / 3 msgs, font: 1 / 1), refresh the rollup.
fn seed() {
  let mut backend = backend::testdb();
  if let Ok(existing) = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection) {
    let ids: Vec<i64> = tasks::table
      .filter(tasks::corpus_id.eq(existing.id))
      .select(tasks::id)
      .load(&mut backend.connection)
      .unwrap_or_default();
    diesel::delete(log_warnings::table.filter(log_warnings::task_id.eq_any(&ids)))
      .execute(&mut backend.connection)
      .ok();
    diesel::delete(tasks::table.filter(tasks::corpus_id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
    diesel::delete(corpora::table.filter(corpora::id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
  }
  diesel::delete(services::table.filter(services::name.eq(SERVICE_NAME)))
    .execute(&mut backend.connection)
    .ok();

  backend
    .add(&NewCorpus {
      name: CORPUS_NAME.to_string(),
      path: "/tmp/reports-api".to_string(),
      complex: true,
      description: String::new(),
    })
    .expect("add corpus");
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("corpus");
  backend
    .add(&NewService {
      name: SERVICE_NAME.to_string(),
      version: 0.1,
      inputformat: "tex".to_string(),
      outputformat: "html".to_string(),
      inputconverter: Some("import".to_string()),
      complex: true,
      description: String::from("reports api service"),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");

  let a = add_task(&mut backend.connection, "/r/a", service.id, corpus.id);
  let b = add_task(&mut backend.connection, "/r/b", service.id, corpus.id);
  let c = add_task(&mut backend.connection, "/r/c", service.id, corpus.id);
  add_warning(&mut backend.connection, a, "math", "undefined_x");
  add_warning(&mut backend.connection, a, "math", "undefined_y");
  add_warning(&mut backend.connection, b, "math", "undefined_x");
  add_warning(&mut backend.connection, c, "font", "missing");
  backend.refresh_report_summary().expect("refresh rollup");
}

fn find<'a>(rows: &'a [Value], name: &str) -> &'a Value {
  rows
    .iter()
    .find(|row| row["name"] == name)
    .unwrap_or_else(|| panic!("row {name:?} present"))
}

fn category_and_what_reports_match_seed() {
  seed();
  let client = client();

  // --- Category report: math (2 tasks / 3 msgs) + font (1 / 1), severity total 3 / 4 ------------
  let response = client
    .get(format!("/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/warning"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let report: Value = response.into_json().expect("category report json");
  assert_eq!(report["severity"], "warning");
  assert_eq!(
    report["total_tasks"], 3,
    "distinct logged warning tasks A,B,C"
  );
  assert_eq!(report["total_messages"], 4, "2 + 1 + 1 warning messages");
  let categories = report["categories"].as_array().expect("categories array");
  assert_eq!(find(categories, "math")["tasks"], 2);
  assert_eq!(find(categories, "math")["messages"], 3);
  assert_eq!(find(categories, "font")["tasks"], 1);

  // --- What drill-down within math: undefined_x (2 / 2) + undefined_y (1 / 1), category total 2/3
  let response = client
    .get(format!(
      "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/warning/math"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let report: Value = response.into_json().expect("what report json");
  assert_eq!(report["category"], "math");
  assert_eq!(report["total_tasks"], 2, "distinct tasks A,B in math");
  assert_eq!(report["total_messages"], 3);
  let whats = report["whats"].as_array().expect("whats array");
  assert_eq!(find(whats, "undefined_x")["tasks"], 2);
  assert_eq!(find(whats, "undefined_x")["messages"], 2);
  assert_eq!(find(whats, "undefined_y")["tasks"], 1);

  // --- Entry list: the deepest rung — enumerate the affected documents (the agent's macro→micro
  // bridge). undefined_x is on tasks A and B, so the list has exactly those 2 documents, each with
  // a real task id. Was human-only (no agent twin); now `GET …/<severity>/<category>/<what>`.
  let response = client
    .get(format!(
      "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/warning/math/undefined_x"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let list: Value = response.into_json().expect("entry list json");
  assert_eq!(list["what"], "undefined_x");
  let entries = list["entries"].as_array().expect("entries array");
  assert_eq!(
    entries.len(),
    2,
    "exactly the two documents (A, B) carrying warning/math/undefined_x"
  );
  for entry in entries {
    assert!(
      entry["name"].as_str().is_some_and(|name| !name.is_empty()),
      "each entry has a document name"
    );
    assert!(
      entry["task_id"].as_i64().is_some_and(|id| id > 0),
      "each entry has a real task id (drill into it via the document endpoint)"
    );
  }
  // An unknown severity at this depth is still a 400, not a silent empty list.
  assert_eq!(
    client
      .get(format!(
        "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/bogus/math/undefined_x"
      ))
      .dispatch()
      .status(),
    Status::BadRequest,
    "unknown severity on the entry list -> 400"
  );
  // A pathologically deep `offset` is rejected (bounds the OFFSET scan-and-discard cost; P-4).
  assert_eq!(
    client
      .get(format!(
        "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/warning/math/undefined_x?offset=200001"
      ))
      .dispatch()
      .status(),
    Status::BadRequest,
    "offset past ENTRY_LIST_MAX_OFFSET -> 400 (no multi-second deep scan)"
  );

  // --- The human live `?all=true` report is gated by the P-2 bounded-concurrency limiter but still
  //     serves the page (this seed is tiny, so the live aggregation is fast). Two sequential hits
  //     exercise the permit acquire-before-pool path + RAII release — a leaked permit would still
  //     pass the first but the limiter is sized > 1 so this mainly pins "the gate doesn't break
  // it".
  for _ in 0..2 {
    let response = client
      .get(format!(
        "/corpus/{CORPUS_NAME}/{SERVICE_NAME}/warning?all=true"
      ))
      .dispatch();
    assert_eq!(
      response.status(),
      Status::Ok,
      "the limiter-gated ?all=true live report still serves (permit acquired + released)"
    );
  }

  // --- Guards ----------------------------------------------------------------------------------
  let response = client
    .get(format!("/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/bogus"))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "unknown severity -> 400"
  );

  let response = client
    .get("/api/reports/no-such-corpus-xyz/no_svc/warning")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound, "unknown corpus -> 404");

  // --- Rerun is token-gated: denied by default (the critical security property) ----------------
  let response = client
    .post(format!(
      "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/rerun?severity=warning"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "rerun without a token is 401 (no unauthenticated result wipes)"
  );
  let response = client
    .post(format!(
      "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/rerun?severity=warning&token=bogus"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "rerun with an unknown token is 401"
  );

  // --- HTML report screens (relocated to the library + pooled): top + severity drill-down --------
  let response = client
    .get(format!("/corpus/{CORPUS_NAME}/{SERVICE_NAME}"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok, "top report renders");
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains(CORPUS_NAME),
    "top report names the corpus it reports on"
  );

  let response = client
    .get(format!("/corpus/{CORPUS_NAME}/{SERVICE_NAME}/warning"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok, "severity report renders");
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("math"),
    "severity report lists the seeded `math` category server-side"
  );

  // Unknown corpus -> 404 (the relocated serve_report now returns a Status, not a panic).
  let response = client.get("/corpus/no-such-xyz/no_svc/warning").dispatch();
  assert_eq!(response.status(), Status::NotFound, "unknown corpus -> 404");

  // --- Forced report refresh: token-gated, returns an async background job handle (the
  // multi-minute rollup rebuild must not block the request).
  // ------------------------------------------------
  let response = client.post("/api/reports/refresh").dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "force-refresh without a token is 401"
  );
  let response = client.post("/api/reports/refresh?token=token1").dispatch();
  assert_eq!(
    response.status(),
    Status::Accepted,
    "force-refresh with a valid token is 202 (async job)"
  );
  let ack: Value = response.into_json().expect("refresh ack json");
  assert!(
    ack["job"].as_str().is_some_and(|j| !j.is_empty()),
    "force-refresh returns a background job handle to poll"
  );
  assert_eq!(
    ack["actor"], "username1",
    "the refresh job is attributed to the token's owner"
  );
  assert!(
    ack["poll"]
      .as_str()
      .is_some_and(|p| p.starts_with("/api/jobs/")),
    "the ack carries a poll URL for the job's status/health"
  );

  // --- Error catchers are content-negotiated: agents (an /api path or Accept: json) get a JSON
  // `{error, status}`; humans get the themed HTML error page (the error-path symmetry contract).
  // ---
  let response = client
    .get("/api/reports/no-such-corpus-xyz/no_svc/warning")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
  assert_eq!(
    response.content_type(),
    Some(ContentType::JSON),
    "an /api error renders as JSON, not Rocket's default HTML page"
  );
  let err: Value = response.into_json().expect("json error body");
  assert_eq!(err["status"], 404);
  assert!(err["error"].is_string(), "the JSON error carries a message");

  let response = client.get("/corpus/no-such-xyz/no_svc/warning").dispatch();
  assert_eq!(response.status(), Status::NotFound);
  assert_eq!(
    response.content_type(),
    Some(ContentType::HTML),
    "a human error renders as the themed HTML page"
  );
}

/// The service-overview hub (`GET /api/reports/<c>/<svc>`): the macro status breakdown an agent
/// reads before drilling into a severity.
fn service_overview_reports_the_status_breakdown() {
  seed(); // 3 warning tasks (a, b, c), no invalids
  let client = client();

  let response = client
    .get(format!("/api/reports/{CORPUS_NAME}/{SERVICE_NAME}"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok, "overview renders");
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let overview: Value = response.into_json().expect("overview json");
  assert_eq!(overview["total"], 3, "3 valid tasks (no invalids)");
  let statuses = overview["statuses"].as_array().expect("statuses array");
  let bucket = |key: &str| {
    statuses
      .iter()
      .find(|s| s["status"] == key)
      .unwrap_or_else(|| panic!("status bucket {key:?} present"))
  };
  assert_eq!(
    bucket("warning")["tasks"],
    3,
    "all 3 seeded tasks are warnings"
  );
  assert_eq!(
    bucket("warning")["percent"].as_f64(),
    Some(100.0),
    "warnings are 100% of the valid total"
  );
  assert_eq!(bucket("no_problem")["tasks"], 0);

  // Unknown corpus -> 404.
  let response = client
    .get("/api/reports/no-such-corpus-xyz/no_svc")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound, "unknown corpus -> 404");
}

/// The per-article forensic endpoint (`GET /api/corpus/<c>/<svc>/document/<name>`): a document's
/// status plus every worker-log message behind it — "what are the errors of this article?".
fn document_forensics_reports_status_and_messages() {
  seed(); // ensures the corpus + service exist
  let mut backend = backend::testdb();
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("corpus");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");

  // A document whose entry ends in `<name>.zip`, so `Task::find_by_name` resolves it. Status =
  // Error (-3); seed one warning and one error message as its forensic evidence.
  let entry = "/r/1808/0801.1234.zip";
  let task_id: i64 = diesel::insert_into(tasks::table)
    .values((
      tasks::entry.eq(entry),
      tasks::service_id.eq(service.id),
      tasks::corpus_id.eq(corpus.id),
      tasks::status.eq(-3i32),
    ))
    .returning(tasks::id)
    .get_result(&mut backend.connection)
    .expect("insert document task");
  add_warning(&mut backend.connection, task_id, "math", "undefined_macro");
  diesel::insert_into(log_errors::table)
    .values((
      log_errors::task_id.eq(task_id),
      log_errors::category.eq("latex"),
      log_errors::what.eq("undefined_control_sequence"),
      log_errors::details.eq("\\foo at line 3"),
    ))
    .execute(&mut backend.connection)
    .expect("insert log_error");
  // An info message: it must be tucked into the collapsed `<details>`, not the lead table.
  diesel::insert_into(log_infos::table)
    .values((
      log_infos::task_id.eq(task_id),
      log_infos::category.eq("io"),
      log_infos::what.eq("loaded_file"),
      log_infos::details.eq("TeX.pool.ltxml"),
    ))
    .execute(&mut backend.connection)
    .expect("insert log_info");

  let client = client();
  let response = client
    .get(format!(
      "/api/corpus/{CORPUS_NAME}/{SERVICE_NAME}/document/0801.1234"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok, "forensic report renders");
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let doc: Value = response.into_json().expect("document json");
  assert_eq!(doc["name"], "0801.1234");
  assert_eq!(doc["status"], "error", "the tasks-row status, keyed");
  assert_eq!(doc["status_code"], -3);
  assert!(
    doc["result_url"]
      .as_str()
      .is_some_and(|u| u == format!("/entry/{SERVICE_NAME}/{task_id}")),
    "carries a result-download URL keyed by the task id"
  );
  let messages = doc["messages"].as_array().expect("messages array");
  assert_eq!(messages.len(), 3, "the seeded warning + error + info");
  assert!(
    messages.iter().any(|m| m["severity"] == "warning"
      && m["category"] == "math"
      && m["what"] == "undefined_macro"),
    "the warning message is surfaced with its category/what"
  );
  assert!(
    messages.iter().any(|m| m["severity"] == "error"
      && m["what"] == "undefined_control_sequence"
      && m["details"] == "\\foo at line 3"),
    "the error message carries its forensic details"
  );

  // Unknown document -> 404.
  let response = client
    .get(format!(
      "/api/corpus/{CORPUS_NAME}/{SERVICE_NAME}/document/no-such-paper-9999"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::NotFound,
    "unknown document -> 404"
  );

  // The human forensic screen (HTML twin) renders the same status + messages from the shared DTO.
  let response = client
    .get(format!("/document/{CORPUS_NAME}/{SERVICE_NAME}/0801.1234"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok, "forensic screen renders");
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("undefined_control_sequence") && body.contains("undefined_macro"),
    "the forensic screen leads with the actionable error + warning"
  );
  // The info message is collapsed into a <details> disclosure (not in the lead table) so the noise
  // never buries the warnings/errors.
  assert!(
    body.contains("<details") && body.contains("info message") && body.contains("loaded_file"),
    "info messages are tucked into a collapsed <details>, out of the lead table"
  );

  // --- Document serving (migrated bin → library): graceful degradation on the hostile `/data`
  // filesystem. The seeded task's result archive does not exist on disk, so a download must yield a
  // clean 404 (never a panic / 500); an unknown task id likewise 404s; and the preview shell still
  // renders (its converted-document asset is fetched client-side from the download URL).
  let response = client
    .post(format!("/entry/{SERVICE_NAME}/{task_id}"))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::NotFound,
    "a missing result archive on /data yields a clean 404, not a panic/500"
  );
  let response = client
    .post(format!("/entry/{SERVICE_NAME}/999999999"))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::NotFound,
    "an unknown task id is a clean 404"
  );
  // Security: the `<service>` segment is interpolated into the result-archive filesystem path, so a
  // path-traversal payload must be rejected BEFORE any file open — even with a real task id. The
  // guard's `invalid service` body distinguishes this from an incidental file-not-found 404.
  for bad in ["evil..svc", "..%2f..%2fetc%2fpasswd", "a%2fb"] {
    let response = client.post(format!("/entry/{bad}/{task_id}")).dispatch();
    assert_eq!(
      response.status(),
      Status::NotFound,
      "a traversal service segment {bad:?} is 404, never a file read"
    );
  }
  // The literal `..` case provably hits the guard (real task id, so not a task-not-found 404).
  let guarded = client
    .post(format!("/entry/evil..svc/{task_id}"))
    .dispatch()
    .into_string()
    .unwrap_or_default();
  assert!(
    guarded.contains("invalid service"),
    "a `..` service segment is rejected by the traversal guard, got {guarded:?}"
  );
  let response = client
    .get(format!("/preview/{CORPUS_NAME}/{SERVICE_NAME}/0801.1234"))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "the preview shell renders even without the archive (asset fetched client-side)"
  );

  // The no-JS lookup shortcut: GET /document/<c>/<s>?name=<id> redirects to the canonical path URL,
  // so the service-overview "look up an article" form reaches the forensic screen with scripting
  // off.
  let response = client
    .get(format!(
      "/document/{CORPUS_NAME}/{SERVICE_NAME}?name=0801.1234"
    ))
    .dispatch();
  assert!(
    (300..400).contains(&response.status().code),
    "document lookup redirects (3xx), got {}",
    response.status().code
  );
  assert_eq!(
    response.headers().get_one("Location"),
    Some(format!("/document/{CORPUS_NAME}/{SERVICE_NAME}/0801.1234").as_str()),
    "redirect points at the canonical path URL"
  );
  // A blank id is a 400 — no silent redirect to an empty document path.
  let response = client
    .get(format!("/document/{CORPUS_NAME}/{SERVICE_NAME}?name="))
    .dispatch();
  assert_eq!(response.status(), Status::BadRequest, "blank lookup -> 400");

  // Clean up the extra task + its logs.
  diesel::delete(log_warnings::table.filter(log_warnings::task_id.eq(task_id)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(log_errors::table.filter(log_errors::task_id.eq(task_id)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(log_infos::table.filter(log_infos::task_id.eq(task_id)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(tasks::table.filter(tasks::id.eq(task_id)))
    .execute(&mut backend.connection)
    .ok();
}

/// Robustness: a document with more messages than the per-severity cap is **sampled, not dumped** —
/// the response stays bounded while the true counts remain exact. A production arXiv task was found
/// with 1.6M warnings on one document; loading them all hung the request (KNOWN_ISSUES R-7). The
/// forensic builder caps the loaded rows at `DOCUMENT_MESSAGE_CAP` per severity and reports the
/// real totals + a `messages_truncated` flag.
fn document_forensics_caps_pathological_message_volume() {
  seed();
  let mut backend = backend::testdb();
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("corpus");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");

  let entry = "/r/cap/9999.0001.zip";
  let task_id: i64 = diesel::insert_into(tasks::table)
    .values((
      tasks::entry.eq(entry),
      tasks::service_id.eq(service.id),
      tasks::corpus_id.eq(corpus.id),
      tasks::status.eq(-2i32),
    ))
    .returning(tasks::id)
    .get_result(&mut backend.connection)
    .expect("insert flood task");

  // One over the cap, in a single batched insert (well under the bind-parameter limit).
  let overflow = DOCUMENT_MESSAGE_CAP + 5;
  let rows: Vec<_> = (0..overflow)
    .map(|_| {
      (
        log_warnings::task_id.eq(task_id),
        log_warnings::category.eq("flood"),
        log_warnings::what.eq("noise"),
        log_warnings::details.eq(""),
      )
    })
    .collect();
  diesel::insert_into(log_warnings::table)
    .values(rows)
    .execute(&mut backend.connection)
    .expect("batch insert overflow warnings");

  let client = client();
  let response = client
    .get(format!(
      "/api/corpus/{CORPUS_NAME}/{SERVICE_NAME}/document/9999.0001"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "flood document still renders"
  );
  let report: Value = response.into_json().expect("document json");

  // True totals are exact (not capped)...
  assert_eq!(
    report["message_counts"]["warning"], overflow,
    "true warning total is reported exactly"
  );
  assert_eq!(report["message_counts"]["total"], overflow);
  assert_eq!(
    report["messages_truncated"], true,
    "cap flagged transparently"
  );
  // ...but the loaded sample is bounded by the cap (the whole point: no unbounded load).
  let shown = report["messages"].as_array().expect("messages array").len() as i64;
  assert_eq!(
    shown, DOCUMENT_MESSAGE_CAP,
    "the message list is capped at DOCUMENT_MESSAGE_CAP, not the full {overflow}"
  );

  diesel::delete(log_warnings::table.filter(log_warnings::task_id.eq(task_id)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(tasks::table.filter(tasks::id.eq(task_id)))
    .execute(&mut backend.connection)
    .ok();
}

// The **human** rerun route (cookie-authed, migrated from bin → library): denied without a session
// (no anonymous reprocessing), accepted for a signed-in admin — the human twin of the token-gated
// agent rerun. (The agent path's 401 is covered above; this pins the cookie-auth half.)
fn human_rerun_requires_session() {
  seed();
  let client = client();

  // No session cookie → 401 (the modal's XHR carries the cookie; anonymous is rejected).
  let response = client
    .post(format!("/rerun/{CORPUS_NAME}/{SERVICE_NAME}"))
    .header(ContentType::JSON)
    .body(r#"{"description":"contract test rerun"}"#)
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "a human rerun without a signed-in session is 401"
  );

  // Sign in (AdminSession cookie), then the scoped rerun is accepted (marks the warning slice
  // TODO).
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
  let response = client
    .post(format!("/rerun/{CORPUS_NAME}/{SERVICE_NAME}/warning"))
    .header(ContentType::JSON)
    .body(r#"{"description":"contract test rerun"}"#)
    .dispatch();
  let status = response.status();
  assert!(
    status == Status::Ok || status == Status::Accepted,
    "a signed-in admin rerun is accepted, got {status}"
  );

  // --- R-9: rerun severity validation is now correct + consistent across surfaces ---------------
  // Agent: `no_problem` (no category) IS a valid rerun scope (re-convert already-clean tasks) —
  // accepted, not 400 (the old report validator wrongly rejected it). The seed has no no_problem
  // tasks, so it is a harmless no-op.
  let np = client
    .post(format!(
      "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/rerun?severity=no_problem&token=token1"
    ))
    .dispatch();
  assert!(
    np.status() == Status::Ok || np.status() == Status::Accepted,
    "no_problem (no category) is a valid rerun scope, got {}",
    np.status()
  );
  // Agent: `info` is not a task status — rejected (was wrongly accepted, then mark_rerun silently
  // mis-scoped it to no_problem).
  let info = client
    .post(format!(
      "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/rerun?severity=info&token=token1"
    ))
    .dispatch();
  assert_eq!(
    info.status(),
    Status::BadRequest,
    "info is not a valid no-category rerun severity"
  );
  // Agent: a typo'd severity is rejected, not silently mis-scoped.
  let bad = client
    .post(format!(
      "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/rerun?severity=bogus&token=token1"
    ))
    .dispatch();
  assert_eq!(
    bad.status(),
    Status::BadRequest,
    "a bogus rerun severity is 400"
  );
  // Human: the same guard now applies (session established above) — a typo is rejected, not a
  // silent no_problem rerun.
  let human_bad = client
    .post(format!("/rerun/{CORPUS_NAME}/{SERVICE_NAME}/bogus"))
    .header(ContentType::JSON)
    .body(r#"{"description":"r9 probe"}"#)
    .dispatch();
  assert_eq!(
    human_bad.status(),
    Status::BadRequest,
    "the human rerun path validates severity too (R-9)"
  );
}

/// Run control: `POST /api/reports/<c>/<s>/{pause,resume}` blocks every in-progress task
/// (`status >= 0`) and returns every Blocked task (`status < -5`) to TODO — the agent twin of the
/// report screen's Pause/Resume buttons. Completed tasks (the seed's `-2` warnings) are untouched;
/// token-gated; unknown corpus 404s.
fn pause_resume_blocks_and_restores_in_progress_tasks() {
  seed(); // corpus + service + 3 completed warning tasks (status -2)
  let mut backend = backend::testdb();
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).unwrap();
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).unwrap();
  let insert_status = |conn: &mut PgConnection, entry: &str, st: i32| -> i64 {
    diesel::insert_into(tasks::table)
      .values((
        tasks::entry.eq(entry),
        tasks::service_id.eq(service.id),
        tasks::corpus_id.eq(corpus.id),
        tasks::status.eq(st),
      ))
      .returning(tasks::id)
      .get_result(conn)
      .expect("insert in-progress task")
  };
  // 2 TODO (0) + 1 leased/Queued (5) — the in-progress set pause must catch.
  let todo_a = insert_status(&mut backend.connection, "pr/todo_a", 0);
  let todo_b = insert_status(&mut backend.connection, "pr/todo_b", 0);
  let queued = insert_status(&mut backend.connection, "pr/queued", 5);
  let status_of = |conn: &mut PgConnection, id: i64| -> i32 {
    tasks::table
      .find(id)
      .select(tasks::status)
      .first(conn)
      .unwrap()
  };

  let client = client();
  // Gated: no token → 401; unknown corpus → 404.
  assert_eq!(
    client
      .post(format!("/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/pause"))
      .dispatch()
      .status(),
    Status::Unauthorized
  );
  assert_eq!(
    client
      .post("/api/reports/no_such/no_such/pause?token=token1")
      .dispatch()
      .status(),
    Status::NotFound
  );

  // Pause: the 3 in-progress tasks become Blocked; the completed warnings are untouched.
  let response = client
    .post(format!(
      "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/pause?token=token1"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let body: Value = response.into_json().unwrap();
  assert_eq!(body["action"], "pause");
  assert_eq!(
    body["affected"], 3,
    "the 2 TODO + 1 Queued task were blocked"
  );
  assert!(
    status_of(&mut backend.connection, todo_a) < -5,
    "TODO → Blocked"
  );
  assert!(
    status_of(&mut backend.connection, queued) < -5,
    "Queued → Blocked"
  );
  let still_warning: i64 = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .filter(tasks::status.eq(WARNING))
    .count()
    .get_result(&mut backend.connection)
    .unwrap();
  assert!(still_warning >= 1, "completed warning tasks are not paused");

  // Resume: every Blocked task returns to TODO (0).
  let response = client
    .post(format!(
      "/api/reports/{CORPUS_NAME}/{SERVICE_NAME}/resume?token=token1"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let body: Value = response.into_json().unwrap();
  assert_eq!(body["action"], "resume");
  assert_eq!(body["affected"], 3);
  assert_eq!(
    status_of(&mut backend.connection, todo_a),
    0,
    "Blocked → TODO"
  );
  assert_eq!(status_of(&mut backend.connection, todo_b), 0);
  assert_eq!(status_of(&mut backend.connection, queued), 0);
}

// Custom harness (`harness = false`): own `main`, so we end with `libc::_exit(0)` while the Client
// is still alive — skipping the racy libpq/OpenSSL `atexit` teardown that SIGSEGVs a
// default-harness exit (KNOWN_ISSUES L-1). A panic still aborts non-zero, so a real assertion
// failure still fails CI.
fn main() {
  category_and_what_reports_match_seed();
  service_overview_reports_the_status_breakdown();
  document_forensics_reports_status_and_messages();
  document_forensics_caps_pathological_message_volume();
  human_rerun_requires_session();
  pause_resume_blocks_and_restores_in_progress_tasks();
  eprintln!("reports_api_test: all cases passed");
  unsafe { libc::_exit(0) }
}
