use pericortex::worker::Worker;
use cortex::worker::InitWorker;
use cortex::backend::Backend;
use std::process;
use std::error::Error;


fn main() -> Result<(), Box<dyn Error>> {
  let backend = Backend::default();
  backend
    .override_daemon_record("init_worker".to_owned(), process::id())
    .expect("Could not register the process id with the backend, aborting...");

  InitWorker::default().start(None)
}