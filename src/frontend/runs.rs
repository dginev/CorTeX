// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Historical-runs capability: inspect the run history of a `(corpus, service)` as an agent API
//! (the JSON twin of the human history screen).
//!
//! Follows the symmetry contract — one shared [`RunDto`] is the read model for both surfaces.
//! Handlers live here; the app is assembled in [`crate::frontend::server`]. This drains the
//! binary's legacy `history` route toward the testable library surface; the HTML twin migrates in a
//! later increment (the legacy `history` page still renders today).

use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::Serialize;

use crate::backend::DbPool;
use crate::models::{Corpus, HistoricalRun, Service};

/// A historical `(corpus, service)` run as exposed over the API: a stable `id` handle,
/// who/why/when, whether it has completed, and the per-severity task tallies captured at
/// completion.
#[derive(Debug, Serialize)]
pub struct RunDto {
  /// Stable run identifier (the external handle for managing a specific run).
  pub id: i32,
  /// Who initiated the run.
  pub owner: String,
  /// Why the run was initiated (free-text description / rerun filter summary).
  pub description: String,
  /// Run start, ISO-8601 (`YYYY-MM-DDThh:mm:ss`, naive/local).
  pub start_time: String,
  /// Run end, ISO-8601; `None` while the run is still open (it closes when the next run starts).
  pub end_time: Option<String>,
  /// Whether the run has completed (`end_time` is set).
  pub completed: bool,
  /// Total tasks in the run (excludes invalids from the denominator elsewhere).
  pub total: i32,
  /// Tasks that completed with no notable problems.
  pub no_problem: i32,
  /// Tasks that completed with warnings.
  pub warning: i32,
  /// Tasks that completed with errors.
  pub error: i32,
  /// Tasks that failed fatally.
  pub fatal: i32,
  /// Invalid tasks (excluded from totals).
  pub invalid: i32,
  /// Tasks still in progress when the run closed.
  pub in_progress: i32,
}

impl From<HistoricalRun> for RunDto {
  fn from(run: HistoricalRun) -> RunDto {
    RunDto {
      id: run.id,
      owner: run.owner,
      description: run.description,
      start_time: run.start_time.format("%Y-%m-%dT%H:%M:%S").to_string(),
      end_time: run
        .end_time
        .map(|end| end.format("%Y-%m-%dT%H:%M:%S").to_string()),
      completed: run.end_time.is_some(),
      total: run.total,
      no_problem: run.no_problem,
      warning: run.warning,
      error: run.error,
      fatal: run.fatal,
      invalid: run.invalid,
      in_progress: run.in_progress,
    }
  }
}

/// Resolves a `(corpus, service)` name pair to its records, mapping each miss to `404`.
fn resolve(
  corpus: &str,
  service: &str,
  connection: &mut diesel::PgConnection,
) -> Result<(Corpus, Service), Status> {
  let corpus = Corpus::find_by_name(corpus, connection).map_err(|_| Status::NotFound)?;
  let service = Service::find_by_name(service, connection).map_err(|_| Status::NotFound)?;
  Ok((corpus, service))
}

/// Lists the run history of a `(corpus, service)`, most-recent first (the agent twin of the history
/// screen). `404` if the corpus or service is unknown.
#[get("/api/runs/<corpus>/<service>")]
pub fn api_runs(
  corpus: &str,
  service: &str,
  pool: &State<DbPool>,
) -> Result<Json<Vec<RunDto>>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let runs = HistoricalRun::find_by(&corpus, &service, &mut connection).unwrap_or_default();
  Ok(Json(runs.into_iter().map(RunDto::from).collect()))
}

/// Returns the currently open run of a `(corpus, service)`, or `null` if none is in progress. `404`
/// if the corpus or service is unknown.
#[get("/api/runs/<corpus>/<service>/current")]
pub fn api_run_current(
  corpus: &str,
  service: &str,
  pool: &State<DbPool>,
) -> Result<Json<Option<RunDto>>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let current = HistoricalRun::find_current(&corpus, &service, &mut connection)
    .map_err(|_| Status::InternalServerError)?;
  Ok(Json(current.map(RunDto::from)))
}

/// The route set for the historical-runs capability.
pub fn routes() -> Vec<Route> { routes![api_runs, api_run_current] }
