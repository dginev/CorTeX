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
}

impl From<Job> for JobDto {
  fn from(job: Job) -> Self {
    JobDto {
      uuid: job.uuid.to_string(),
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

/// The human progress page; it polls `GET /api/jobs/<uuid>` (vanilla fetch, no JS framework — D11).
#[get("/jobs/<uuid>")]
pub fn job_page(uuid: &str) -> Template { Template::render("job", context! { uuid }) }

/// The route set for the jobs capability.
pub fn routes() -> Vec<Route> { routes![api_job, job_page] }
