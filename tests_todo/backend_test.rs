// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate postgres;

use cortex::backend;

#[test]
fn init_tables() {
  let backend = backend::testdb();
  // Test table reset and basic getters?
}

#[test]
fn connection_pool_availability() {
  // Test connections are available
  // Test going over the MAX pool limit of connections is sane
}