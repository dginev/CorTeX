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
//! hard-coded constants for the dispatcher ports.
//!
//! Values are resolved with the following precedence (lowest to highest):
//! 1. built-in [`Default`] values,
//! 2. an optional `cortex.toml` file in the working directory,
//! 3. `CORTEX_`-prefixed environment variables (nested with `__`, e.g.
//!    `CORTEX_DISPATCHER__SOURCE_PORT`),
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
  /// Maximum size of the web frontend connection pool. Sized for the expected load (~2 admins +
  /// 20 users; the ~200 workers speak ZeroMQ to the dispatcher, not Postgres) within PostgreSQL's
  /// default `max_connections` of 100, which is shared with the dispatcher.
  pub pool_size: u32,
}
impl Default for DatabaseConfig {
  fn default() -> Self {
    DatabaseConfig {
      url: "postgres://cortex:cortex@localhost/cortex".to_string(),
      test_url: "postgres://cortex_tester:cortex_tester@localhost/cortex_tester".to_string(),
      pool_size: 32,
    }
  }
}

/// ZeroMQ dispatcher settings.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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
  /// Backpressure threshold: the maximum number of in-flight (dispatched-but-unfinished) tasks the
  /// ventilator tolerates before it stops leasing new work and mock-replies to requesting workers
  /// (which back off and retry). This bounds the in-flight set so it drains via the sink as
  /// results return, instead of growing toward the hard panic bound
  /// [`crate::dispatcher::server::PROGRESS_QUEUE_HARD_LIMIT`] — graceful degradation under
  /// overload rather than a crash (KNOWN_ISSUES D-6). Keep it well below that hard bound to
  /// leave recovery headroom. In steady state the in-flight set is ~the worker count (~200), so
  /// the default leaves a wide margin.
  pub max_in_flight: usize,
  /// How often (seconds) the finalize thread refreshes the `report_summary` rollup *regardless of
  /// drain*, bounding report staleness while a long run is in flight (a conversion run can take
  /// weeks, so drain-only refresh is not enough). This is the automatic freshness guarantee; with
  /// `REFRESH ... CONCURRENTLY` the rebuild no longer blocks readers, so it is cheap to run often.
  /// The cost is one rebuild's DB load per interval (a few minutes at production scale). Default
  /// 1h.
  pub report_refresh_interval_seconds: u64,
  /// **Hard cap** on the byte size of a single worker result the sink will write to `/data`. A
  /// reply that exceeds it is **rejected** (the partial file is removed, the rest of the
  /// multipart message is drained frame-by-frame to keep the socket in sync, and the task is
  /// marked `Invalid`) rather than allowed to fill the disk — protecting the shared filesystem
  /// from a runaway worker. We accept genuinely large jobs but draw the line here. Default 2
  /// GiB.
  pub max_result_bytes: usize,
}
impl Default for DispatcherConfig {
  fn default() -> Self {
    DispatcherConfig {
      source_port: 51695,
      result_port: 51696,
      queue_size: 800,
      message_size: 100_000,
      max_in_flight: 5000,
      report_refresh_interval_seconds: 3600,
      max_result_bytes: 2 * 1024 * 1024 * 1024, // 2 GiB
    }
  }
}

/// Frontend authentication / secrets (formerly the hand-edited `config.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthConfig {
  /// Password-like tokens mapped to a human-readable owner. The bootstrap / break-glass + agent
  /// credential that gates every write action and the `/admin` sign-in, alongside passkeys (see
  /// [`WebauthnConfig`] and `docs/AAA_DESIGN.md`). Set via `cortex set-admin-token`.
  pub rerun_tokens: HashMap<String, String>,
}

/// Passkey (**WebAuthn**) sign-in settings for the human admin UI (`docs/WEBAUTHN_DESIGN.md`). The
/// relying party is the CorTeX server itself — no external IdP, no per-deploy app registration. The
/// admin token (see [`AuthConfig`]) remains the bootstrap / break-glass path; passkeys are the
/// convenient day-to-day human sign-in once enrolled.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WebauthnConfig {
  /// Whether passkey sign-in is offered. Off until a deployment configures `rp_id`/`rp_origin` and
  /// an admin enrolls a passkey (the token path keeps working regardless).
  pub enabled: bool,
  /// The relying-party id: the registrable domain passkeys are scoped to (host only, no scheme or
  /// port), e.g. `localhost` for development or `corpora.latexml.rs` for the preview deployment.
  pub rp_id: String,
  /// The full origin the app is served from (scheme + host + optional port), e.g.
  /// `http://localhost:8000` or `https://corpora.latexml.rs`. WebAuthn requires a secure context
  /// (https) in production; `localhost` is exempt for development.
  pub rp_origin: String,
}
impl Default for WebauthnConfig {
  fn default() -> Self {
    WebauthnConfig {
      enabled: false,
      rp_id: "localhost".to_string(),
      rp_origin: "http://localhost:8000".to_string(),
    }
  }
}

/// On-disk asset locations, so the binary is not bound to its working directory.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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
  /// Frontend authentication / secrets.
  pub auth: AuthConfig,
  /// On-disk asset locations.
  pub assets: AssetsConfig,
  /// Passkey (WebAuthn) sign-in settings.
  pub webauthn: WebauthnConfig,
}

impl CortexConfig {
  /// Builds the layered configuration figment (defaults → `cortex.toml` → `CORTEX_` env).
  /// Does not apply the legacy `DATABASE_URL` overrides; see [`CortexConfig::load`].
  pub fn figment() -> Figment {
    Figment::from(Serialized::defaults(CortexConfig::default()))
      .merge(Toml::file(config_file_path()))
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
    // Back-compat: the legacy frontend `config.json` (rerun_tokens), if present in the working
    // directory, remains authoritative for the auth section so running deployments keep working.
    // The new home for these values is the `[auth]` section of `cortex.toml` / `CORTEX_AUTH__*`
    // (written by `cortex set-admin-token`). The prototype's `captcha_secret` is gone — bot
    // protection is a deployment concern (an Anubis reverse proxy), not framework code
    // (docs/DEPLOYMENT.md).
    if let Ok(text) = std::fs::read_to_string("config.json") {
      match serde_json::from_str::<LegacyFrontendConfig>(&text) {
        Ok(legacy) => config.auth.rerun_tokens = legacy.rerun_tokens,
        Err(e) => eprintln!("-- ignoring malformed config.json: {e}"),
      }
    }
    config
  }
}

/// Legacy on-disk shape of the prototype `config.json`, read only for backwards compatibility.
/// Extra fields (e.g. the removed `captcha_secret`) are ignored.
#[derive(Deserialize)]
struct LegacyFrontendConfig {
  rerun_tokens: HashMap<String, String>,
}

/// Serializes the non-secret configuration sections (everything except the `auth` secrets) to TOML.
/// Shared by `cortex init`'s config scaffold and the Settings write path so the on-disk shape is
/// identical.
pub fn to_persisted_toml(config: &CortexConfig) -> Result<String, toml::ser::Error> {
  #[derive(Serialize)]
  struct Persisted<'a> {
    database: &'a DatabaseConfig,
    dispatcher: &'a DispatcherConfig,
    assets: &'a AssetsConfig,
    webauthn: &'a WebauthnConfig,
  }
  toml::to_string_pretty(&Persisted {
    database: &config.database,
    dispatcher: &config.dispatcher,
    assets: &config.assets,
    webauthn: &config.webauthn,
  })
}

/// The path of the optional `cortex.toml` configuration file, read and written by both the loader
/// and the Settings write path. Overridable via the `CORTEX_CONFIG_FILE` environment variable.
pub fn config_file_path() -> std::path::PathBuf {
  std::env::var("CORTEX_CONFIG_FILE")
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|_| std::path::PathBuf::from("cortex.toml"))
}

/// Returns the process-wide, lazily-loaded configuration.
pub fn config() -> &'static CortexConfig {
  static CONFIG: LazyLock<CortexConfig> = LazyLock::new(CortexConfig::load);
  &CONFIG
}
