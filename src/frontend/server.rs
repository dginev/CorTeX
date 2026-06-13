// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Library-resident HTTP routes for the management & health surface, mounted by the frontend
//! binary and exercised directly by integration tests via `rocket::local`.
//!
//! Each capability follows the plan's "symmetry contract": one shared, serializable DTO renders as
//! JSON for agents here, and (in later increments) as HTML for humans — never two implementations.

use diesel::pg::PgConnection;
use diesel::prelude::*;
use rocket::serde::json::Json;
use rocket::{Build, Rocket};
use serde::Serialize;

use crate::config::{config, AssetsConfig, CacheConfig, CortexConfig, DispatcherConfig};

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
  /// Builds the masked DTO from the live configuration.
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

/// Structured health report, identical for agents and human supervisors.
#[derive(Debug, Serialize)]
pub struct HealthDto {
  /// Overall status: `"ok"` when every dependency is healthy, else `"degraded"`.
  pub status: &'static str,
  /// Database dependency health.
  pub database: DbHealth,
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

/// Returns whether the configured database accepts a connection and a trivial query.
fn database_reachable() -> bool {
  match PgConnection::establish(&config().database.url) {
    Ok(mut connection) => diesel::sql_query("SELECT 1")
      .execute(&mut connection)
      .is_ok(),
    Err(_) => false,
  }
}

/// The effective configuration, masked for safe exposure (agent twin of the Settings screen).
#[get("/api/config")]
pub fn api_config() -> Json<ConfigDto> { Json(ConfigDto::from_config(config())) }

/// A structured, pollable health report for humans and agents alike.
#[get("/healthz")]
pub fn healthz() -> Json<HealthDto> {
  let reachable = database_reachable();
  Json(HealthDto {
    status: if reachable { "ok" } else { "degraded" },
    database: DbHealth { reachable },
  })
}

/// Mounts the management and health routes onto the given Rocket instance.
pub fn mount_management(rocket: Rocket<Build>) -> Rocket<Build> {
  rocket.mount("/", routes![api_config, healthz])
}
