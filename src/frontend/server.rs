// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! The library-resident Rocket composition root.
//!
//! `server` assembles the per-capability route groups (`management`, and from Arm 5 onward
//! `corpora`, …), the shared fairings, and the managed state (config-file path + connection pool)
//! into a testable app that the binary and the integration tests both build. Route handlers live in
//! their capability modules; this file only wires them together. As later arms land, their routes
//! are mounted here too (the binary's legacy routes migrate in incrementally).

use std::path::PathBuf;

use rocket::{Build, Rocket};
use rocket_dyn_templates::Template;

use crate::backend::{build_pool, DbPool};
use crate::config::{config, config_file_path};
use crate::frontend::corpora;
use crate::frontend::management::{self, ConfigFile};

/// Mounts the full library API/UI surface, building the connection pool and resolving the config
/// file from the runtime configuration. This is the composition root the binary uses.
pub fn mount_api(rocket: Rocket<Build>) -> Rocket<Build> {
  let cfg = config();
  let pool = build_pool(&cfg.database.url, cfg.database.pool_size);
  mount_api_with(rocket, config_file_path(), pool)
}

/// Like [`mount_api`], but with an explicit config-file path and connection pool. Tests use this to
/// target the test database and a temporary config file.
pub fn mount_api_with(rocket: Rocket<Build>, config_file: PathBuf, pool: DbPool) -> Rocket<Build> {
  rocket
    .manage(ConfigFile(config_file))
    .manage(pool)
    .mount("/", management::routes())
    .mount("/", corpora::routes())
    .attach(Template::fairing())
}
