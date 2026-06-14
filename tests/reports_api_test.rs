// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the reports API: the typed, rollup-backed category and `what` reports — the
//! agent twin of the severity-report / category-report screens.

use cortex::backend::{self, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::models::{Corpus, NewCorpus, NewService, Service};
use cortex::schema::{corpora, log_warnings, services, tasks};
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

#[test]
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
}
