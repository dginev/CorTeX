// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Backend effect test for `mark_rerun`: marking a severity for rerun resets those tasks to TODO
//! and clears their log messages (the mutation behind the token-gated rerun action).

use cortex::backend::{self, RerunOptions};
use cortex::helpers::TaskStatus;
use cortex::models::{Corpus, NewCorpus, NewService, Service};
use cortex::schema::{corpora, log_warnings, services, tasks};
use diesel::prelude::*;

const CORPUS_NAME: &str = "rerun-test-corpus";
const SERVICE_NAME: &str = "rerun_test_svc";

#[test]
fn rerun_severity_resets_tasks_to_todo_and_clears_logs() {
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
      path: "/tmp/rerun".to_string(),
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
      description: String::from("rerun test service"),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");

  // A completed warning task plus its log message.
  let task_id: i64 = diesel::insert_into(tasks::table)
    .values((
      tasks::entry.eq("/rerun/a"),
      tasks::service_id.eq(service.id),
      tasks::corpus_id.eq(corpus.id),
      tasks::status.eq(TaskStatus::Warning.raw()),
    ))
    .returning(tasks::id)
    .get_result(&mut backend.connection)
    .expect("insert task");
  diesel::insert_into(log_warnings::table)
    .values((
      log_warnings::task_id.eq(task_id),
      log_warnings::category.eq("math"),
      log_warnings::what.eq("undefined_x"),
      log_warnings::details.eq(""),
    ))
    .execute(&mut backend.connection)
    .expect("insert log");

  backend
    .mark_rerun(RerunOptions {
      corpus: &corpus,
      service: &service,
      severity_opt: Some("warning".to_string()),
      category_opt: None,
      what_opt: None,
      owner_opt: Some("tester".to_string()),
      description_opt: Some("test rerun".to_string()),
    })
    .expect("mark_rerun");

  // The task is back to TODO and its warning log is gone.
  let status: i32 = tasks::table
    .filter(tasks::id.eq(task_id))
    .select(tasks::status)
    .first(&mut backend.connection)
    .expect("task status");
  assert_eq!(status, TaskStatus::TODO.raw(), "warning task reset to TODO");
  let logs: i64 = log_warnings::table
    .filter(log_warnings::task_id.eq(task_id))
    .count()
    .get_result(&mut backend.connection)
    .unwrap();
  assert_eq!(logs, 0, "warning logs deleted on rerun");

  diesel::delete(tasks::table.filter(tasks::corpus_id.eq(corpus.id)))
    .execute(&mut backend.connection)
    .ok();
}
