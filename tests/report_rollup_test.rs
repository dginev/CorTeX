// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the `report_summary` materialized-view rollup (Arm 14 #6).
//!
//! Pins the data contract: after `refresh_report_summary`, the rollup's category-grain and
//! `what`-grain reads must return the ground-truth distinct-task and message counts for a known
//! seed — the cheap, fresh replacement for the expensive live `task_report` aggregation.

use cortex::backend;
use cortex::models::{Corpus, NewCorpus, NewService, Service};
use diesel::prelude::*;

use cortex::schema::{corpora, log_warnings, services, tasks};

const CORPUS_NAME: &str = "rollup-test corpus";
const SERVICE_NAME: &str = "rollup_svc";
const WARNING: i32 = -2;

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

#[test]
fn rollup_matches_seeded_ground_truth() {
  let mut backend = backend::testdb();

  // --- Clean slate (logs -> tasks -> corpus -> service) ----------------------------------------
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

  // --- Seed corpus + service -------------------------------------------------------------------
  backend
    .add(&NewCorpus {
      name: CORPUS_NAME.to_string(),
      path: "/tmp/rollup-test".to_string(),
      complex: true,
      description: String::new(),
    })
    .expect("add corpus");
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("find corpus");
  backend
    .add(&NewService {
      name: SERVICE_NAME.to_string(),
      version: 0.1,
      inputformat: "tex".to_string(),
      outputformat: "html".to_string(),
      inputconverter: Some("import".to_string()),
      complex: true,
      description: String::from("rollup test service"),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("find service");

  // --- Seed a known set of warning tasks + messages --------------------------------------------
  //   task A: math/undefined_x, math/undefined_y
  //   task B: math/undefined_x
  //   task C: font/missing
  let a = add_task(&mut backend.connection, "/rollup/a", service.id, corpus.id);
  let b = add_task(&mut backend.connection, "/rollup/b", service.id, corpus.id);
  let c = add_task(&mut backend.connection, "/rollup/c", service.id, corpus.id);
  add_warning(&mut backend.connection, a, "math", "undefined_x");
  add_warning(&mut backend.connection, a, "math", "undefined_y");
  add_warning(&mut backend.connection, b, "math", "undefined_x");
  add_warning(&mut backend.connection, c, "font", "missing");

  backend.refresh_report_summary().expect("refresh rollup");

  // --- Category grain: distinct tasks + total messages per category ----------------------------
  let categories = backend.category_rollup(&corpus, &service, "warning", 100, 0);
  let math = categories
    .iter()
    .find(|r| r.category == "math")
    .expect("math category present");
  let font = categories
    .iter()
    .find(|r| r.category == "font")
    .expect("font category present");

  assert_eq!(math.task_count, 2, "math: distinct tasks A,B"); // NOT 3 — A's two whats are one task
  assert_eq!(math.message_count, 3, "math: 2 (A) + 1 (B) messages");
  assert!(math.what.is_none(), "category-grain row has no `what`");
  assert_eq!(font.task_count, 1, "font: task C");
  assert_eq!(font.message_count, 1, "font: 1 message");
  // Ordered by task_count desc -> math (2) before font (1).
  assert_eq!(categories[0].category, "math");

  // --- `what` grain: drill-down within the math category ---------------------------------------
  let whats = backend.what_rollup(&corpus, &service, "warning", "math", 100, 0);
  let ux = whats
    .iter()
    .find(|r| r.what.as_deref() == Some("undefined_x"))
    .expect("undefined_x present");
  let uy = whats
    .iter()
    .find(|r| r.what.as_deref() == Some("undefined_y"))
    .expect("undefined_y present");
  assert_eq!(ux.task_count, 2, "undefined_x: tasks A,B");
  assert_eq!(ux.message_count, 2);
  assert_eq!(uy.task_count, 1, "undefined_y: task A");
  assert_eq!(uy.message_count, 1);
  assert_eq!(whats.len(), 2, "exactly two `what` classes under math");

  // --- Pagination: categories are ordered by descending task count (math=2, font=1), windowed ---
  let page0 = backend.category_rollup(&corpus, &service, "warning", 1, 0);
  let page1 = backend.category_rollup(&corpus, &service, "warning", 1, 1);
  let page2 = backend.category_rollup(&corpus, &service, "warning", 1, 2);
  assert_eq!(page0.len(), 1);
  assert_eq!(
    page0[0].category, "math",
    "page 0 (offset 0) = the busiest category"
  );
  assert_eq!(page1.len(), 1);
  assert_eq!(
    page1[0].category, "font",
    "page 1 (offset 1) = the next category"
  );
  assert!(page2.is_empty(), "page 2 (offset 2) is past the end");
}
