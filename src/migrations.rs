// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Embedded database migrations.
//!
//! The migrations under `migrations/` are baked into the binary at compile time, so `cortex init`
//! (and any deployment) can self-migrate the database with **no `diesel_cli` on the host**.

use diesel::pg::PgConnection;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

/// The set of migrations compiled into the binary (from the `migrations/` directory).
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

/// Returns whether the database has any pending (un-applied) migrations.
pub fn has_pending_migrations(connection: &mut PgConnection) -> bool {
  connection.has_pending_migration(MIGRATIONS).unwrap_or(true)
}

/// Applies all pending migrations, returning the versions applied (empty when already current).
pub fn run_pending_migrations(
  connection: &mut PgConnection,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
  let applied = connection.run_pending_migrations(MIGRATIONS)?;
  Ok(applied.iter().map(|version| version.to_string()).collect())
}
