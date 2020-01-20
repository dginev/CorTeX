// Copyright 2015-2020 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! A daemon for Redis cache expirationq
use cortex::backend::cache_worker;
use cortex::backend::Backend;
use std::process;

fn main() {
  let backend = Backend::default();
  backend
    .override_daemon_record("cache_worker".to_owned(), process::id())
    .expect("Could not register the process id with the backend, aborting...");
  cache_worker()
}
