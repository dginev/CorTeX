// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
use cortex::backend::{Backend};

#[test]
fn mock_round_trip() {
  // Initialize a corpus, import a single task, and enable a service on it
  let test_backend = Backend::testdb();
  assert!(test_backend.setup_task_tables().is_ok());

  // Start up a client

  // Start up an echo worker

  // Check round-trip success
}