// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Corpus-management capability: list/inspect/import/delete corpora as screens + API.
//!
//! Follows the symmetry contract — one shared [`CorpusDto`] renders as JSON for agents and (later)
//! as HTML for humans. Handlers live here; the app is assembled in [`crate::frontend::server`].
//! This is the first capability drained out of the binary's legacy routes; more land per increment.

use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::Serialize;

use crate::backend::{progress_report, DbPool};
use crate::models::Corpus;

/// A corpus as exposed over the API/UI. `name` is the stable external handle used by every route.
#[derive(Debug, Serialize)]
pub struct CorpusDto {
  /// Human-readable corpus name (its external handle).
  pub name: String,
  /// Filesystem path to the corpus root.
  pub path: String,
  /// Human-readable description.
  pub description: String,
  /// Whether documents are multi-file (complex) rather than a single TeX file.
  pub complex: bool,
}

impl From<Corpus> for CorpusDto {
  fn from(corpus: Corpus) -> Self {
    CorpusDto {
      name: corpus.name,
      path: corpus.path,
      description: corpus.description,
      complex: corpus.complex,
    }
  }
}

/// Lists all registered corpora (the agent twin of the overview screen).
#[get("/api/corpora")]
pub fn api_corpora(pool: &State<DbPool>) -> Json<Vec<CorpusDto>> {
  let corpora = match pool.get() {
    Ok(mut connection) => Corpus::all(&mut connection).unwrap_or_default(),
    Err(_) => Vec::new(),
  };
  Json(corpora.into_iter().map(CorpusDto::from).collect())
}

/// Per-service status counts within a corpus (mirrors the progress report).
#[derive(Debug, Serialize)]
pub struct ServiceStatusDto {
  /// Service name.
  pub name: String,
  /// Service version.
  pub version: f32,
  /// Total valid tasks (excludes invalids).
  pub total: i64,
  /// Completed with no notable problems.
  pub no_problem: i64,
  /// Completed with warnings.
  pub warning: i64,
  /// Completed with errors.
  pub error: i64,
  /// Fatal failures.
  pub fatal: i64,
  /// Invalid tasks (excluded from totals).
  pub invalid: i64,
  /// Queued / not-yet-processed tasks.
  pub todo: i64,
}

/// A corpus with its activated services and their status counts.
#[derive(Debug, Serialize)]
pub struct CorpusDetailDto {
  /// Corpus name (external handle).
  pub name: String,
  /// Filesystem path to the corpus root.
  pub path: String,
  /// Human-readable description.
  pub description: String,
  /// Whether documents are multi-file.
  pub complex: bool,
  /// Services activated on this corpus, with status counts.
  pub services: Vec<ServiceStatusDto>,
}

/// Inspects a single corpus: its activated services and per-service status counts.
#[get("/api/corpora/<name>")]
pub fn api_corpus(name: &str, pool: &State<DbPool>) -> Result<Json<CorpusDetailDto>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let corpus = Corpus::find_by_name(name, &mut connection).map_err(|_| Status::NotFound)?;
  let services = corpus.select_services(&mut connection).unwrap_or_default();
  let mut service_status = Vec::new();
  for service in services {
    let report = progress_report(&mut connection, corpus.id, service.id);
    let count = |key: &str| report.get(key).copied().unwrap_or(0.0) as i64;
    service_status.push(ServiceStatusDto {
      name: service.name,
      version: service.version,
      total: count("total"),
      no_problem: count("no_problem"),
      warning: count("warning"),
      error: count("error"),
      fatal: count("fatal"),
      invalid: count("invalid"),
      todo: count("todo"),
    });
  }
  Ok(Json(CorpusDetailDto {
    name: corpus.name,
    path: corpus.path,
    description: corpus.description,
    complex: corpus.complex,
    services: service_status,
  }))
}

/// The route set for the corpus-management capability.
pub fn routes() -> Vec<Route> { routes![api_corpora, api_corpus] }
