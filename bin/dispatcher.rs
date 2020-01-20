// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
use cortex::backend::{self, DEFAULT_DB_ADDRESS};
use cortex::dispatcher::manager::TaskManager;
use std::process;

/// A dispatcher executable for `CorTeX` distributed processing with ZMQ
fn main() {
  let backend = backend::from_address(DEFAULT_DB_ADDRESS);
  backend
    .override_daemon_record("dispatcher".to_owned(), process::id())
    .expect("Could not register the process id with the backend, aborting...");

  let manager = TaskManager {
    source_port: 51695,
    result_port: 51696,
    // Note that queue_size must never be larged than postgresql's max_locks_per_transaction setting
    //   (typically specified in /etc/postgresql/9.1/main/postgresql.conf or similar)
    queue_size: 800, /* If we have 400 CPUs, this is allows us two task dispatches before
                      * reload, should be fine. */
    message_size: 100_000,
    backend_address: DEFAULT_DB_ADDRESS.to_string(),
  };
  manager
    .start(None)
    .unwrap_or_else(|_| panic!("Failed to start TaskManager"));
}
