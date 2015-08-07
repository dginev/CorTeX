extern crate cortex;
extern crate postgres;

use cortex::backend::*;
// use postgres::{Connection, SslMode};

#[test]
fn init_tables() {
  let backend = Backend::testdb();
  assert!(backend.setup_task_tables().is_ok())
}

#[test]
fn import_mock_task() {
  // backend = 
}