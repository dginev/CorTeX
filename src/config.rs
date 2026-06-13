// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Layered, runtime configuration for `CorTeX`.
//!
//! This replaces two prototype-era patterns: the **compile-time** `dotenv!` baking of the database
//! URL into the binary (so the deployment target could not change without a rebuild), and ad-hoc
//! hard-coded constants for the dispatcher ports and cache address.
//!
//! Values are resolved with the following precedence (lowest to highest):
//! 1. built-in [`Default`] values,
//! 2. an optional `cortex.toml` file in the working directory,
//! 3. `CORTEX_`-prefixed environment variables (nested with `__`, e.g. `CORTEX_DISPATCHER__SOURCE_PORT`),
//! 4. the legacy `DATABASE_URL` / `TEST_DATABASE_URL` variables (also read from a local `.env`),
//!    which take final precedence so existing deployments keep working unchanged.
//!
//! Access the process-wide configuration through [`config()`].

use figment::{
  providers::{Env, Format, Serialized, Toml},
  Figment,
};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Database connection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
  /// Connection URL for the production database.
  pub url: String,
  /// Connection URL for the test database (used by the integration test-suite).
  pub test_url: String,
}
impl Default for DatabaseConfig {
  fn default() -> Self {
    DatabaseConfig {
      url: "postgres://cortex:cortex@localhost/cortex".to_string(),
      test_url: "postgres://cortex_tester:cortex_tester@localhost/cortex_tester".to_string(),
    }
  }
}

/// ZeroMQ dispatcher settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatcherConfig {
  /// Port the ventilator listens on for worker task requests.
  pub source_port: usize,
  /// Port the sink listens on for worker results.
  pub result_port: usize,
  /// Batch size for task-store queue requests (also the in-memory dispatch queue size).
  ///
  /// Must never exceed PostgreSQL's `max_locks_per_transaction` setting.
  pub queue_size: usize,
  /// Size of an individual ZeroMQ message chunk, in bytes.
  pub message_size: usize,
}
impl Default for DispatcherConfig {
  fn default() -> Self {
    DispatcherConfig {
      source_port: 51695,
      result_port: 51696,
      queue_size: 800,
      message_size: 100_000,
    }
  }
}

/// Report-cache (Redis) settings used by the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
  /// Redis connection URL used to cache frontend report pages.
  pub redis_url: String,
  /// Whether the frontend should refuse to start when the cache is unreachable.
  /// (Graceful degradation when the cache is optional is tracked as plan Arm 11.)
  pub required: bool,
}
impl Default for CacheConfig {
  fn default() -> Self {
    CacheConfig {
      redis_url: "redis://127.0.0.1/".to_string(),
      required: false,
    }
  }
}

/// Top-level `CorTeX` runtime configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CortexConfig {
  /// Database connection settings.
  pub database: DatabaseConfig,
  /// ZeroMQ dispatcher settings.
  pub dispatcher: DispatcherConfig,
  /// Report-cache settings.
  pub cache: CacheConfig,
}

impl CortexConfig {
  /// Builds the layered configuration figment (defaults → `cortex.toml` → `CORTEX_` env).
  /// Does not apply the legacy `DATABASE_URL` overrides; see [`CortexConfig::load`].
  pub fn figment() -> Figment {
    Figment::from(Serialized::defaults(CortexConfig::default()))
      .merge(Toml::file("cortex.toml"))
      .merge(Env::prefixed("CORTEX_").split("__"))
  }

  /// Loads and validates the configuration, applying legacy environment overrides last.
  /// Panics with a clear message if the configuration is malformed.
  pub fn load() -> CortexConfig {
    // Load a local `.env` into the process environment for backwards compatibility.
    dotenvy::dotenv().ok();
    let mut config: CortexConfig = CortexConfig::figment()
      .extract()
      .unwrap_or_else(|e| panic!("invalid CorTeX configuration: {e}"));
    // The historic `DATABASE_URL` / `TEST_DATABASE_URL` variables remain authoritative when set,
    // so existing `.env`-based deployments behave exactly as before.
    if let Ok(url) = std::env::var("DATABASE_URL") {
      config.database.url = url;
    }
    if let Ok(url) = std::env::var("TEST_DATABASE_URL") {
      config.database.test_url = url;
    }
    config
  }
}

/// Returns the process-wide, lazily-loaded configuration.
pub fn config() -> &'static CortexConfig {
  static CONFIG: LazyLock<CortexConfig> = LazyLock::new(CortexConfig::load);
  &CONFIG
}
