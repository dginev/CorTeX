// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate postgres;

use cortex::backend::*;

#[test]
fn init_tables() {
  let backend = Backend::testdb();
  assert!(backend.setup_task_tables().is_ok())
}

#[test]
fn import_mock_task() {
  // backend = 
}