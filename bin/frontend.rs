// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
#![allow(clippy::implicit_hasher, clippy::let_unit_value)]
#[macro_use]
extern crate rocket;

use std::path::{Path, PathBuf};

use rocket::fs::NamedFile;
use rocket::futures::TryFutureExt;
use rocket::response::status::NotFound;

use cortex::config::config;
use cortex::frontend::cors::CORS;

// The binary now owns only the static-asset routes; every database-backed route (corpora, reports,
// runs, services, management, the document-serving + human rerun/save-snapshot paths) lives on the
// pooled, testable library surface and is mounted by `cortex::frontend::server::mount_api`.

#[get("/favicon.ico")]
async fn favicon() -> Result<NamedFile, NotFound<String>> {
  let path = Path::new(&config().assets.public_dir).join("favicon.ico");
  NamedFile::open(&path)
    .map_err(|_| NotFound(format!("Bad path: {path:?}")))
    .await
}

#[get("/robots.txt")]
async fn robots() -> Result<NamedFile, NotFound<String>> {
  let path = Path::new(&config().assets.public_dir).join("robots.txt");
  NamedFile::open(&path)
    .map_err(|_| NotFound(format!("Bad path: {path:?}")))
    .await
}

#[get("/public/<file..>")]
async fn files(file: PathBuf) -> Result<NamedFile, NotFound<String>> {
  let path = Path::new(&config().assets.public_dir).join(file);
  NamedFile::open(&path)
    .map_err(|_| NotFound(format!("Bad path: {path:?}")))
    .await
}

#[launch]
fn rocket() -> _ {
  // Install our `tracing` subscriber *before* Rocket builds, so application-level events (e.g. the
  // P-2 slow-report warning in `frontend::concerns`) are actually emitted — Rocket's own subscriber
  // only surfaces `rocket::*` targets and ignores `RUST_LOG`, so without this the frontend's
  // app-level `tracing` events go nowhere (which is why legacy code reached for `println!`). Set
  // first so Rocket finds an existing global subscriber and defers to it (idempotent `try_init`);
  // `RUST_LOG` now controls frontend verbosity the same way it does the dispatcher.
  cortex::observability::init_tracing();
  // Drive the template directory from the runtime configuration rather than a CWD-relative
  // Rocket.toml, so the binary is not bound to its working directory.
  let figment =
    rocket::Config::figment().merge(("template_dir", config().assets.template_dir.as_str()));
  let rocket = rocket::custom(figment)
    .mount("/", routes![favicon, robots, files])
    .attach(CORS());
  // Mount the library API/UI surface (management/health/settings, corpora, reports, the document +
  // rerun routes, …); the builder owns the connection pool and the Template fairing.
  cortex::frontend::server::mount_api(rocket)
}
