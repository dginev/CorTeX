// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Management & health capability: the configuration view/edit surface, the Settings page, and a
//! health check.
//!
//! Follows the plan's "symmetry contract": one shared, serializable DTO renders as JSON for agents
//! (`GET /api/config`) and as HTML for humans (`GET /settings`), and the write path
//! (`PUT /api/config`, `POST /settings`) shares one merge-and-persist function. Handlers live here;
//! the app is assembled in [`crate::frontend::server`].

use std::path::Path;
use std::path::PathBuf;

use diesel::prelude::*;
use figment::providers::Serialized;
use figment::Figment;
use rocket::form::Form;
use rocket::http::Status;
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Build, Rocket, Route, State};
use rocket_dyn_templates::{context, Template};
use serde::Serialize;

use crate::backend::DbPool;
use crate::config::{config, AssetsConfig, CortexConfig, DispatcherConfig, JobsConfig};
use crate::frontend::actor::{
  require_admin, require_admin_to, Actor, AdminReject, AdminSession, ReturnTo,
};

/// Managed state: the path where the write path persists the configuration file.
pub struct ConfigFile(pub PathBuf);

/// Masked view of the database settings — the password is never exposed.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DatabaseDto {
  /// Connection URL with any password component replaced by `***`.
  pub url: String,
}

/// Masked view of the auth settings — secrets are summarized, never exposed.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct AuthDto {
  /// How many rerun/admin tokens are configured.
  pub rerun_token_count: usize,
}

/// A masked, serializable view of [`CortexConfig`] safe to expose over the API and UI.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ConfigDto {
  /// Database settings (password masked).
  pub database: DatabaseDto,
  /// ZeroMQ dispatcher settings.
  pub dispatcher: DispatcherConfig,
  /// On-disk asset locations.
  pub assets: AssetsConfig,
  /// Background-job lifecycle settings (the stall-reap threshold).
  pub jobs: JobsConfig,
  /// Auth settings (secrets masked).
  pub auth: AuthDto,
  /// Passkey (WebAuthn) sign-in settings (non-secret: enabled flag + relying-party id/origin).
  pub webauthn: crate::config::WebauthnConfig,
}

impl ConfigDto {
  /// Builds the masked DTO from a configuration.
  pub fn from_config(cfg: &CortexConfig) -> ConfigDto {
    ConfigDto {
      database: DatabaseDto {
        url: mask_db_password(&cfg.database.url),
      },
      dispatcher: cfg.dispatcher.clone(),
      assets: cfg.assets.clone(),
      jobs: cfg.jobs.clone(),
      auth: AuthDto {
        rerun_token_count: cfg.auth.rerun_tokens.len(),
      },
      webauthn: cfg.webauthn.clone(),
    }
  }
}

/// Health of the database dependency.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DbHealth {
  /// Whether the configured database accepts a connection and a trivial query.
  pub reachable: bool,
}

/// Health of the schema migrations.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MigrationsHealth {
  /// Whether the database schema is at the latest embedded migration.
  pub current: bool,
}

/// Utilization of the web frontend's database connection pool — a key load / saturation signal
/// (when `in_use` approaches `max`, requests start waiting on `pool.get()` and may `503`).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PoolHealth {
  /// Configured maximum pool size (`database.pool_size`).
  pub max: u32,
  /// Connections currently established (idle + in-use).
  pub connections: u32,
  /// Idle, immediately-available connections.
  pub idle: u32,
  /// Connections currently checked out (in use).
  pub in_use: u32,
}

/// Reachability of the ZeroMQ dispatcher, probed by a short TCP connect to its bound ports. The
/// frontend doesn't otherwise speak to the dispatcher (workers do), so this is a pure liveness
/// probe of the **co-located** dispatcher (localhost) — informational, it does not flip the overall
/// `status` (a read-only/report-only frontend deployment legitimately runs without a dispatcher).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DispatcherHealth {
  /// Whether both the ventilator and sink ports accept a TCP connection on localhost.
  pub reachable: bool,
  /// Ventilator (worker task-request) port.
  pub source_port: usize,
  /// Sink (worker result) port.
  pub result_port: usize,
}

/// A corpus whose configured source directory could not be read on disk (missing / unmounted /
/// wrong permissions). Its conversions and re-imports will fail until the path is restored.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct UnreadableCorpus {
  /// Corpus name (its external handle).
  pub name: String,
  /// The configured source path that is missing or unreadable.
  pub path: String,
}

/// Health of the shared document storage: every corpus's `path` is stat-checked on disk. Document
/// bytes live on a shared filesystem (`tasks.entry` are absolute paths under each `corpus.path`),
/// so a moved/unmounted data mount makes the whole conversion pipeline fail — surfaced here instead
/// of only as mysterious cascading task failures. **Informational** (the frontend still serves
/// reports from the DB), so it does not flip the overall `status`. Corpora with an empty path are
/// skipped.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StorageHealth {
  /// Number of corpora whose source path was checked (non-empty paths).
  pub corpora_checked: usize,
  /// Corpora whose source directory is missing or unreadable (empty = all good).
  pub unreadable: Vec<UnreadableCorpus>,
}

/// Structured health report, identical for agents and human supervisors.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct HealthDto {
  /// Overall status: `"ok"` when every *frontend* dependency (DB + migrations) is healthy, else
  /// `"degraded"`. Pool/dispatcher/storage fields are informational and do not flip this.
  pub status: &'static str,
  /// Database dependency health.
  pub database: DbHealth,
  /// Schema-migration health.
  pub migrations: MigrationsHealth,
  /// Connection-pool utilization.
  pub pool: PoolHealth,
  /// Co-located dispatcher reachability (informational).
  pub dispatcher: DispatcherHealth,
  /// Shared document-storage reachability per corpus (informational).
  pub storage: StorageHealth,
  /// Actionable operator guidance for every degraded or warning signal above, in fix-this-first
  /// order (empty when all-clear). The runtime twin of `cortex doctor`'s remediation hints — so an
  /// operator (or agent) polling health is told *how* to fix a red/amber signal, not just that it
  /// is one. Computed from the fields above by [`HealthDto::remediations`].
  pub remediations: Vec<String>,
}

/// Minimal, **public** liveness projection — the open `GET /healthz`. Just whether the service is
/// up and its database reachable; deliberately omits the internal topology (corpus paths, pool
/// sizing, dispatcher ports, remediations) that [`HealthDto`] exposes. The detailed report is
/// admin-only: the `/health` screen and its token-gated agent twin `GET /api/health` (KNOWN_ISSUES
/// X-1).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct LivenessDto {
  /// `"ok"` when the database is reachable, else `"degraded"`.
  pub status: &'static str,
  /// Database dependency health.
  pub database: DbHealth,
}

impl HealthDto {
  /// Builds the actionable remediation hints from the report's signals (reads every field *except*
  /// `remediations` itself, so it can populate that field). Fix-this-first ordered: a down database
  /// degrades the frontend and makes the migration check unknowable, so it is surfaced alone;
  /// otherwise pending migrations, then the informational pool / dispatcher / storage warnings.
  #[must_use]
  pub fn remediations(&self) -> Vec<String> {
    let mut hints = Vec::new();
    if !self.database.reachable {
      hints.push(
        "database unreachable — the frontend is degraded; check the database URL (`cortex.toml` \
         [database].url or DATABASE_URL) and that PostgreSQL is running"
          .to_string(),
      );
    } else if !self.migrations.current {
      hints
        .push("schema out of date — run `cortex init` to apply the pending migrations".to_string());
    }
    // Pool exhaustion: with every connection checked out, new requests block on `pool.get()` and
    // may time out to 503. Surface it before it cascades.
    if self.pool.in_use >= self.pool.max {
      hints.push(format!(
        "connection pool exhausted ({}/{} in use) — requests are waiting and may 503; raise \
         `database.pool_size` or investigate slow / long-held queries",
        self.pool.in_use, self.pool.max
      ));
    }
    if !self.dispatcher.reachable {
      hints.push(format!(
        "dispatcher not listening on localhost:{}/{} — if this node runs conversions, start the \
         dispatcher; a report-only frontend can ignore this",
        self.dispatcher.source_port, self.dispatcher.result_port
      ));
    }
    if !self.storage.unreadable.is_empty() {
      let names: Vec<&str> = self
        .storage
        .unreadable
        .iter()
        .map(|corpus| corpus.name.as_str())
        .collect();
      hints.push(format!(
        "{} corpus source path(s) unreadable ({}) — check the mount / permissions; conversions and \
         re-imports for them fail until restored",
        self.storage.unreadable.len(),
        names.join(", ")
      ));
    }
    hints
  }
}

/// The editable, non-secret fields of the Settings form (database/auth are edited out-of-band).
#[derive(FromForm)]
pub struct SettingsForm {
  /// Ventilator port.
  pub dispatcher_source_port: usize,
  /// Sink port.
  pub dispatcher_result_port: usize,
  /// Task batch size.
  pub dispatcher_queue_size: usize,
  /// ZeroMQ chunk size, in bytes.
  pub dispatcher_message_size: usize,
  /// Backpressure threshold: max in-flight tasks before the ventilator stops leasing.
  pub dispatcher_max_in_flight: usize,
  /// Hard cap (bytes) on a single worker result archive; an oversized result is rejected + the
  /// task marked `Invalid` so a runaway worker can't fill `/data` (W-1③).
  pub dispatcher_max_result_bytes: usize,
  /// How often (seconds) the finalize thread refreshes the report rollup (the freshness
  /// guarantee).
  pub dispatcher_report_refresh_interval_seconds: u64,
  /// Job stall-reap threshold (seconds): a non-terminal job silent this long is reaped as hung.
  pub jobs_stale_timeout_seconds: i64,
  /// Template directory.
  pub assets_template_dir: String,
  /// Public assets directory.
  pub assets_public_dir: String,
}

/// Replaces the password component of a `scheme://user:pass@host/db` URL with `***`.
fn mask_db_password(url: &str) -> String {
  if let (Some(scheme_end), Some(at)) = (url.find("://"), url.find('@')) {
    let creds = &url[scheme_end + 3..at];
    if let Some(colon) = creds.find(':') {
      let user = &creds[..colon];
      return format!("{}://{}:***{}", &url[..scheme_end], user, &url[at..]);
    }
  }
  url.to_string()
}

/// Deep-merges a partial JSON patch onto the running config, validates it, and persists the
/// non-secret sections to `path` as TOML. Returns the merged configuration.
fn merge_and_persist(patch: &serde_json::Value, path: &Path) -> Result<CortexConfig, Status> {
  let merged: CortexConfig = Figment::from(Serialized::defaults(config().clone()))
    .merge(Serialized::defaults(patch))
    .extract()
    .map_err(|_| Status::UnprocessableEntity)?;
  // The figment extract above only checks *types*. Reject a structurally-valid but operationally
  // **bricking** value (a zero pool/queue, an out-of-range port) BEFORE persisting — otherwise it
  // is written to disk and silently breaks the next restart.
  if let Err(reason) = validate_config_bounds(&merged) {
    tracing::warn!(%reason, "rejected a config update that would brick a component");
    return Err(Status::UnprocessableEntity);
  }
  let toml_text =
    crate::config::to_persisted_toml(&merged).map_err(|_| Status::InternalServerError)?;
  std::fs::write(path, toml_text).map_err(|_| Status::InternalServerError)?;
  Ok(merged)
}

/// Value-bound check for a merged config: rejects knobs whose value is structurally valid (the
/// right type) but would make a component unusable — the kind of footgun an admin could otherwise
/// persist from the Settings screen / `PUT /api/config` and only discover on the next (broken)
/// restart. The returned reason is logged; the write path maps it to `422`.
fn validate_config_bounds(c: &CortexConfig) -> Result<(), String> {
  if c.database.pool_size < 1 {
    return Err("database.pool_size must be >= 1 (a zero pool blocks every request)".to_string());
  }
  let port_ok = |port: usize| (1..=65535).contains(&port);
  if !port_ok(c.dispatcher.source_port) {
    return Err(format!(
      "dispatcher.source_port {} is out of range (1..=65535)",
      c.dispatcher.source_port
    ));
  }
  if !port_ok(c.dispatcher.result_port) {
    return Err(format!(
      "dispatcher.result_port {} is out of range (1..=65535)",
      c.dispatcher.result_port
    ));
  }
  if c.dispatcher.source_port == c.dispatcher.result_port {
    return Err(
      "dispatcher.source_port and result_port must differ (the ventilator and sink can't \
                share a port)"
        .to_string(),
    );
  }
  if c.dispatcher.queue_size < 1 {
    return Err("dispatcher.queue_size must be >= 1 (a zero queue leases no work)".to_string());
  }
  if c.dispatcher.max_in_flight < 1 {
    return Err("dispatcher.max_in_flight must be >= 1".to_string());
  }
  if c.dispatcher.max_result_bytes < 1 {
    return Err(
      "dispatcher.max_result_bytes must be >= 1 (a zero cap rejects every result)".to_string(),
    );
  }
  if c.jobs.stale_timeout_seconds < 1 {
    return Err(
      "jobs.stale_timeout_seconds must be >= 1 (a non-positive timeout reaps every job \
                instantly)"
        .to_string(),
    );
  }
  Ok(())
}

/// One row of the mounted route surface — an endpoint's method, URI pattern, and handler name.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct RouteInfo {
  /// HTTP method (`GET`, `POST`, …).
  pub method: String,
  /// URI pattern, with `<param>` placeholders and any `?<query>` parameters.
  pub uri: String,
  /// The handler's name (the Rust fn) — a hint at the operation.
  pub name: Option<String>,
}

/// A snapshot of the mounted route table, captured at mount time so the discovery index can never
/// drift from the routes actually served.
pub struct RouteTable(pub Vec<RouteInfo>);
impl RouteTable {
  /// Introspects a built Rocket's mounted routes into a serializable snapshot.
  pub fn snapshot(rocket: &Rocket<Build>) -> RouteTable {
    RouteTable(
      rocket
        .routes()
        .map(|route| RouteInfo {
          method: route.method.to_string(),
          uri: route.uri.to_string(),
          name: route.name.as_deref().map(str::to_string),
        })
        .collect(),
    )
  }
}

/// Discovery index of the **agent API**: every mounted `/api/*` endpoint (method, path, handler
/// name) in a single call, so an agent can enumerate CorTeX's machine surface without out-of-band
/// docs. Self-describing — built by introspecting the live route table, so it never drifts.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ApiIndexDto {
  /// One-line orientation for an agent landing on the API root.
  pub description: &'static str,
  /// Path to the full machine-readable OpenAPI 3 specification (typed request/response schemas for
  /// every endpoint) — the authoritative contract behind this lightweight index.
  pub openapi: &'static str,
  /// Path to the human-browsable API reference (RapiDoc, rendered from the same OpenAPI spec).
  pub docs: &'static str,
  /// Number of agent endpoints.
  pub count: usize,
  /// The agent endpoints, sorted by path then method.
  pub endpoints: Vec<RouteInfo>,
}

/// `GET /api` — the agent-API discovery index (see [`ApiIndexDto`]).
#[rocket_okapi::openapi(tag = "Meta")]
#[get("/api")]
pub fn api_index(routes: &State<RouteTable>) -> Json<ApiIndexDto> {
  let mut endpoints: Vec<RouteInfo> = routes
    .0
    .iter()
    .filter(|r| r.uri == "/api" || r.uri.starts_with("/api/"))
    .cloned()
    .collect();
  endpoints.sort_by(|a, b| a.uri.cmp(&b.uri).then_with(|| a.method.cmp(&b.method)));
  Json(ApiIndexDto {
    description: "CorTeX agent API. Enumerate endpoints below; see `openapi` for the full typed \
                 contract. Most reads are open; mutations require an X-Cortex-Token header.",
    openapi: "/api/openapi.json",
    docs: "/api/docs",
    count: endpoints.len(),
    endpoints,
  })
}

/// The effective configuration, masked for safe exposure (the agent twin of the Settings screen).
#[rocket_okapi::openapi(tag = "Management")]
#[get("/api/config")]
pub fn api_config() -> Json<ConfigDto> { Json(ConfigDto::from_config(config())) }

/// Whether a TCP connection to `127.0.0.1:port` succeeds within a short timeout — a liveness probe
/// of a ZeroMQ socket bound by the dispatcher (ZMQ `tcp://` sockets are TCP listeners). A closed
/// port returns "connection refused" immediately; the timeout only bounds the rare filtered-port
/// hang, so this stays fast on the common (co-located) path.
fn port_listening(port: usize) -> bool {
  use std::net::{TcpStream, ToSocketAddrs};
  ("127.0.0.1", port as u16)
    .to_socket_addrs()
    .ok()
    .and_then(|mut addrs| addrs.next())
    .is_some_and(|addr| {
      TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)).is_ok()
    })
}

/// Builds the health report: probes the database through the pool (so reachability reflects the
/// same path requests use), samples pool utilization, and probes the co-located dispatcher's ports.
/// Shared by the agent (`/api/health`) and human (`/health`) routes so both report identical state.
fn health_report(pool: &DbPool) -> HealthDto {
  let (reachable, migrations_current, corpora_paths) = match pool.get() {
    Ok(mut connection) => {
      use crate::schema::corpora;
      let reachable = diesel::sql_query("SELECT 1")
        .execute(&mut *connection)
        .is_ok();
      let migrations_current = !crate::migrations::has_pending_migrations(&mut connection);
      // Gather the (name, path) pairs *inside* the checkout; the disk stat happens after the
      // connection is returned, so we don't hold a pooled connection during filesystem I/O.
      let corpora_paths: Vec<(String, String)> = corpora::table
        .select((corpora::name, corpora::path))
        .load(&mut connection)
        .unwrap_or_default();
      (reachable, migrations_current, corpora_paths)
    },
    Err(_) => (false, false, Vec::new()),
  };
  // Stat each corpus source path (local shared storage; a plain existence check, no read_dir). A
  // corpus with an empty path has no configured location, so it is not a storage fault.
  let corpora_checked = corpora_paths
    .iter()
    .filter(|(_, path)| !path.is_empty())
    .count();
  let unreadable: Vec<UnreadableCorpus> = corpora_paths
    .into_iter()
    .filter(|(_, path)| !path.is_empty() && !Path::new(path).is_dir())
    .map(|(name, path)| UnreadableCorpus { name, path })
    .collect();
  // Sample utilization after the probe connection is returned, so it reflects concurrent load.
  let state = pool.state();
  let status = if reachable && migrations_current {
    "ok"
  } else {
    "degraded"
  };
  let mut report = HealthDto {
    status,
    database: DbHealth { reachable },
    migrations: MigrationsHealth {
      current: migrations_current,
    },
    pool: PoolHealth {
      max: pool.max_size(),
      connections: state.connections,
      idle: state.idle_connections,
      in_use: state.connections.saturating_sub(state.idle_connections),
    },
    dispatcher: {
      let dispatcher = &config().dispatcher;
      DispatcherHealth {
        reachable: port_listening(dispatcher.source_port) && port_listening(dispatcher.result_port),
        source_port: dispatcher.source_port,
        result_port: dispatcher.result_port,
      }
    },
    storage: StorageHealth {
      corpora_checked,
      unreadable,
    },
    remediations: Vec::new(),
  };
  // Derive the operator guidance from the assembled signals.
  report.remediations = report.remediations();
  report
}

/// A cheap liveness probe: a single pooled `SELECT 1`, nothing else (no corpus stat / port probes),
/// so the **public** endpoint stays O(1) and leaks no internal structure.
fn liveness_report(pool: &DbPool) -> LivenessDto {
  let reachable = pool
    .get()
    .map(|mut connection| {
      diesel::sql_query("SELECT 1")
        .execute(&mut *connection)
        .is_ok()
    })
    .unwrap_or(false);
  LivenessDto {
    status: if reachable { "ok" } else { "degraded" },
    database: DbHealth { reachable },
  }
}

/// Public liveness probe — minimal by design (KNOWN_ISSUES X-1): `{status, database.reachable}`
/// only, safe to expose unauthenticated at the edge for load balancers and agents. The *detailed*
/// report (pool, dispatcher ports, corpus storage, remediations) is admin-only: the `/health`
/// screen and its token-gated agent twin [`api_health`] (`GET /api/health`).
#[rocket_okapi::openapi(tag = "Management")]
#[get("/healthz")]
pub fn healthz(pool: &State<DbPool>) -> Json<LivenessDto> { Json(liveness_report(pool)) }

/// Detailed health report for agents — the **token-gated** JSON twin of the admin [`health_page`]
/// screen (sharing [`HealthDto`]). Gated by the [`Actor`] guard (clean `401` without a token) so
/// the internal topology it exposes (corpus paths, pool sizing, dispatcher ports) isn't
/// world-readable like the open `/healthz` once was (KNOWN_ISSUES X-1).
#[rocket_okapi::openapi(tag = "Management")]
#[get("/api/health")]
pub fn api_health(_caller: Actor, pool: &State<DbPool>) -> Json<HealthDto> {
  Json(health_report(pool))
}

/// The human health screen: the HTML twin of `GET /api/health`, sharing [`HealthDto`] — database
/// reachability, migration currency, and live connection-pool utilization at a glance. **Signed-in
/// admins only** (unauthenticated → sign-in page); the public `/healthz` JSON probe stays open for
/// liveness, but the detailed view is admin/token-gated.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/health")]
pub fn health_page(
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
  let health = health_report(pool);
  let global = serde_json::json!({
    "title": format!("System health — {}", health.status),
    "description": "CorTeX system health: database, schema migrations, connection pool.",
  });
  Ok(Template::render("health", context! { global, health }))
}

/// The Settings page: the human (HTML) twin of `GET /api/config`. **Signed-in admins only**
/// (unauthenticated → sign-in page); the agent twin keeps the token guard.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/settings?<saved>")]
pub fn settings(
  saved: Option<bool>,
  session: Option<AdminSession>,
  return_to: ReturnTo,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
  let global = serde_json::json!({
    "title": "Configuration",
    "description": "CorTeX framework configuration",
  });
  // `?saved=true` (set by the post-redirect-get after a successful save) flashes a confirmation, so
  // the admin gets feedback on the write instead of a silent reload — the same pattern as the
  // retention screen's `?pruned`.
  Ok(Template::render(
    "settings",
    context! { global, config: ConfigDto::from_config(config()), saved: saved.unwrap_or(false) },
  ))
}

/// Agent write path: deep-merge a partial config patch, persist it, and return the masked result.
/// **Token-gated** via the [`Actor`] guard — rewriting the running configuration (dispatcher ports,
/// queue/result sizes, asset dirs, the job stall threshold) is a consequential mutation, so it
/// requires a valid `X-Cortex-Token` exactly like every other agent write (the human twin
/// `post_settings` is `AdminSession`-gated). `401` without a token.
#[rocket_okapi::openapi(tag = "Management")]
#[put("/api/config", format = "json", data = "<patch>")]
pub fn put_config(
  patch: Json<serde_json::Value>,
  actor: Actor,
  config_file: &State<ConfigFile>,
) -> Result<Json<ConfigDto>, Status> {
  let patch = patch.into_inner();
  // The changed top-level sections (keys only — never the values, which can carry secrets) for the
  // operational journal; the audit fairing records the full action + actor to the DB.
  let sections: Vec<&str> = patch
    .as_object()
    .map(|o| o.keys().map(String::as_str).collect())
    .unwrap_or_default();
  let merged = merge_and_persist(&patch, &config_file.0)?;
  tracing::info!(actor = %actor.owner, sections = ?sections, "config updated via API");
  Ok(Json(ConfigDto::from_config(&merged)))
}

/// Human write path: a native form POST from the Settings page; persists, then redirects back.
/// **Gated by the signed-in [`AdminSession`] cookie** (the Settings screen is signed-in-only;
/// anonymous → sign-in).
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[post("/settings", data = "<form>")]
pub fn post_settings(
  form: Form<SettingsForm>,
  session: Option<AdminSession>,
  config_file: &State<ConfigFile>,
) -> Result<Redirect, AdminReject> {
  let _session = require_admin(session)?;
  let f = form.into_inner();
  let patch = serde_json::json!({
    "dispatcher": {
      "source_port": f.dispatcher_source_port,
      "result_port": f.dispatcher_result_port,
      "queue_size": f.dispatcher_queue_size,
      "message_size": f.dispatcher_message_size,
      "max_in_flight": f.dispatcher_max_in_flight,
      "max_result_bytes": f.dispatcher_max_result_bytes,
      "report_refresh_interval_seconds": f.dispatcher_report_refresh_interval_seconds,
    },
    "jobs": { "stale_timeout_seconds": f.jobs_stale_timeout_seconds },
    "assets": { "template_dir": f.assets_template_dir, "public_dir": f.assets_public_dir },
  });
  merge_and_persist(&patch, &config_file.0)?;
  Ok(Redirect::to("/settings?saved=true"))
}

/// Acknowledgement for a maintenance job: the background [`crate::jobs`] handle to poll.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MaintenanceAckDto {
  /// The spawned (or already-running, if debounced) maintenance job's external uuid.
  pub job: String,
  /// Where to poll the job's status / health / per-table progress.
  pub poll: String,
  /// The token-resolved actor recorded as the job's initiator.
  pub actor: String,
}

/// Triggers an **online** index rebuild (`REINDEX (CONCURRENTLY)` over the high-churn tables) as a
/// background job — index bloat slows scans over time, and this rebuilds without an exclusive lock
/// (DB ongoing-maintenance; `docs/DB_TUNING.md`). **Token-gated**; returns `202` + the job handle,
/// poll `GET /api/jobs/<job>` for per-table progress. Debounced.
#[rocket_okapi::openapi(tag = "Management")]
#[post("/api/maintenance/reindex")]
pub fn reindex(
  actor: Actor,
  pool: &State<DbPool>,
) -> Result<(Status, Json<MaintenanceAckDto>), Status> {
  let job_uuid = crate::jobs::spawn_reindex(pool.inner().clone(), &actor.owner)
    .map_err(|_| Status::InternalServerError)?;
  Ok((
    Status::Accepted,
    Json(MaintenanceAckDto {
      job: job_uuid.to_string(),
      poll: format!("/api/jobs/{job_uuid}"),
      actor: actor.owner,
    }),
  ))
}

/// The human twin of [`reindex`]: the health screen's "Reindex database now" button. **Gated by the
/// signed-in [`AdminSession`] cookie** (the health screen is itself signed-in-only; anonymous →
/// sign-in). Spawns the same debounced reindex job and redirects to `/jobs`.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[post("/maintenance/reindex")]
pub fn reindex_human(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<rocket::response::Redirect, AdminReject> {
  let session = require_admin(session)?;
  let uuid = crate::jobs::spawn_reindex(pool.inner().clone(), &session.owner)
    .map_err(|_| Status::InternalServerError)?;
  Ok(rocket::response::Redirect::to(format!("/jobs/{uuid}")))
}

/// Triggers a planner-statistics refresh (`ANALYZE` over the high-churn tables) as a background job
/// — keeps the planner's row estimates current after bulk imports/reruns so it keeps choosing the
/// right indexes (e.g. the TODO leasing index) instead of waiting for autovacuum (DB
/// ongoing-maintenance; `docs/DB_TUNING.md`). **Token-gated**; returns `202` + the job handle, poll
/// `GET /api/jobs/<job>` for per-table progress. Debounced.
#[rocket_okapi::openapi(tag = "Management")]
#[post("/api/maintenance/analyze")]
pub fn analyze(
  actor: Actor,
  pool: &State<DbPool>,
) -> Result<(Status, Json<MaintenanceAckDto>), Status> {
  let job_uuid = crate::jobs::spawn_analyze(pool.inner().clone(), &actor.owner)
    .map_err(|_| Status::InternalServerError)?;
  Ok((
    Status::Accepted,
    Json(MaintenanceAckDto {
      job: job_uuid.to_string(),
      poll: format!("/api/jobs/{job_uuid}"),
      actor: actor.owner,
    }),
  ))
}

/// The human twin of [`analyze`]: the health screen's "Refresh planner statistics" button. **Gated
/// by the signed-in [`AdminSession`] cookie** (anonymous → sign-in). Spawns the same debounced
/// analyze job and redirects to `/jobs`.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[post("/maintenance/analyze")]
pub fn analyze_human(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<rocket::response::Redirect, AdminReject> {
  let session = require_admin(session)?;
  let uuid = crate::jobs::spawn_analyze(pool.inner().clone(), &session.owner)
    .map_err(|_| Status::InternalServerError)?;
  Ok(rocket::response::Redirect::to(format!("/jobs/{uuid}")))
}

/// The route set for the management/health/settings capability.
pub fn routes() -> Vec<Route> {
  // NB: the agent management routes (`api_index`, `api_config`, `healthz`, `put_config`, `reindex`,
  // `analyze`) are mounted via `frontend::apidoc` (rocket_okapi).
  routes![
    health_page,
    reindex_human,
    analyze_human,
    settings,
    post_settings
  ]
}

#[cfg(test)]
mod tests {
  use super::*;

  /// An all-clear report; tests mutate one signal at a time off this baseline.
  fn healthy() -> HealthDto {
    HealthDto {
      status: "ok",
      database: DbHealth { reachable: true },
      migrations: MigrationsHealth { current: true },
      pool: PoolHealth {
        max: 32,
        connections: 4,
        idle: 4,
        in_use: 0,
      },
      dispatcher: DispatcherHealth {
        reachable: true,
        source_port: 51695,
        result_port: 51696,
      },
      storage: StorageHealth {
        corpora_checked: 2,
        unreadable: Vec::new(),
      },
      remediations: Vec::new(),
    }
  }

  #[test]
  fn healthy_report_has_no_remediations() {
    assert!(
      healthy().remediations().is_empty(),
      "an all-clear report needs no actions"
    );
  }

  #[test]
  fn db_down_surfaces_only_the_db_fix() {
    let mut health = healthy();
    health.database.reachable = false;
    health.migrations.current = false; // a consequence of the down DB — must not add its own hint
    let hints = health.remediations();
    assert_eq!(hints.len(), 1, "a down DB surfaces only the DB fix");
    assert!(
      hints[0].contains("database") && hints[0].contains("DATABASE_URL"),
      "the DB hint names the URL to check"
    );
  }

  #[test]
  fn pending_migrations_point_at_init() {
    let mut health = healthy();
    health.migrations.current = false;
    assert!(
      health
        .remediations()
        .iter()
        .any(|h| h.contains("cortex init")),
      "pending migrations point at cortex init"
    );
  }

  #[test]
  fn pool_exhaustion_is_flagged_with_counts() {
    let mut health = healthy();
    health.pool.in_use = health.pool.max; // every connection checked out
    assert!(
      health
        .remediations()
        .iter()
        .any(|h| h.contains("pool exhausted") && h.contains("32/32")),
      "an exhausted pool is flagged with its in-use/max counts"
    );
  }

  #[test]
  fn dispatcher_and_storage_warnings_are_actionable() {
    let mut health = healthy();
    health.dispatcher.reachable = false;
    health.storage.unreadable = vec![UnreadableCorpus {
      name: "arxiv".to_string(),
      path: "/data/arxiv".to_string(),
    }];
    let hints = health.remediations();
    assert!(
      hints.iter().any(|h| h.contains("dispatcher not listening")),
      "an unreachable dispatcher is actionable"
    );
    assert!(
      hints
        .iter()
        .any(|h| h.contains("unreadable") && h.contains("arxiv")),
      "an unreadable corpus path is named in the hint"
    );
  }
}
