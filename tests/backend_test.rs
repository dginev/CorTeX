// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate postgres;

use cortex::backend;
use cortex::models::NewTask;

#[test]
fn task_table_crud() {
  let backend = backend::testdb();
  let test_task = NewTask{entry: "mock_task", serviceid: 1, corpusid:1, status: -5};
  // Delete any mock tasks
  let cleanup = backend.delete_by(&test_task, "entry");
  assert_eq!(cleanup, Ok(0));

  // Add a mock task
  let count_added = backend.add(&test_task);
  assert_eq!(count_added, Ok(1)); // new task!
  // Get the fields from the DB

  // Delete again and verify deletion works
  let cleanup = backend.delete_by(&test_task, "entry");
  assert_eq!(cleanup, Ok(1));
}

#[test]
fn connection_pool_availability() {
  // Test connections are available
  // Test going over the MAX pool limit of connections is sane
}
