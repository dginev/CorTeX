// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate rustlibxml;
extern crate zmq;

use cortex::manager::TaskManager;
use cortex::backend::DEFAULT_DB_ADDRESS;

/// A dispatcher executable for CorTeX distributed processing with ZMQ
fn main() {
  let manager = TaskManager {
    source_port : 5555,
    result_port : 5556,
    queue_size : 100000,
    message_size : 100,
    backend_address : DEFAULT_DB_ADDRESS.clone().to_string()
  };
  manager.start().unwrap();
}