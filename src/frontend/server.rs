// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! The library-resident Rocket composition root.
//!
//! `server` assembles the per-capability route groups (`management`, `corpora`, `jobs`, …), the
//! shared fairings, and the managed state (config-file path, database URL, and connection pool)
//! into a testable app that the binary and the integration tests both build. Route handlers live in
//! their capability modules; this file only wires them together. As later arms land, their routes
//! are mounted here too (the binary's legacy routes migrate in incrementally).

use std::path::PathBuf;

use diesel::pg::PgConnection;
use diesel::Connection;
use rocket::{Build, Rocket};
use rocket_dyn_templates::Template;

use crate::backend::{build_pool, DatabaseUrl};
use crate::config::{config, config_file_path};
use crate::frontend::corpora;
use crate::frontend::jobs;
use crate::frontend::management::{self, ConfigFile};
use crate::frontend::reports;
use crate::frontend::runs;
use crate::frontend::services;

/// Mounts the full library API/UI surface from the runtime configuration. The composition root the
/// binary uses.
pub fn mount_api(rocket: Rocket<Build>) -> Rocket<Build> {
  let database_url = config().database.url.clone();
  // Best-effort: mark jobs left 'running' by a previous process as interrupted (prod startup only;
  // tests build via mount_api_with, so their in-flight jobs are never touched).
  if let Ok(mut connection) = PgConnection::establish(&database_url) {
    crate::jobs::interrupt_orphans(&mut connection);
  }
  mount_api_with(rocket, config_file_path(), &database_url)
}

/// Like [`mount_api`], but with an explicit config-file path and database URL (tests target the
/// test database and a temporary config file). Builds the connection pool and manages it alongside
/// the URL, so background jobs open their own connection against the same database.
pub fn mount_api_with(
  rocket: Rocket<Build>,
  config_file: PathBuf,
  database_url: &str,
) -> Rocket<Build> {
  let pool = build_pool(database_url, config().database.pool_size);
  let rocket = rocket
    .manage(ConfigFile(config_file))
    .manage(DatabaseUrl(database_url.to_string()))
    .manage(pool)
    // Passkey (WebAuthn) sign-in: the relying-party instance (`None` when disabled) + the in-memory
    // ceremony store. See `frontend::webauthn`.
    .manage(crate::frontend::webauthn::build_state(&config().webauthn))
    .manage(crate::frontend::webauthn::CeremonyStore::new())
    .mount("/", management::routes())
    .mount("/", corpora::routes())
    .mount("/", reports::routes())
    .mount("/", runs::routes())
    .mount("/", jobs::routes())
    .mount("/", services::routes())
    .mount("/", crate::frontend::admin::routes())
    .mount("/", crate::frontend::audit::routes())
    .mount("/", crate::frontend::sessions::routes())
    .mount("/", crate::frontend::metrics::routes())
    .mount("/", crate::frontend::webauthn::routes())
    .register("/", crate::frontend::catchers::catchers())
    .attach(Template::fairing())
    // Accounting (AAA): record every mutating admin request to the `audit_log` (drift-proof —
    // covers every write route, present and future). See `frontend::audit`.
    .attach(crate::frontend::audit::AuditFairing);
  // Mount the generated OpenAPI spec (`/api/openapi.json`), the `#[openapi]`-documented agent
  // routes, and the RapiDoc browser page (`/api/docs`) — built by rocket_okapi from the routes
  // themselves.
  let rocket = crate::frontend::apidoc::mount(rocket);
  // Snapshot the now-complete route table (legacy binary routes + all library routes) so the `/api`
  // discovery index introspects the real surface and can never drift.
  let route_table = management::RouteTable::snapshot(&rocket);
  rocket.manage(route_table)
}
