extern crate cortex;
use cortex::backend::*;

#[test]
fn init_tables() {
  let backend = Backend::default();
  assert!(backend.setup_task_tables().is_ok())
}

#[test]
fn import_mock_task() {
  // backend = 
}