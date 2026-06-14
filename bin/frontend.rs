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
use rocket::response::status::{Accepted, NotFound};
use rocket::serde::json::Json;
use rocket_dyn_templates::Template;

use cortex::config::config;
use cortex::frontend::concerns::{serve_entry, serve_entry_preview, serve_rerun, serve_savetasks};
use cortex::frontend::cors::CORS;
use cortex::frontend::params::RerunRequestParams;

#[get("/preview/<corpus_name>/<service_name>/<entry_name>")]
fn preview_entry(
  corpus_name: String,
  service_name: String,
  entry_name: String,
) -> Result<Template, NotFound<String>> {
  serve_entry_preview(corpus_name, service_name, entry_name)
}

#[post("/entry/<service_name>/<entry_id>")]
async fn entry_fetch(service_name: String, entry_id: usize) -> Result<NamedFile, NotFound<String>> {
  serve_entry(service_name, entry_id).await
}

#[post(
  "/rerun/<corpus_name>/<service_name>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_corpus(
  corpus_name: String,
  service_name: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  let corpus_name = corpus_name.to_lowercase();
  serve_rerun(corpus_name, service_name, None, None, None, rr)
}

#[post(
  "/rerun/<corpus_name>/<service_name>/<severity>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_severity(
  corpus_name: String,
  service_name: String,
  severity: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  serve_rerun(corpus_name, service_name, Some(severity), None, None, rr)
}

#[post(
  "/rerun/<corpus_name>/<service_name>/<severity>/<category>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_category(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  serve_rerun(
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    None,
    rr,
  )
}

#[post(
  "/rerun/<corpus_name>/<service_name>/<severity>/<category>/<what>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_what(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  what: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  serve_rerun(
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    Some(what),
    rr,
  )
}

#[post(
  "/savetasks/<corpus_name>/<service_name>",
  format = "application/json",
  data = "<rr>"
)]
fn savetasks(
  corpus_name: String,
  service_name: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  let corpus_name = corpus_name.to_lowercase();
  serve_savetasks(corpus_name, service_name, rr)
}

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
  // Drive the template directory from the runtime configuration rather than a CWD-relative
  // Rocket.toml, so the binary is not bound to its working directory.
  let figment =
    rocket::Config::figment().merge(("template_dir", config().assets.template_dir.as_str()));
  let rocket = rocket::custom(figment)
    .mount(
      "/",
      routes![
        favicon,
        robots,
        files,
        preview_entry,
        entry_fetch,
        rerun_corpus,
        rerun_severity,
        rerun_category,
        rerun_what,
        savetasks
      ],
    )
    .attach(CORS());
  // Mount the library API/UI surface (management/health/settings, corpora, …); the builder owns the
  // connection pool and the Template fairing.
  cortex::frontend::server::mount_api(rocket)
}
