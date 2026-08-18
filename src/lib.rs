// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! A general purpose processing framework for corpora of scientific documents

#![doc(html_root_url = "https://dginev.github.io/CorTeX/")]
#![doc(
  html_logo_url = "https://raw.githubusercontent.com/dginev/CorTeX/main/public/img/logo-icon.png"
)]
#![deny(missing_docs)]
#![recursion_limit = "256"]
#![allow(clippy::implicit_hasher)]
// TEMPORARY (POSSIBLE_UPGRADES E-5): the floating `nightly` clippy started (2026-08-10) firing
// `redundant_field_names` *inside* Diesel `#[derive(Queryable/QueryableByName/Insertable)]`
// expansions — generated code we can't rename — reddening `clippy -D warnings` on every branch
// (66 false positives, zero hand-written cases). Remove this `allow` once the upstream clippy
// regression reverts (re-run CI bare and confirm clean).
#![allow(clippy::redundant_field_names)]
#[macro_use]
extern crate diesel;
#[macro_use]
extern crate rocket;

pub mod backend;
pub mod bootstrap;
pub mod concerns;
pub mod config;
pub mod dispatcher;
pub mod frontend;
pub mod helpers;
pub mod importer;
pub mod jobs;
pub mod migrations;
pub mod models;
pub mod observability;
pub mod reports;
/// Auto-generated diesel schema for the backend DB
pub mod schema;
pub mod telemetry;
pub mod worker;
