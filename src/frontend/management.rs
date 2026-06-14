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
use crate::config::{config, AssetsConfig, CortexConfig, DispatcherConfig};
use crate::frontend::actor::{owner_for_token, Actor};

/// Managed state: the path where the write path persists the configuration file.
pub struct ConfigFile(pub PathBuf);

/// Masked view of the database settings — the password is never exposed.
#[derive(Debug, Serialize)]
pub struct DatabaseDto {
  /// Connection URL with any password component replaced by `***`.
  pub url: String,
}

/// Masked view of the auth settings — secrets are summarized, never exposed.
#[derive(Debug, Serialize)]
pub struct AuthDto {
  /// Whether a captcha secret is configured.
  pub captcha_secret_set: bool,
  /// How many rerun tokens are configured.
  pub rerun_token_count: usize,
}

/// A masked, serializable view of [`CortexConfig`] safe to expose over the API and UI.
#[derive(Debug, Serialize)]
pub struct ConfigDto {
  /// Database settings (password masked).
  pub database: DatabaseDto,
  /// ZeroMQ dispatcher settings.
  pub dispatcher: DispatcherConfig,
  /// On-disk asset locations.
  pub assets: AssetsConfig,
  /// Auth settings (secrets masked).
  pub auth: AuthDto,
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
      auth: AuthDto {
        captcha_secret_set: !cfg.auth.captcha_secret.is_empty(),
        rerun_token_count: cfg.auth.rerun_tokens.len(),
      },
    }
  }
}

/// Health of the database dependency.
#[derive(Debug, Serialize)]
pub struct DbHealth {
  /// Whether the configured database accepts a connection and a trivial query.
  pub reachable: bool,
}

/// Health of the schema migrations.
#[derive(Debug, Serialize)]
pub struct MigrationsHealth {
  /// Whether the database schema is at the latest embedded migration.
  pub current: bool,
}

/// Utilization of the web frontend's database connection pool — a key load / saturation signal
/// (when `in_use` approaches `max`, requests start waiting on `pool.get()` and may `503`).
#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
pub struct DispatcherHealth {
  /// Whether both the ventilator and sink ports accept a TCP connection on localhost.
  pub reachable: bool,
  /// Ventilator (worker task-request) port.
  pub source_port: usize,
  /// Sink (worker result) port.
  pub result_port: usize,
}

/// Structured health report, identical for agents and human supervisors.
#[derive(Debug, Serialize)]
pub struct HealthDto {
  /// Overall status: `"ok"` when every *frontend* dependency (DB + migrations) is healthy, else
  /// `"degraded"`. Pool/dispatcher fields are informational and do not flip this.
  pub status: &'static str,
  /// Database dependency health.
  pub database: DbHealth,
  /// Schema-migration health.
  pub migrations: MigrationsHealth,
  /// Connection-pool utilization.
  pub pool: PoolHealth,
  /// Co-located dispatcher reachability (informational).
  pub dispatcher: DispatcherHealth,
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
  let toml_text =
    crate::config::to_persisted_toml(&merged).map_err(|_| Status::InternalServerError)?;
  std::fs::write(path, toml_text).map_err(|_| Status::InternalServerError)?;
  Ok(merged)
}

/// One row of the mounted route surface — an endpoint's method, URI pattern, and handler name.
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Serialize)]
pub struct ApiIndexDto {
  /// Number of agent endpoints.
  pub count: usize,
  /// The agent endpoints, sorted by path then method.
  pub endpoints: Vec<RouteInfo>,
}

/// `GET /api` — the agent-API discovery index (see [`ApiIndexDto`]).
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
    count: endpoints.len(),
    endpoints,
  })
}

/// The effective configuration, masked for safe exposure (the agent twin of the Settings screen).
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
/// Shared by the agent (`/healthz`) and human (`/health`) routes so both report identical state.
fn health_report(pool: &DbPool) -> HealthDto {
  let (reachable, migrations_current) = match pool.get() {
    Ok(mut connection) => {
      let reachable = diesel::sql_query("SELECT 1")
        .execute(&mut *connection)
        .is_ok();
      let migrations_current = !crate::migrations::has_pending_migrations(&mut connection);
      (reachable, migrations_current)
    },
    Err(_) => (false, false),
  };
  // Sample utilization after the probe connection is returned, so it reflects concurrent load.
  let state = pool.state();
  let status = if reachable && migrations_current {
    "ok"
  } else {
    "degraded"
  };
  HealthDto {
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
  }
}

/// A structured, pollable health report for agents (probes through the pool, samples pool
/// utilization). The JSON twin of the human [`health_page`] screen.
#[get("/healthz")]
pub fn healthz(pool: &State<DbPool>) -> Json<HealthDto> { Json(health_report(pool)) }

/// The human health screen: the HTML twin of `GET /healthz`, sharing [`HealthDto`] — database
/// reachability, migration currency, and live connection-pool utilization at a glance.
#[get("/health")]
pub fn health_page(pool: &State<DbPool>) -> Template {
  let health = health_report(pool);
  let global = serde_json::json!({
    "title": format!("System health — {}", health.status),
    "description": "CorTeX system health: database, schema migrations, connection pool.",
  });
  Template::render("health", context! { global, health })
}

/// The Settings page: the human (HTML) twin of `GET /api/config`.
#[get("/settings")]
pub fn settings() -> Template {
  Template::render(
    "settings",
    context! { config: ConfigDto::from_config(config()) },
  )
}

/// Agent write path: deep-merge a partial config patch, persist it, and return the masked result.
#[put("/api/config", format = "json", data = "<patch>")]
pub fn put_config(
  patch: Json<serde_json::Value>,
  config_file: &State<ConfigFile>,
) -> Result<Json<ConfigDto>, Status> {
  let merged = merge_and_persist(&patch.into_inner(), &config_file.0)?;
  Ok(Json(ConfigDto::from_config(&merged)))
}

/// Human write path: a native form POST from the Settings page; persists, then redirects back.
#[post("/settings", data = "<form>")]
pub fn post_settings(
  form: Form<SettingsForm>,
  config_file: &State<ConfigFile>,
) -> Result<Redirect, Status> {
  let f = form.into_inner();
  let patch = serde_json::json!({
    "dispatcher": {
      "source_port": f.dispatcher_source_port,
      "result_port": f.dispatcher_result_port,
      "queue_size": f.dispatcher_queue_size,
      "message_size": f.dispatcher_message_size,
      "max_in_flight": f.dispatcher_max_in_flight,
    },
    "assets": { "template_dir": f.assets_template_dir, "public_dir": f.assets_public_dir },
  });
  merge_and_persist(&patch, &config_file.0)?;
  Ok(Redirect::to("/settings"))
}

/// Acknowledgement for a maintenance job: the background [`crate::jobs`] handle to poll.
#[derive(Debug, Serialize)]
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

/// The token field of the human "Reindex database" form on the health screen.
#[derive(FromForm)]
pub struct MaintenanceForm {
  /// A rerun token, resolved to the acting owner.
  pub token: String,
}

/// The human twin of [`reindex`]: the health screen's "Reindex database now" button. Spawns the
/// same debounced reindex job and redirects to `/jobs` to watch it. `401` on a bad token.
#[post("/maintenance/reindex", data = "<form>")]
pub fn reindex_human(
  form: rocket::form::Form<MaintenanceForm>,
  pool: &State<DbPool>,
) -> Result<rocket::response::Redirect, Status> {
  let owner = owner_for_token(&form.token).ok_or(Status::Unauthorized)?;
  crate::jobs::spawn_reindex(pool.inner().clone(), &owner)
    .map_err(|_| Status::InternalServerError)?;
  Ok(rocket::response::Redirect::to("/jobs"))
}

/// The route set for the management/health/settings capability.
pub fn routes() -> Vec<Route> {
  routes![
    api_index,
    api_config,
    healthz,
    health_page,
    reindex,
    reindex_human,
    settings,
    put_config,
    post_settings
  ]
}
