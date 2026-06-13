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

use std::collections::HashSet;

use diesel::pg::PgConnection;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::{from_address, progress_report, DatabaseUrl, DbPool};
use crate::concerns::CortexInsertable;
use crate::frontend::jobs::JobDto;
use crate::importer::Importer;
use crate::jobs::{self, JobProgress};
use crate::models::{Corpus, NewCorpus};

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

/// Request body for registering and importing a corpus.
#[derive(Debug, Deserialize)]
pub struct ImportRequest {
  /// Corpus name (external handle).
  pub name: String,
  /// Filesystem path to the corpus root.
  pub path: String,
  /// Whether documents are multi-file (complex).
  pub complex: bool,
  /// Optional human-readable description.
  pub description: Option<String>,
}

/// Registers a corpus and starts an in-process import job; returns `202 Accepted` + the job handle.
/// Agents and humans poll `GET /api/jobs/<uuid>` (or the progress page) for completion.
#[post("/api/corpora", format = "json", data = "<request>")]
pub fn import_corpus(
  request: Json<ImportRequest>,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<(Status, Json<JobDto>), Status> {
  let request = request.into_inner();
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  if Corpus::find_by_name(&request.name, &mut connection).is_ok() {
    return Err(Status::Conflict);
  }
  NewCorpus {
    name: request.name.clone(),
    path: request.path.clone(),
    complex: request.complex,
    description: request.description.clone().unwrap_or_default(),
  }
  .create(&mut connection)
  .map_err(|_| Status::InternalServerError)?;
  let corpus = Corpus::find_by_name(&request.name, &mut connection)
    .map_err(|_| Status::InternalServerError)?;
  drop(connection);

  let database_url = database_url.0.clone();
  let params = serde_json::json!({ "name": request.name, "path": request.path });
  let job_uuid = jobs::spawn_job(
    pool.inner().clone(),
    "corpus_import",
    "admin",
    params,
    move |progress| run_import(&database_url, corpus, progress),
  )
  .map_err(|_| Status::InternalServerError)?;

  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let job = jobs::find_job(&mut connection, job_uuid).ok_or(Status::InternalServerError)?;
  Ok((Status::Accepted, Json(JobDto::from(job))))
}

/// The body of a `corpus_import` job: run the importer in-process against `corpus`, reporting
/// progress, and return the number of import-service tasks created.
fn run_import(database_url: &str, corpus: Corpus, progress: &JobProgress) -> Result<Value, String> {
  let corpus_id = corpus.id;
  let mut importer = Importer {
    corpus,
    backend: from_address(database_url),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };
  progress.step(0, None, "importing corpus");
  importer.process().map_err(|error| error.to_string())?;
  let imported = count_import_tasks(&mut importer.backend.connection, corpus_id);
  progress.step(imported, Some(imported), "import complete");
  Ok(serde_json::json!({ "imported": imported }))
}

/// Counts the import-service tasks (service id 2) registered for a corpus.
fn count_import_tasks(connection: &mut PgConnection, corpus: i32) -> i32 {
  use crate::schema::tasks::dsl::{corpus_id, service_id, tasks};
  use diesel::prelude::*;
  tasks
    .filter(corpus_id.eq(corpus))
    .filter(service_id.eq(2))
    .count()
    .get_result::<i64>(connection)
    .unwrap_or(0) as i32
}

/// Deletes a corpus and all of its tasks and log messages. **Guarded:** the caller must echo the
/// corpus name via `?confirm=<name>` to proceed (prevents accidental wipes; the UI confirms the
/// same way). Returns 204 on success, 400 if the confirmation does not match, 404 if unknown.
#[delete("/api/corpora/<name>?<confirm>")]
pub fn delete_corpus(name: &str, confirm: Option<&str>, pool: &State<DbPool>) -> Status {
  if confirm != Some(name) {
    return Status::BadRequest;
  }
  let mut connection = match pool.get() {
    Ok(connection) => connection,
    Err(_) => return Status::ServiceUnavailable,
  };
  let corpus = match Corpus::find_by_name(name, &mut connection) {
    Ok(corpus) => corpus,
    Err(_) => return Status::NotFound,
  };
  match delete_corpus_cascade(&mut connection, corpus) {
    Ok(()) => Status::NoContent,
    Err(status) => status,
  }
}

/// Removes a corpus's log messages (the `log_*` tables have no FK cascade), then its tasks and the
/// corpus row itself.
fn delete_corpus_cascade(connection: &mut PgConnection, corpus: Corpus) -> Result<(), Status> {
  use crate::schema::{log_errors, log_fatals, log_infos, log_invalids, log_warnings, tasks};
  use diesel::prelude::*;
  let corpus_id = corpus.id;
  let task_ids = || {
    tasks::table
      .filter(tasks::corpus_id.eq(corpus_id))
      .select(tasks::id)
  };
  let fail = |_| Status::InternalServerError;
  diesel::delete(log_infos::table.filter(log_infos::task_id.eq_any(task_ids())))
    .execute(connection)
    .map_err(fail)?;
  diesel::delete(log_warnings::table.filter(log_warnings::task_id.eq_any(task_ids())))
    .execute(connection)
    .map_err(fail)?;
  diesel::delete(log_errors::table.filter(log_errors::task_id.eq_any(task_ids())))
    .execute(connection)
    .map_err(fail)?;
  diesel::delete(log_fatals::table.filter(log_fatals::task_id.eq_any(task_ids())))
    .execute(connection)
    .map_err(fail)?;
  diesel::delete(log_invalids::table.filter(log_invalids::task_id.eq_any(task_ids())))
    .execute(connection)
    .map_err(fail)?;
  corpus.destroy(connection).map_err(fail)?;
  Ok(())
}

/// The route set for the corpus-management capability.
pub fn routes() -> Vec<Route> { routes![api_corpora, api_corpus, import_corpus, delete_corpus] }
