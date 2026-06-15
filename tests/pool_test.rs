// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the connection pool: a pooled connection runs a query.

use cortex::backend::{build_pool, test_db_address};
use diesel::prelude::*;

fn pool_checks_out_a_working_connection() {
  let pool = build_pool(test_db_address(), 4);
  let mut connection = pool.get().expect("the pool should check out a connection");
  let ran = diesel::sql_query("SELECT 1")
    .execute(&mut *connection)
    .is_ok();
  assert!(ran, "a pooled connection should run a trivial query");
}

// Custom harness (see KNOWN_ISSUES L-1): run the case then `_exit(0)` so the racy C atexit cleanup
// (libpq global teardown vs the still-live r2d2 reaper thread this bare pool spawns) never runs —
// it SIGSEGV'd here ~4/5 of the time. A panic above still aborts non-zero, so a real failure fails
// CI.
fn main() {
  pool_checks_out_a_working_connection();
  eprintln!("pool_test: all cases passed");
  unsafe { libc::_exit(0) }
}
