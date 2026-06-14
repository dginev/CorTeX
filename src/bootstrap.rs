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

/// Operator guidance for tuning the PostgreSQL **server** (`shared_buffers`, `work_mem`, …),
/// printed by `cortex tune-db` and at the end of `cortex init`. Per the owner decision (see
/// `docs/DB_TUNING.md`), `cortex` does **not** reimplement the pgtune heuristic — it points at the
/// upstream service and pre-fills the host's RAM / cores so the operator's inputs are ready. The
/// per-table autovacuum is
/// already automatic (a migration); this is the host-sized server config (see `docs/DB_TUNING.md`).
pub fn db_tuning_guidance() -> String {
  format!(
    "PostgreSQL server tuning is recommended but not automated.\n\
     CorTeX is a \"Mixed\" workload (OLTP task/log writes + DW bulk-loads + reporting); the stock\n\
     defaults (shared_buffers=128MB, work_mem=4MB) are far too small for a real corpus.\n\n\
     1. Generate a config at https://pgtune.leopard.in.ua/ with these inputs:\n\
     \x20    DB Type = mixed   OS = linux   DB Version = <your PG major>\n\
     \x20    Total RAM = {ram}   CPUs = {cores}   Connections = 300   Storage = nvme (or ssd)\n\
     2. Apply the ALTER SYSTEM block it prints, then restart PostgreSQL.\n\
     \x20  (build note: keep wal_compression=lz4 / io_method=io_uring only if your build supports them.)\n\n\
     A verified example block (256 GB / 64 cores / nvme) is in docs/DB_TUNING.md.",
    ram = total_ram_hint(),
    cores = core_hint(),
  )
}

/// Best-effort host RAM for the tuning guidance (Linux `/proc/meminfo`), or a placeholder.
fn total_ram_hint() -> String {
  std::fs::read_to_string("/proc/meminfo")
    .ok()
    .and_then(|meminfo| {
      meminfo
        .lines()
        .find(|line| line.starts_with("MemTotal"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<u64>().ok())
    })
    .map(|kb| format!("{} GB", kb / 1024 / 1024))
    .unwrap_or_else(|| "<your host RAM>".to_string())
}

/// Best-effort CPU hint for the tuning guidance (logical count, with a physical-cores reminder).
fn core_hint() -> String {
  std::thread::available_parallelism()
    .map(|logical| {
      format!("{logical} (use *physical* cores — often half this on hyperthreaded CPUs)")
    })
    .unwrap_or_else(|_| "<physical cores>".to_string())
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
