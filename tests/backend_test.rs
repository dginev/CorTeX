// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate postgres;

use cortex::backend;
use cortex::models::{Service,NewTask};
use cortex::helpers::TaskStatus;

#[test]
fn task_table_crud() {
  let backend = backend::testdb();
  let mock_service = Service{
  	id: 1, name: String::from("mock_service"), complex: false, 
  	inputconverter: None, inputformat: String::from("tex"), outputformat: String::from("tex"), version: 0.1};
  let mock_task = NewTask{entry: "mock_task", serviceid: mock_service.id, corpusid:1, status: TaskStatus::TODO.raw()};
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
  println!("Fetched tasks: {:?}", fetched_tasks);
  assert_eq!(fetched_tasks.len(), 1);
  let fetched_task = fetched_tasks.first().unwrap();
  assert!(fetched_task.id > 0);
  assert_eq!(fetched_task.entry, mock_task.entry);

  // Delete again and verify deletion works
  let cleanup = backend.delete_by(&mock_task, "entry");
  assert_eq!(cleanup, Ok(1));
}
