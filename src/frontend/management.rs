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

use diesel::pg::PgConnection;
use diesel::prelude::*;
use figment::providers::Serialized;
use figment::Figment;
use rocket::form::Form;
use rocket::http::Status;
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::{context, Template};
use serde::Serialize;

use crate::config::{
  config, AssetsConfig, CacheConfig, CortexConfig, DatabaseConfig, DispatcherConfig,
};

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
  /// Report-cache settings.
  pub cache: CacheConfig,
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
      cache: cfg.cache.clone(),
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

/// Structured health report, identical for agents and human supervisors.
#[derive(Debug, Serialize)]
pub struct HealthDto {
  /// Overall status: `"ok"` when every dependency is healthy, else `"degraded"`.
  pub status: &'static str,
  /// Database dependency health.
  pub database: DbHealth,
  /// Schema-migration health.
  pub migrations: MigrationsHealth,
}

/// The subset of configuration that is safe to persist to the config file (excludes secrets).
#[derive(Serialize)]
struct PersistedConfig<'a> {
  database: &'a DatabaseConfig,
  dispatcher: &'a DispatcherConfig,
  cache: &'a CacheConfig,
  assets: &'a AssetsConfig,
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
  /// Redis cache URL.
  pub cache_redis_url: String,
  /// Whether the cache is required at boot.
  pub cache_required: bool,
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

/// Probes the database, returning `(reachable, migrations_current)` from a single connection.
fn diagnose_database() -> (bool, bool) {
  match PgConnection::establish(&config().database.url) {
    Ok(mut connection) => {
      let reachable = diesel::sql_query("SELECT 1")
        .execute(&mut connection)
        .is_ok();
      let migrations_current = !crate::migrations::has_pending_migrations(&mut connection);
      (reachable, migrations_current)
    },
    Err(_) => (false, false),
  }
}

/// Deep-merges a partial JSON patch onto the running config, validates it, and persists the
/// non-secret sections to `path` as TOML. Returns the merged configuration.
fn merge_and_persist(patch: &serde_json::Value, path: &Path) -> Result<CortexConfig, Status> {
  let merged: CortexConfig = Figment::from(Serialized::defaults(config().clone()))
    .merge(Serialized::defaults(patch))
    .extract()
    .map_err(|_| Status::UnprocessableEntity)?;
  let persisted = PersistedConfig {
    database: &merged.database,
    dispatcher: &merged.dispatcher,
    cache: &merged.cache,
    assets: &merged.assets,
  };
  let toml_text = toml::to_string_pretty(&persisted).map_err(|_| Status::InternalServerError)?;
  std::fs::write(path, toml_text).map_err(|_| Status::InternalServerError)?;
  Ok(merged)
}

/// The effective configuration, masked for safe exposure (the agent twin of the Settings screen).
#[get("/api/config")]
pub fn api_config() -> Json<ConfigDto> { Json(ConfigDto::from_config(config())) }

/// A structured, pollable health report for humans and agents alike.
#[get("/healthz")]
pub fn healthz() -> Json<HealthDto> {
  let (reachable, migrations_current) = diagnose_database();
  let status = if reachable && migrations_current {
    "ok"
  } else {
    "degraded"
  };
  Json(HealthDto {
    status,
    database: DbHealth { reachable },
    migrations: MigrationsHealth {
      current: migrations_current,
    },
  })
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
    },
    "cache": { "redis_url": f.cache_redis_url, "required": f.cache_required },
    "assets": { "template_dir": f.assets_template_dir, "public_dir": f.assets_public_dir },
  });
  merge_and_persist(&patch, &config_file.0)?;
  Ok(Redirect::to("/settings"))
}

/// The route set for the management/health/settings capability.
pub fn routes() -> Vec<Route> { routes![api_config, healthz, settings, put_config, post_settings] }
