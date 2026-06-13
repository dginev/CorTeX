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
use std::collections::HashMap;
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

/// Frontend authentication / secrets (formerly the hand-edited `config.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthConfig {
  /// A captcha secret registered with the captcha provider (currently unused by the codebase).
  pub captcha_secret: String,
  /// Password-like tokens mapped to a human-readable owner, gating rerun / save-snapshot actions.
  pub rerun_tokens: HashMap<String, String>,
}

/// On-disk asset locations, so the binary is not bound to its working directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetsConfig {
  /// Directory holding the Tera templates.
  pub template_dir: String,
  /// Directory holding the static public assets (css/js/images, favicon, robots.txt).
  pub public_dir: String,
}
impl Default for AssetsConfig {
  fn default() -> Self {
    AssetsConfig {
      template_dir: "templates".to_string(),
      public_dir: "public".to_string(),
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
  /// Frontend authentication / secrets.
  pub auth: AuthConfig,
  /// On-disk asset locations.
  pub assets: AssetsConfig,
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
    // Back-compat: the legacy frontend `config.json` (captcha_secret + rerun_tokens), if present in
    // the working directory, remains authoritative for the auth section so running deployments keep
    // working. The new home for these values is the `[auth]` section of `cortex.toml` / `CORTEX_AUTH__*`.
    if let Ok(text) = std::fs::read_to_string("config.json") {
      match serde_json::from_str::<LegacyFrontendConfig>(&text) {
        Ok(legacy) => {
          config.auth.captcha_secret = legacy.captcha_secret;
          config.auth.rerun_tokens = legacy.rerun_tokens;
        },
        Err(e) => eprintln!("-- ignoring malformed config.json: {e}"),
      }
    }
    config
  }
}

/// Legacy on-disk shape of the prototype `config.json`, read only for backwards compatibility.
#[derive(Deserialize)]
struct LegacyFrontendConfig {
  captcha_secret: String,
  rerun_tokens: HashMap<String, String>,
}

/// Returns the process-wide, lazily-loaded configuration.
pub fn config() -> &'static CortexConfig {
  static CONFIG: LazyLock<CortexConfig> = LazyLock::new(CortexConfig::load);
  &CONFIG
}
