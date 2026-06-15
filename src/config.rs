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
  /// **Sink archive-writer pool size.** Number of background threads the sink fans the blocking
  /// `/data` result-archive writes out to (dispatcher rationalization phase 3, closes D-7). The
  /// sink's single ZMQ-PULL receive loop reads each result's frames and hands them — task, then
  /// streamed chunks, then a commit — to one of these writers, so *receiving* the next result is
  /// no longer hostage to the current one's slow QLC-RAID6 write + `cortex.log` parse. Per-task
  /// ordering is preserved (a task's frames go contiguously to one writer); fan-out is across
  /// *different* tasks. Memory stays O(chunk) per writer (chunks are streamed and dropped, never
  /// the whole archive resident) bounded by a small per-writer channel. Default **4** — a modest
  /// decoupling that suits a box co-resident with ~200 workers; raise toward host cores if the
  /// disk can absorb more concurrent writes. (1 is the floor; a single writer ≈ the legacy
  /// inline behavior but still off the receive loop.)
  pub sink_writers: usize,
  /// **Finalize batch coalescing — size threshold (N).** The finalize thread accumulates returned
  /// task reports and persists them to Postgres in **one** transaction per batch
  /// ([`crate::backend::Backend::mark_done`]), flushing when the batch reaches this many reports —
  /// or `finalize_flush_ms` elapses, whichever fires first. Larger N amortizes the DB round-trip
  /// harder under load (fewer, bigger writes) at the cost of more rows per transaction. It is a
  /// *ceiling* that mainly bites under burst/saturation; at steady-state load the time window
  /// usually flushes first. Unlike `queue_size`, N is **not** bound by
  /// `max_locks_per_transaction` (that limits *object* locks; `mark_done` takes only row locks),
  /// so it can be large. Default **1024** — the empirical throughput knee from
  /// `examples/dispatcher_bench.rs` (tasks/s rises to ~1024 then plateaus, and *regresses* by
  /// ~4096 where a single transaction holds row locks long enough to stall the pipeline; see
  /// `docs/DISPATCHER_BENCH.md`). 1024 also bounds worst-case crash re-work to ~1024 tasks.
  /// (Dispatcher rationalization phase 2, `docs/DISPATCHER_RATIONALIZATION.md`.)
  pub finalize_batch_size: usize,
  /// **Finalize batch coalescing — time threshold (T), milliseconds.** The maximum time a report
  /// waits in an accumulating batch before it is flushed, bounding both report staleness and
  /// worst-case crash **re-work**. An unflushed in-memory batch is never *lost* — its tasks stay
  /// `Queued` and are recovered on restart — so T trades a little latency for far fewer DB writes,
  /// not safety. At steady-state load this is usually the threshold that fires. Default 300 ms (at
  /// ~200 tasks/s it coalesces ~60 tasks per write instead of one write per task, for a few
  /// hundred ms of staleness).
  pub finalize_flush_ms: u64,
  /// **Lease / visibility timeout (seconds).** Base deadline for a dispatched (in-flight) task to
  /// return a result before the reaper re-leases it. The effective per-task deadline backs off
  /// with retries — `(retries + 1) × lease_timeout_seconds` from dispatch — so a task that keeps
  /// timing out waits progressively longer rather than re-leasing ever-faster
  /// ([`crate::helpers::TaskProgress::expected_at`]). This is the correctness net for a *silently
  /// dead / half-open* worker (no ZMTP heartbeat needed): its task is recovered once the lease
  /// lapses. Default **3600** (1 h) — deliberately generous, because a single hostile arXiv paper
  /// can take many minutes under `latexml`, and re-leasing a still-running task wastes compute.
  /// Shorten it only when worker runtimes are known-bounded (a fast `echo`/import service, or a
  /// chaos test driving fast reaper-recovery). Paired with `reap_interval_seconds` (how often the
  /// sweep runs).
  pub lease_timeout_seconds: i64,
  /// **Reaper sweep interval (seconds).** How often the ventilator scans the in-flight set for
  /// tasks past their `lease_timeout_seconds` deadline and re-leases / dead-letters them.
  /// Decoupled from the request path so the in-flight set drains even under sustained
  /// backpressure (KNOWN_ISSUES D-6). Kept well below the lease timeout so an expired task is
  /// recovered promptly without scanning the set on every request. Default **60** s. (Lowering
  /// both this and `lease_timeout_seconds` is what lets a fast chaos test exercise reaper-based
  /// recovery in seconds instead of the hour-scale production timing.)
  pub reap_interval_seconds: i64,
  /// **TCP keepalive idle (seconds) on the worker-facing ZMQ sockets** (ventilator + sink). After
  /// this many idle seconds the OS begins probing the peer; this both keeps idle worker
  /// connections alive across NAT/firewall idle-timeouts — essential when the ~200 remote
  /// workers reach the dispatcher over an overlay/VPN or any NAT'd path, where an idle mapping
  /// is otherwise silently dropped and the worker falls out of the fleet until it reconnects —
  /// and lets the OS reap a genuinely dead peer so the ROUTER doesn't accumulate stale routes.
  /// Task-recovery *correctness* does **not** depend on this (the lease reaper is that net, see
  /// `lease_timeout_seconds`); it keeps the *fleet connected*. `<= 0` leaves the OS keepalive
  /// default (effectively off). Default **120**, well under the common 5-minute NAT idle window.
  /// (Probe interval/count are fixed sane
  /// values in [`crate::dispatcher::server::apply_tcp_keepalive`].)
  pub tcp_keepalive_idle_seconds: i32,
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
      sink_writers: 4,
      finalize_batch_size: 1024,
      finalize_flush_ms: 300,
      lease_timeout_seconds: 3600,
      reap_interval_seconds: 60,
      tcp_keepalive_idle_seconds: 120,
    }
  }
}

/// Frontend authentication / secrets (formerly the hand-edited `config.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthConfig {
  /// Password-like tokens mapped to a human-readable owner. The bootstrap / break-glass + agent
  /// credential that gates every write action and the `/admin` sign-in, alongside passkeys (see
  /// [`WebauthnConfig`] and `docs/archive/AAA_DESIGN.md`). Set via `cortex set-admin-token`.
  pub rerun_tokens: HashMap<String, String>,
}

/// Passkey (**WebAuthn**) sign-in settings for the human admin UI
/// (`docs/archive/WEBAUTHN_DESIGN.md`). The relying party is the CorTeX server itself — no external
/// IdP, no per-deploy app registration. The admin token (see [`AuthConfig`]) remains the bootstrap
/// / break-glass path; passkeys are the convenient day-to-day human sign-in once enrolled.
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
    // Back-compat: the legacy frontend `config.json` (rerun_tokens) remains authoritative for the
    // auth section so running deployments keep working. The path defaults to `config.json` in the
    // working directory but is **overridable via `CORTEX_AUTH_FILE`** ([`auth_file_path`]) — so a
    // deployment keeps its live admin token in a file *outside the repo* (e.g.
    // `/etc/cortex/config.json`, root-owned), while the repo's `config.json` stays the demo/test
    // fixture and the real token is never checked into git. The new home for these values is the
    // `[auth]` section of `cortex.toml` / `CORTEX_AUTH__*` (written by `cortex set-admin-token`);
    // the prototype's `captcha_secret` is gone — bot protection is a deployment concern (an
    // Anubis reverse proxy), not framework code (docs/DEPLOYMENT.md).
    let auth_file = auth_file_path();
    if let Ok(text) = std::fs::read_to_string(&auth_file) {
      match serde_json::from_str::<LegacyFrontendConfig>(&text) {
        Ok(legacy) => config.auth.rerun_tokens = legacy.rerun_tokens,
        Err(e) => eprintln!("-- ignoring malformed {}: {e}", auth_file.display()),
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

/// The path of the **token file** — the JSON holding `rerun_tokens` (admin/agent credentials). It
/// is **gitignored** and scaffolded by `cortex init`; the tracked `config.default.json` is only a
/// template. Defaults to `config.json` in the working directory, **overridable via
/// `CORTEX_AUTH_FILE`** so a deployment keeps its live token *outside the repo* (e.g.
/// `/etc/cortex/config.json`, root-owned) and the repo copy stays the demo/test fixture — the live
/// secret is never checked in.
pub fn auth_file_path() -> std::path::PathBuf {
  std::env::var("CORTEX_AUTH_FILE")
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|_| std::path::PathBuf::from("config.json"))
}

/// Returns the process-wide, lazily-loaded configuration.
pub fn config() -> &'static CortexConfig {
  static CONFIG: LazyLock<CortexConfig> = LazyLock::new(CortexConfig::load);
  &CONFIG
}
