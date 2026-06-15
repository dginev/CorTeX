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
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
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
  /// At least one admin/API token is configured (`auth.rerun_tokens`) — i.e. the deployment is
  /// sign-in-able. **Informational, not part of `ok`**: a freshly-`init`ed box legitimately has
  /// none until `cortex set-admin-token` runs, so this must not make `cortex init` exit
  /// non-zero.
  pub admin_token_configured: bool,
  /// Whether every *blocking* check passed (database + migrations + seeded services).
  pub ok: bool,
}

impl DoctorReport {
  /// Actionable next-step hints for any failing or unconfigured check, in fix-this-first order, so
  /// a stuck operator is told *how* to fix a red check, not merely that it is red. Empty when the
  /// box is healthy and configured. Shared by the `cortex doctor` text output and its JSON twin
  /// (the agent gets the same guidance).
  #[must_use]
  pub fn remediations(&self) -> Vec<String> {
    let mut hints = Vec::new();
    if !self.database_reachable {
      // Until the database is back, the migration / service checks are unknowable — fix this first
      // and re-run, rather than chasing the cascade of `false`s it produces.
      hints.push(
        "database unreachable — check the database URL (`cortex.toml` [database].url, or \
         DATABASE_URL) and that PostgreSQL is running"
          .to_string(),
      );
      return hints;
    }
    if !self.migrations_current {
      hints
        .push("schema out of date — run `cortex init` to apply the pending migrations".to_string());
    }
    // The built-in services are seeded by a migration, so a missing pair *while migrations are
    // current* means they were removed out of band; otherwise `cortex init` (above) restores them,
    // and a separate hint here would be redundant noise.
    if !self.services_seeded && self.migrations_current {
      hints.push(
        "the built-in `init`/`import` services are missing despite current migrations — they were \
         likely deleted; re-create them (originally seeded by migration \
         2017-10-01-204801_services)"
          .to_string(),
      );
    }
    if !self.admin_token_configured {
      hints.push(
        "no admin token configured — run `cortex set-admin-token --generate --owner <you>` to \
         enable sign-in and write actions"
          .to_string(),
      );
    }
    hints
  }
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
  // Auth readiness is independent of the database — a token may be configured even if the DB is
  // down.
  let admin_token_configured = !crate::config::config().auth.rerun_tokens.is_empty();
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
        admin_token_configured,
        ok: database_reachable && migrations_current && services_seeded,
      }
    },
    Err(_) => DoctorReport {
      database_reachable: false,
      migrations_current: false,
      services_seeded: false,
      admin_token_configured,
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

/// The outcome of `cortex set-admin-token`.
#[derive(Debug, Serialize)]
pub struct SetTokenOutcome {
  /// The owner the token now maps to.
  pub owner: String,
  /// `true` if the token already existed (its owner was updated) rather than added.
  pub replaced: bool,
  /// How many tokens are configured after the write.
  pub token_count: usize,
  /// `true` if a legacy `config.json` in the working directory shadows `cortex.toml`'s `[auth]`
  /// (so the written token will not take effect until that file is reconciled or removed).
  pub shadowed_by_legacy_json: bool,
}

/// Generates a fresh random admin/API token: 32 URL-safe alphanumeric characters (~190 bits). Used
/// by `cortex set-admin-token --generate`. The token is a plaintext bearer credential (the
/// lightweight scheme — see `docs/archive/AAA_DESIGN.md`); hashing-at-rest is a documented later
/// step.
pub fn generate_token() -> String {
  thread_rng()
    .sample_iter(&Alphanumeric)
    .take(32)
    .map(char::from)
    .collect()
}

/// Sets (or updates) an admin/API token in the `[auth].rerun_tokens` table of `config_path`,
/// **merging** into the existing file so other sections and other tokens are preserved — no
/// hand-editing of `cortex.toml`. The library logic behind `cortex set-admin-token`.
///
/// If the file is missing it is first scaffolded from the defaults (a complete config, like `cortex
/// init`) so the result is always valid. Because `to_persisted_toml` never writes secrets, this
/// merges at the raw-TOML level rather than re-serializing a `CortexConfig`. Mapping the token to a
/// per-person `owner` is what gives the audit log its actor (`docs/archive/AAA_DESIGN.md`).
pub fn set_admin_token(
  config_path: &Path,
  token: &str,
  owner: &str,
) -> Result<SetTokenOutcome, String> {
  if token.is_empty() {
    return Err("refusing to set an empty token".to_string());
  }
  // Start from the existing file, or a fresh complete scaffold when none exists yet.
  let existing = match std::fs::read_to_string(config_path) {
    Ok(text) => text,
    Err(_) => to_persisted_toml(&CortexConfig::default())
      .map_err(|e| format!("cannot scaffold config: {e}"))?,
  };
  let mut document: toml::Table = existing
    .parse()
    .map_err(|e| format!("cannot parse {}: {e}", config_path.display()))?;
  // Navigate/create [auth].rerun_tokens, then insert. `entry` preserves any sibling keys.
  let auth = document
    .entry("auth")
    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
  let auth_table = auth
    .as_table_mut()
    .ok_or_else(|| "the [auth] section is not a table".to_string())?;
  let tokens = auth_table
    .entry("rerun_tokens")
    .or_insert_with(|| toml::Value::Table(toml::Table::new()));
  let tokens_table = tokens
    .as_table_mut()
    .ok_or_else(|| "auth.rerun_tokens is not a table".to_string())?;
  let replaced = tokens_table
    .insert(token.to_string(), toml::Value::String(owner.to_string()))
    .is_some();
  let token_count = tokens_table.len();
  let serialized =
    toml::to_string_pretty(&document).map_err(|e| format!("cannot serialize config: {e}"))?;
  std::fs::write(config_path, serialized)
    .map_err(|e| format!("cannot write {}: {e}", config_path.display()))?;
  Ok(SetTokenOutcome {
    owner: owner.to_string(),
    replaced,
    token_count,
    // The loader treats a working-directory config.json as authoritative for [auth] (back-compat).
    shadowed_by_legacy_json: Path::new("config.json").exists(),
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
