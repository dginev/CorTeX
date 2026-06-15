// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
use cortex::backend;
use cortex::backend::RerunOptions;
use cortex::helpers::{rand_in_range, random_mark, NewTaskMessage, TaskReport, TaskStatus};
use cortex::models::{Corpus, NewLogInfo, NewTask, Service, Task};
use cortex::schema::log_infos::dsl::task_id;
use cortex::schema::tasks::dsl::{service_id, status};
use cortex::schema::{log_infos, tasks};
use diesel::prelude::*;

#[test]
fn task_table_crud() {
  let mut backend = backend::testdb();
  let mock_service_id = random_mark();
  let mock_corpus_id = random_mark();
  let mock_service = Service {
    id: mock_service_id,
    name: String::from("mock_service"),
    complex: false,
    inputconverter: None,
    inputformat: String::from("tex"),
    outputformat: String::from("tex"),
    description: String::from("mock"),
    version: 0.1,
  };
  let mock_task = NewTask {
    entry: String::from("mock_task"),
    service_id: mock_service.id,
    corpus_id: mock_corpus_id,
    status: TaskStatus::TODO.raw(),
  };
  // Delete any mock tasks
  let cleanup = backend.delete_by(&mock_task, "entry");
  assert_eq!(cleanup, Ok(0));

  // Add a mock task
  let count_added = backend.add(&mock_task);
  assert_eq!(count_added, Ok(1)); // new task!

  // We should be able to fetch it back
  let fetched_tasks_result = backend.fetch_tasks(&mock_service, 2);
  assert!(fetched_tasks_result.is_ok());
  let fetched_tasks = fetched_tasks_result.unwrap();
  assert_eq!(fetched_tasks.len(), 1);
  let fetched_task = fetched_tasks.first().unwrap();
  assert!(fetched_task.id > 0);
  assert_eq!(fetched_task.entry, mock_task.entry);

  // Delete again and verify deletion works
  let cleanup = backend.delete_by(&mock_task, "entry");
  assert_eq!(cleanup, Ok(1));
}

#[test]
fn task_lifecycle_test() {
  let mut backend = backend::testdb();
  // Add 100 tasks, out of which we will mark 17
  let mock_service_id = random_mark();
  let mock_corpus_id = random_mark();
  let mock_service = Service {
    id: mock_service_id,
    name: String::from("mark_tasks"),
    complex: false,
    inputconverter: None,
    description: String::from("mock"),
    inputformat: String::from("tex"),
    outputformat: String::from("tex"),
    version: 0.1,
  };
  let mock_task = NewTask {
    entry: String::from("mark_task"),
    service_id: mock_service.id,
    corpus_id: mock_corpus_id,
    status: TaskStatus::TODO.raw(),
  };

  assert!(dbg!(backend.delete_by(&mock_task, "service_id")).is_ok());

  // insert 100 tasks
  for index in 1..101 {
    let indexed_task = NewTask {
      entry: format!("{}{}", mock_task.entry, index),
      ..mock_task
    };
    assert!(backend.add(&indexed_task).is_ok());
  }
  // fetch 17 tasks for work
  let fetched_tasks_result = backend.fetch_tasks(&mock_service, 17);
  assert!(fetched_tasks_result.is_ok());
  let fetched_tasks = fetched_tasks_result.unwrap();
  assert_eq!(fetched_tasks.len(), 17);
  // The entire fetched batch shares the random mark
  let random_mark = fetched_tasks[0].status;
  assert!(fetched_tasks.iter().all(|task| task.status == random_mark));
  // Check that querying by the mark also gets us these 17 tasks,
  // i.e. they are saved in the DB
  use cortex::schema::tasks;
  use cortex::schema::tasks::dsl::status;
  use diesel::prelude::*;
  let marked_in_db = tasks::table
    .filter(status.eq(random_mark))
    .count()
    .get_result(&mut backend.connection);
  assert_eq!(marked_in_db, Ok(17));

  let cleared_limbo_tasks = backend.clear_limbo_tasks();
  assert_eq!(cleared_limbo_tasks, Ok(17));
  let marked_in_db_2 = tasks::table
    .filter(status.eq(random_mark))
    .count()
    .get_result(&mut backend.connection);
  assert_eq!(marked_in_db_2, Ok(0));

  let post_cleanup = backend.delete_by(&mock_task, "service_id");
  assert_eq!(post_cleanup, Ok(100));
}

#[test]
fn batch_ops_test() {
  let mut backend = backend::testdb();
  let mock_service = Service {
    id: random_mark(),
    name: String::from("batch_ops_test"),
    complex: false,
    inputconverter: None,
    description: String::from("mock"),
    inputformat: String::from("tex"),
    outputformat: String::from("tex"),
    version: 0.1,
  };

  let mock_corpus_id = random_mark();
  let mock_corpus = Corpus {
    id: mock_corpus_id,
    name: String::from("batch_ops_test_corpus"),
    path: String::new(),
    complex: false,
    description: String::new(),
    parent_corpus_id: None,
    selection: None,
  };

  let mock_task_count = rand_in_range(10, 100) as usize;
  let mock_new_task = NewTask {
    entry: String::from("mock_task"),
    service_id: mock_service.id,
    corpus_id: mock_corpus_id,
    status: TaskStatus::TODO.raw(),
  };
  // mark_imported on `mock_task_count` tasks
  let names: Vec<String> = (1..mock_task_count + 1)
    .map(|index| format!("{}{}", mock_new_task.entry, index))
    .collect();
  let new_tasks: Vec<NewTask> = (0..mock_task_count)
    .map(|index| NewTask {
      entry: names[index].clone(),
      ..mock_new_task
    })
    .collect();
  let imported_count = backend.mark_imported(&new_tasks);
  assert_eq!(imported_count, Ok(mock_task_count));
  let todo_tasks_result: Result<Vec<Task>, _> = tasks::table
    .filter(service_id.eq(mock_service.id))
    .filter(status.eq(TaskStatus::TODO.raw()))
    .get_results(&mut backend.connection);
  assert!(todo_tasks_result.is_ok());
  let todo_tasks = todo_tasks_result.unwrap();
  // We now have `mock_task_count` registered tasks in the DB
  assert_eq!(todo_tasks.len(), mock_task_count);

  // mark_done -- time to mark them as "done", with some trivial ok reports,
  //              with a single info message
  let task_reports: Vec<TaskReport> = todo_tasks
    .into_iter()
    .map(|task| TaskReport {
      status: TaskStatus::NoProblem,
      messages: vec![NewTaskMessage::Info(NewLogInfo {
        task_id: task.id,
        category: String::from("trivial"),
        what: String::from("mock"),
        details: String::new(),
      })],
      task,
    })
    .collect();

  let mark_done_result = backend.mark_done(&task_reports);
  assert!(mark_done_result.is_ok());
  let done_tasks_result: Result<Vec<Task>, _> = tasks::table
    .filter(service_id.eq(mock_service.id))
    .filter(status.eq(TaskStatus::NoProblem.raw()))
    .get_results(&mut backend.connection);
  // Are all tasks marked as NoProblem after?
  assert!(done_tasks_result.is_ok());
  let done_tasks = done_tasks_result.unwrap();
  assert_eq!(done_tasks.len(), mock_task_count);
  let done_task_ids: Vec<i64> = done_tasks.into_iter().map(|task| task.id).collect();
  // Does each done task have a LogInfo message present?
  let done_logs_result: Result<i64, _> = log_infos::table
    .filter(task_id.eq_any(&done_task_ids))
    .count()
    .get_result(&mut backend.connection);
  assert_eq!(done_logs_result, Ok(mock_task_count as i64));

  // Re-finalize the SAME tasks with NO messages: mark_done must batch-delete the prior logs (not
  // leave them stale), even when the new report carries no messages (covers the D-8 batched
  // delete).
  let refinalize_targets: Vec<Task> = tasks::table
    .filter(tasks::id.eq_any(&done_task_ids))
    .get_results(&mut backend.connection)
    .expect("refetch the done tasks");
  let empty_reports: Vec<TaskReport> = refinalize_targets
    .into_iter()
    .map(|task| TaskReport {
      status: TaskStatus::NoProblem,
      messages: vec![],
      task,
    })
    .collect();
  assert!(backend.mark_done(&empty_reports).is_ok());
  let logs_after_refinalize: Result<i64, _> = log_infos::table
    .filter(task_id.eq_any(&done_task_ids))
    .count()
    .get_result(&mut backend.connection);
  assert_eq!(
    logs_after_refinalize,
    Ok(0),
    "re-finalizing with no messages deletes the prior logs (batched delete)"
  );

  // There should be no TODO tasks left for this service
  let pre_rerun_todo_count = tasks::table
    .filter(service_id.eq(mock_service.id))
    .filter(status.eq(TaskStatus::TODO.raw()))
    .count()
    .get_result(&mut backend.connection);
  assert_eq!(pre_rerun_todo_count, Ok(0));
  // mark_rerun of all tasks
  let mark_rerun_result = backend.mark_rerun(RerunOptions {
    corpus: &mock_corpus,
    service: &mock_service,
    severity_opt: None,
    category_opt: None,
    what_opt: None,
    owner_opt: None,
    description_opt: None,
  });
  println!("debug : {mark_rerun_result:?}");
  assert!(mark_rerun_result.is_ok());

  let post_rerun_todo_count: Result<i64, _> = tasks::table
    .filter(service_id.eq(mock_service.id))
    .filter(status.eq(TaskStatus::TODO.raw()))
    .count()
    .get_result(&mut backend.connection);
  // are all tasks marked as TODO after the rerun?
  assert_eq!(post_rerun_todo_count, Ok(mock_task_count as i64));
  // are all messages erased for those tasks?
  let post_rerun_logs_result: Result<i64, _> = log_infos::table
    .filter(task_id.eq_any(&done_task_ids))
    .count()
    .get_result(&mut backend.connection);
  assert_eq!(post_rerun_logs_result, Ok(0));

  let post_rerun_done_count: Result<i64, _> = tasks::table
    .filter(service_id.eq(mock_service.id))
    .filter(status.eq(TaskStatus::NoProblem.raw()))
    .count()
    .get_result(&mut backend.connection);
  println!("Found {post_rerun_done_count:?} done tasks");
  assert_eq!(post_rerun_done_count, Ok(0));

  // cleanup!
  let post_cleanup = backend.delete_by(&mock_new_task, "service_id");

  assert_eq!(post_cleanup, Ok(mock_task_count));
}

#[test]
fn mark_done_routes_messages_to_severity_tables() {
  // The batched per-table inserts must route each message to its own `log_*` table by severity.
  use cortex::models::{NewLogError, NewLogFatal, NewLogWarning};
  use cortex::schema::{log_errors, log_fatals, log_warnings};
  let mut backend = backend::testdb();
  let mock_service_id = random_mark();
  let mock_corpus_id = random_mark();
  let mock_task = NewTask {
    entry: String::from("sev_route_task"),
    service_id: mock_service_id,
    corpus_id: mock_corpus_id,
    status: TaskStatus::TODO.raw(),
  };
  let _ = backend.delete_by(&mock_task, "service_id");
  for index in 1..=2 {
    let indexed = NewTask {
      entry: format!("sev_route_task{index}"),
      ..mock_task.clone()
    };
    backend.add(&indexed).expect("add task");
  }
  let tasks: Vec<Task> = tasks::table
    .filter(service_id.eq(mock_service_id))
    .get_results(&mut backend.connection)
    .expect("fetch tasks");
  assert_eq!(tasks.len(), 2);
  let (id_a, id_b) = (tasks[0].id, tasks[1].id);
  let reports = vec![
    TaskReport {
      status: TaskStatus::Error,
      messages: vec![
        NewTaskMessage::Warning(NewLogWarning {
          task_id: id_a,
          category: String::from("cat"),
          what: String::from("what"),
          details: String::new(),
        }),
        NewTaskMessage::Error(NewLogError {
          task_id: id_a,
          category: String::from("cat"),
          what: String::from("what"),
          details: String::new(),
        }),
      ],
      task: tasks[0].clone(),
    },
    TaskReport {
      status: TaskStatus::Fatal,
      messages: vec![NewTaskMessage::Fatal(NewLogFatal {
        task_id: id_b,
        category: String::from("cat"),
        what: String::from("what"),
        details: String::new(),
      })],
      task: tasks[1].clone(),
    },
  ];
  backend.mark_done(&reports).expect("mark_done");

  // The status UPDATEs are batched by distinct status; with two different statuses in one batch,
  // each task must still receive its own (the grouping routes the disjoint id sets correctly).
  let status_a: i32 = tasks::table
    .find(id_a)
    .select(tasks::status)
    .first(&mut backend.connection)
    .unwrap();
  let status_b: i32 = tasks::table
    .find(id_b)
    .select(tasks::status)
    .first(&mut backend.connection)
    .unwrap();
  assert_eq!(
    status_a,
    TaskStatus::Error.raw(),
    "task_a finalized as Error"
  );
  assert_eq!(
    status_b,
    TaskStatus::Fatal.raw(),
    "task_b finalized as Fatal"
  );

  let ids = vec![id_a, id_b];
  let warns: i64 = log_warnings::table
    .filter(log_warnings::task_id.eq_any(&ids))
    .count()
    .get_result(&mut backend.connection)
    .unwrap();
  let errs: i64 = log_errors::table
    .filter(log_errors::task_id.eq_any(&ids))
    .count()
    .get_result(&mut backend.connection)
    .unwrap();
  let fatals: i64 = log_fatals::table
    .filter(log_fatals::task_id.eq_any(&ids))
    .count()
    .get_result(&mut backend.connection)
    .unwrap();
  assert_eq!(warns, 1, "the warning routed to log_warnings");
  assert_eq!(errs, 1, "the error routed to log_errors");
  assert_eq!(fatals, 1, "the fatal routed to log_fatals");

  // Cleanup (log_* have no FK cascade, so remove them explicitly, then the tasks).
  let _ = diesel::delete(log_warnings::table.filter(log_warnings::task_id.eq_any(&ids)))
    .execute(&mut backend.connection);
  let _ = diesel::delete(log_errors::table.filter(log_errors::task_id.eq_any(&ids)))
    .execute(&mut backend.connection);
  let _ = diesel::delete(log_fatals::table.filter(log_fatals::task_id.eq_any(&ids)))
    .execute(&mut backend.connection);
  let _ = backend.delete_by(&mock_task, "service_id");
}

#[test]
fn clear_limbo_except_preserves_in_flight_tasks() {
  // A ventilator restart re-runs limbo-clearing while the sink is still processing in-flight tasks;
  // those (held in `progress_queue`) must NOT be reset to TODO, or they get re-leased while their
  // original result is still pending (a double-dispatch). The excluded ids are preserved.
  let mut backend = backend::testdb();
  let mock_service_id = random_mark();
  let mock_corpus_id = random_mark();
  let mock_task = NewTask {
    entry: String::from("limbo_except_task"),
    service_id: mock_service_id,
    corpus_id: mock_corpus_id,
    status: TaskStatus::TODO.raw(),
  };
  let _ = backend.delete_by(&mock_task, "service_id");
  // Three tasks all marked Queued (a positive lease mark, > TODO).
  let queued_mark: i32 = 4321;
  for index in 1..=3 {
    let indexed = NewTask {
      entry: format!("limbo_except_task{index}"),
      status: queued_mark,
      ..mock_task.clone()
    };
    backend.add(&indexed).expect("add queued task");
  }
  let queued: Vec<Task> = tasks::table
    .filter(service_id.eq(mock_service_id))
    .get_results(&mut backend.connection)
    .expect("fetch queued tasks");
  assert_eq!(queued.len(), 3);
  let in_flight = vec![queued[0].id]; // pretend the first is in-flight (in progress_queue)
  backend
    .clear_limbo_tasks_except(&in_flight)
    .expect("clear limbo except in-flight");

  // The in-flight task is preserved (still Queued); the other two reset to TODO.
  let first_status: i32 = tasks::table
    .find(queued[0].id)
    .select(status)
    .first(&mut backend.connection)
    .unwrap();
  assert_eq!(
    first_status, queued_mark,
    "the in-flight task is NOT reset (no double-dispatch)"
  );
  let todo_count: i64 = tasks::table
    .filter(service_id.eq(mock_service_id))
    .filter(status.eq(TaskStatus::TODO.raw()))
    .count()
    .get_result(&mut backend.connection)
    .unwrap();
  assert_eq!(
    todo_count, 2,
    "the other two Queued tasks were recovered to TODO"
  );

  let _ = backend.delete_by(&mock_task, "service_id");
}
