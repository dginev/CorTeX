// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the connection pool: a pooled connection runs a query.

use cortex::backend::{build_pool, test_db_address};
use diesel::prelude::*;

#[test]
fn pool_checks_out_a_working_connection() {
  let pool = build_pool(test_db_address(), 4);
  let mut connection = pool.get().expect("the pool should check out a connection");
  let ran = diesel::sql_query("SELECT 1")
    .execute(&mut *connection)
    .is_ok();
  assert!(ran, "a pooled connection should run a trivial query");
}
