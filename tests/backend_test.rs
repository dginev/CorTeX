// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate diesel;

use cortex::backend;
use cortex::models::{Corpus, Service, NewTask, Task, NewLogInfo};
use cortex::helpers::{TaskStatus, TaskReport, NewTaskMessage, random_mark, rand_in_range};
use cortex::schema::tasks;
use cortex::schema::tasks::dsl::{service_id, status};
use diesel::prelude::*;

#[test]
fn task_table_crud() {
  let backend = backend::testdb();
  let mock_service_id = random_mark();
  let mock_corpus_id = random_mark();
  let mock_service = Service {
    id: mock_service_id,
    name: String::from("mock_service"),
    complex: false,
    inputconverter: None,
    inputformat: String::from("tex"),
    outputformat: String::from("tex"),
    version: 0.1,
  };
  let mock_task = NewTask {
    entry: "mock_task",
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
  let backend = backend::testdb();
  // Add 100 tasks, out of which we will mark 17
  let mock_service_id = random_mark();
  let mock_corpus_id = random_mark();
  let mock_service = Service {
    id: mock_service_id,
    name: String::from("mark_tasks"),
    complex: false,
    inputconverter: None,
    inputformat: String::from("tex"),
    outputformat: String::from("tex"),
    version: 0.1,
  };
  let mock_task = NewTask {
    entry: "mark_task",
    service_id: mock_service.id,
    corpus_id: mock_corpus_id,
    status: TaskStatus::TODO.raw(),
  };

  assert!(backend.delete_by(&mock_task, "service_id").is_ok());

  // insert 100 tasks
  for index in 1..101 {
    let indexed_task = NewTask {
      entry: &format!("{}{}", mock_task.entry, index.to_string()),
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
    .get_result(&backend.connection);
  assert_eq!(marked_in_db, Ok(17));

  let cleared_limbo_tasks = backend.clear_limbo_tasks();
  assert_eq!(cleared_limbo_tasks, Ok(17));
  let marked_in_db_2 = tasks::table
    .filter(status.eq(random_mark))
    .count()
    .get_result(&backend.connection);
  assert_eq!(marked_in_db_2, Ok(0));

  let post_cleanup = backend.delete_by(&mock_task, "service_id");
  assert_eq!(post_cleanup, Ok(100));
}

#[test]
fn batch_ops_test() {
  let backend = backend::testdb();
  let mock_service = Service {
    id: random_mark(),
    name: String::from("batch_ops_test"),
    complex: false,
    inputconverter: None,
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
  };

  let mock_task_count = rand_in_range(10, 100) as usize;
  let mock_new_task = NewTask {
    entry: "mock_task",
    service_id: mock_service.id,
    corpus_id: mock_corpus_id,
    status: TaskStatus::TODO.raw(),
  };
  // mark_imported on `mock_task_count` tasks
  let names: Vec<String> = (1..mock_task_count + 1)
    .map(|index| {
      format!("{}{}", mock_new_task.entry, index.to_string())
    })
    .collect();
  let new_tasks: Vec<NewTask> = (0..mock_task_count)
    .map(|index| {
      NewTask {
        entry: &names[index],
        ..mock_new_task
      }
    })
    .collect();
  let imported_count = backend.mark_imported(&new_tasks);
  assert_eq!(imported_count, Ok(mock_task_count));
  let todo_tasks_result: Result<Vec<Task>, _> = tasks::table
    .filter(service_id.eq(mock_service.id))
    .filter(status.eq(TaskStatus::TODO.raw()))
    .get_results(&backend.connection);
  assert!(todo_tasks_result.is_ok());
  let todo_tasks = todo_tasks_result.unwrap();
  // We now have `mock_task_count` registered tasks in the DB
  assert_eq!(todo_tasks.len(), mock_task_count);

  // mark_done -- time to mark them as "done", with some trivial ok reports,
  //              with a single info message
  let task_reports: Vec<TaskReport> = todo_tasks
    .into_iter()
    .map(|task| {
      TaskReport {
        status: TaskStatus::NoProblem,
        messages: vec![
          NewTaskMessage::Info(NewLogInfo {
            task_id: task.id,
            category: String::from("trivial"),
            what: String::from("mock"),
            details: String::new(),
          }),
        ],
        task,
      }
    })
    .collect();

  let mark_done_result = backend.mark_done(&task_reports);
  assert!(mark_done_result.is_ok());
  let done_tasks_result: Result<Vec<Task>, _> = tasks::table
    .filter(service_id.eq(mock_service.id))
    .filter(status.eq(TaskStatus::NoProblem.raw()))
    .get_results(&backend.connection);
  assert!(done_tasks_result.is_ok());
  let done_tasks = done_tasks_result.unwrap();
  assert_eq!(done_tasks.len(), mock_task_count);

  // mark_rerun of all tasks
  let mark_rerun_result = backend.mark_rerun(&mock_corpus, &mock_service, None, None, None);
  assert!(mark_rerun_result.is_ok());

  let post_rerun_todo_count: Result<i64, _> = tasks::table
    .filter(service_id.eq(mock_service.id))
    .filter(status.eq(TaskStatus::TODO.raw()))
    .count()
    .get_result(&backend.connection);
  assert_eq!(post_rerun_todo_count, Ok(mock_task_count as i64));

  let post_rerun_done_count: Result<i64, _> = tasks::table
    .filter(service_id.eq(mock_service.id))
    .filter(status.eq(TaskStatus::NoProblem.raw()))
    .count()
    .get_result(&backend.connection);
  println!("Found {:?} done tasks", post_rerun_done_count);
  assert_eq!(post_rerun_done_count, Ok(0));

  // cleanup!
  let post_cleanup = backend.delete_by(&mock_new_task, "service_id");

  assert_eq!(post_cleanup, Ok(mock_task_count));
}