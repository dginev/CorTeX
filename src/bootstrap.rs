// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Self-install and diagnostics: the library logic behind `cortex init` and `cortex doctor`.
//!
//! Kept in the library (not the binary) so the contracts are testable; the `cortex` binary is a
//! thin renderer over these functions.

use std::path::Path;

use diesel::pg::PgConnection;
use diesel::prelude::*;
use serde::Serialize;

use crate::config::{to_persisted_toml, CortexConfig};
use crate::migrations;

/// Structured diagnostics — the data contract shared by `cortex doctor` and its agent twin.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
  /// The database accepts a connection and a trivial query.
  pub database_reachable: bool,
  /// The schema is at the latest embedded migration.
  pub migrations_current: bool,
  /// The built-in `init` and `import` services are seeded.
  pub services_seeded: bool,
  /// Whether every check passed.
  pub ok: bool,
}

/// The outcome of `cortex init`.
#[derive(Debug, Serialize)]
pub struct InitOutcome {
  /// Migration versions applied (empty when the database was already current).
  pub migrations_applied: Vec<String>,
  /// Whether a configuration file was scaffolded (because it was missing).
  pub config_created: bool,
}

/// Runs the install diagnostics against the given database URL.
pub fn doctor(database_url: &str) -> DoctorReport {
  match PgConnection::establish(database_url) {
    Ok(mut connection) => {
      let database_reachable = diesel::sql_query("SELECT 1")
        .execute(&mut connection)
        .is_ok();
      let migrations_current = !migrations::has_pending_migrations(&mut connection);
      let services_seeded = builtin_services_present(&mut connection);
      DoctorReport {
        database_reachable,
        migrations_current,
        services_seeded,
        ok: database_reachable && migrations_current && services_seeded,
      }
    },
    Err(_) => DoctorReport {
      database_reachable: false,
      migrations_current: false,
      services_seeded: false,
      ok: false,
    },
  }
}

/// Self-migrates the database (embedded migrations) and scaffolds a config file if one is missing.
pub fn init(database_url: &str, config_path: &Path) -> Result<InitOutcome, String> {
  let mut connection = PgConnection::establish(database_url)
    .map_err(|e| format!("cannot connect to database: {e}"))?;
  let migrations_applied = migrations::run_pending_migrations(&mut connection)
    .map_err(|e| format!("migration failed: {e}"))?;
  let config_created = if config_path.exists() {
    false
  } else {
    let toml = to_persisted_toml(&CortexConfig::default())
      .map_err(|e| format!("cannot serialize config: {e}"))?;
    std::fs::write(config_path, toml).map_err(|e| format!("cannot write config: {e}"))?;
    true
  };
  Ok(InitOutcome {
    migrations_applied,
    config_created,
  })
}

/// Whether the built-in `init` and `import` services are present in the database.
fn builtin_services_present(connection: &mut PgConnection) -> bool {
  use crate::schema::services::dsl::{name, services};
  let count: i64 = services
    .filter(name.eq_any(["init", "import"]))
    .count()
    .get_result(connection)
    .unwrap_or(0);
  count >= 2
}
