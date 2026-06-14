// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Jobs capability: poll long-running jobs. One shared [`JobDto`] renders as JSON for agents
//! (`GET /api/jobs/<uuid>`) and the progress page (`GET /jobs/<uuid>`) polls that same JSON.
//! The job mechanism itself lives in [`crate::jobs`].

use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::{context, Template};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::backend::DbPool;
use crate::jobs::{self, Job};

/// A job as exposed over the API/UI (uuid handle, no internal serial id).
#[derive(Debug, Serialize)]
pub struct JobDto {
  /// External handle.
  pub uuid: String,
  /// Operation kind.
  pub kind: String,
  /// Lifecycle status.
  pub status: String,
  /// Units of work completed.
  pub progress_current: i32,
  /// Total units, when known.
  pub progress_total: Option<i32>,
  /// Current step, or the error on failure.
  pub message: String,
  /// Who started it.
  pub actor: String,
  /// Terminal result payload.
  pub result: Option<Value>,
  /// Created timestamp.
  pub created_at: String,
  /// Last-updated timestamp.
  pub updated_at: String,
  /// Seconds of activity (`updated_at - created_at`): a finished job's total runtime, or a running
  /// one's time from start to its last progress update — observability on duration.
  pub duration_seconds: i64,
  /// Normalized health derived from `status`: `ok` (succeeded), `failed`, `interrupted`, `pending`
  /// (queued), or `running` — the at-a-glance state for the fleet-wide pending check.
  pub health: String,
}

/// Maps a raw lifecycle `status` (`queued`→`running`→`succeeded`/`failed`, plus `interrupted` for
/// orphans) to a normalized health label.
fn health_of(status: &str) -> &'static str {
  match status {
    "succeeded" => "ok",
    "failed" => "failed",
    "interrupted" => "interrupted",
    "queued" => "pending",
    "running" => "running",
    _ => "unknown",
  }
}

impl From<Job> for JobDto {
  fn from(job: Job) -> Self {
    JobDto {
      uuid: job.uuid.to_string(),
      health: health_of(&job.status).to_string(),
      duration_seconds: (job.updated_at - job.created_at).num_seconds().max(0),
      kind: job.kind,
      status: job.status,
      progress_current: job.progress_current,
      progress_total: job.progress_total,
      message: job.message,
      actor: job.actor,
      result: job.result,
      created_at: job.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
      updated_at: job.updated_at.format("%Y-%m-%d %H:%M:%S").to_string(),
    }
  }
}

/// Polls a job by its uuid (the agent twin of the progress page).
#[get("/api/jobs/<uuid>")]
pub fn api_job(uuid: &str, pool: &State<DbPool>) -> Result<Json<JobDto>, Status> {
  let parsed = Uuid::parse_str(uuid).map_err(|_| Status::NotFound)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let job = jobs::find_job(&mut connection, parsed).ok_or(Status::NotFound)?;
  Ok(Json(JobDto::from(job)))
}

/// Lists recent jobs across every background-task capability (import / extend / activate / …) — the
/// fleet-wide **pending check** the observability mandate requires. `?active=true` narrows to the
/// non-terminal (queued/running) jobs; `?limit=` caps the page (default 50, max 200). Most-recent
/// first; each carries `health` + `duration_seconds`.
#[get("/api/jobs?<active>&<limit>")]
pub fn api_jobs(
  active: Option<bool>,
  limit: Option<i64>,
  pool: &State<DbPool>,
) -> Result<Json<Vec<JobDto>>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let limit = limit.unwrap_or(50).clamp(1, 200);
  let jobs = jobs::list_recent(&mut connection, active.unwrap_or(false), limit);
  Ok(Json(jobs.into_iter().map(JobDto::from).collect()))
}

/// The human jobs dashboard (HTML twin of [`api_jobs`]): recent background jobs with their health,
/// duration and progress — the at-a-glance observability screen. `?active=true` shows only pending.
#[get("/jobs?<active>&<limit>")]
pub fn jobs_page(
  active: Option<bool>,
  limit: Option<i64>,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let limit = limit.unwrap_or(50).clamp(1, 200);
  let active = active.unwrap_or(false);
  let jobs: Vec<JobDto> = jobs::list_recent(&mut connection, active, limit)
    .into_iter()
    .map(JobDto::from)
    .collect();
  let global = serde_json::json!({
    "title": "Background jobs",
    "description": "Recent background jobs across the CorTeX framework",
  });
  Ok(Template::render("jobs", context! { global, jobs, active }))
}

/// The human progress page; it polls `GET /api/jobs/<uuid>` (vanilla fetch, no JS framework — D11).
#[get("/jobs/<uuid>")]
pub fn job_page(uuid: &str) -> Template { Template::render("job", context! { uuid }) }

/// The route set for the jobs capability.
pub fn routes() -> Vec<Route> { routes![api_jobs, api_job, jobs_page, job_page] }
