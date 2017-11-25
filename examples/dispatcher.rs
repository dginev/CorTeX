// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
// extern crate libxml;
extern crate zmq;

use cortex::manager::TaskManager;
use cortex::backend::DEFAULT_DB_ADDRESS;

/// A dispatcher executable for `CorTeX` distributed processing with ZMQ
fn main() {
  let manager = TaskManager {
    source_port: 5555,
    result_port: 5556,
    // Note that queue_size must never be larged than postgresql's max_locks_per_transaction setting
    //   (typically specified in /etc/postgresql/9.1/main/postgresql.conf or similar)
    queue_size: 800, // If we have 400 CPUs, this is allows us two task dispatches before reload, should be fine.
    message_size: 100000,
    backend_address: DEFAULT_DB_ADDRESS.to_string(),
  };
  let job_limit = None;
  manager.start(job_limit).unwrap();
}
