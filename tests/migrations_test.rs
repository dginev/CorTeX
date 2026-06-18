// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract tests for the embedded-migrations interface (the self-install enabler).
//! High level: the binary carries its migrations and can self-migrate without `diesel_cli`.

use cortex::{backend, migrations};

#[test]
fn migrated_test_db_reports_no_pending_migrations() {
  let mut backend = backend::testdb();
  assert!(
    !migrations::has_pending_migrations(&mut backend.connection),
    "an already-migrated database must report zero pending migrations"
  );
}

#[test]
fn run_pending_migrations_is_idempotent() {
  let mut backend = backend::testdb();
  let applied =
    migrations::run_pending_migrations(&mut backend.connection).expect("migrations should run");
  assert!(
    applied.is_empty(),
    "an already-current database should apply nothing"
  );
}
