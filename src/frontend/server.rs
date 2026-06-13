// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! The library-resident Rocket composition root.
//!
//! `server` assembles capability route groups (currently [`crate::frontend::management`]),
//! fairings, and managed state into a testable app that the binary and the integration tests both
//! build. Route handlers live in their per-capability modules; this file only wires them together.
//! As later arms land, their routes are mounted here too (the binary's legacy routes migrate in
//! incrementally).

use std::path::PathBuf;

use rocket::{Build, Rocket};
use rocket_dyn_templates::Template;

use crate::backend::build_pool;
use crate::config::{config, config_file_path};
use crate::frontend::management::{self, ConfigFile};

/// Mounts the management/health/settings capability, persisting edits to the default config file.
pub fn mount_management(rocket: Rocket<Build>) -> Rocket<Build> {
  mount_management_with(rocket, config_file_path())
}

/// Like [`mount_management`], but with an explicit configuration-file path (used by tests).
pub fn mount_management_with(rocket: Rocket<Build>, config_file: PathBuf) -> Rocket<Build> {
  let cfg = config();
  rocket
    .manage(ConfigFile(config_file))
    .manage(build_pool(&cfg.database.url, cfg.database.pool_size))
    .mount("/", management::routes())
    .attach(Template::fairing())
}
