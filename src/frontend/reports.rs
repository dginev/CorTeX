// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Reports capability: the typed, paginated agent API for the category and `what` reports — the
//! agent twin of the most-used human screens (severity-report / category-report).
//!
//! Reads straight from the `report_summary` rollup (`crate::backend::{category_rollup, what_rollup,
//! severity_total, category_total}`): indexed, refreshed on the run-completion path, and already
//! paginated. This is the structured counterpart to the HTML reports the legacy routes render;
//! both reflect the same rollup, so humans and agents see the same numbers.

use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::Template;
use serde::Serialize;

use crate::backend::{
  category_rollup, category_total, from_address, severity_total, what_rollup, DatabaseUrl, DbPool,
  ReportSummaryRow, RerunOptions,
};
use crate::frontend::actor::Actor;
use crate::frontend::concerns::serve_report;
use crate::frontend::params::ReportParams;
use crate::jobs;
use crate::models::{Corpus, Service};

/// One report row: a category (in the category report) or a `what` class (in the drill-down), with
/// its distinct-task and message counts.
#[derive(Debug, Serialize)]
pub struct ReportRowDto {
  /// Category or `what` name (the empty string for uncategorized messages).
  pub name: String,
  /// Distinct tasks contributing to this row.
  pub tasks: i64,
  /// Total messages for this row.
  pub messages: i64,
}

/// The category report for a `(corpus, service, severity)`: one row per category (a page of them),
/// plus the severity grand totals to compute shares against.
#[derive(Debug, Serialize)]
pub struct CategoryReportDto {
  /// The severity reported on.
  pub severity: String,
  /// Distinct tasks carrying at least one message of this severity.
  pub total_tasks: i64,
  /// Total messages of this severity.
  pub total_messages: i64,
  /// The category rows for the requested page.
  pub categories: Vec<ReportRowDto>,
}

/// The `what` drill-down for a `(corpus, service, severity, category)`: one row per `what` (a
/// page), plus the category totals.
#[derive(Debug, Serialize)]
pub struct WhatReportDto {
  /// The severity reported on.
  pub severity: String,
  /// The category drilled into.
  pub category: String,
  /// Distinct tasks in this category.
  pub total_tasks: i64,
  /// Total messages in this category.
  pub total_messages: i64,
  /// The `what` rows for the requested page.
  pub whats: Vec<ReportRowDto>,
}

/// Severities the rollup aggregates over (the four message severities plus the all-messages `info`
/// dimension). Anything else is a `400` rather than a silently-empty report.
fn is_rollup_severity(severity: &str) -> bool {
  matches!(severity, "warning" | "error" | "fatal" | "invalid" | "info")
}

/// Resolves a `(corpus, service)` name pair, mapping each miss to `404`.
fn resolve(
  corpus: &str,
  service: &str,
  connection: &mut diesel::PgConnection,
) -> Result<(Corpus, Service), Status> {
  let corpus = Corpus::find_by_name(corpus, connection).map_err(|_| Status::NotFound)?;
  let service = Service::find_by_name(service, connection).map_err(|_| Status::NotFound)?;
  Ok((corpus, service))
}

/// Grand totals (distinct tasks, messages) from an optional rollup total row.
fn totals(row: Option<ReportSummaryRow>) -> (i64, i64) {
  row.map_or((0, 0), |total| (total.task_count, total.message_count))
}

/// The category report (agent twin of the severity screen): one row per category, descending by
/// task count, paginated. `400` on an unknown severity, `404` on an unknown corpus/service.
#[get("/api/reports/<corpus>/<service>/<severity>?<offset>&<page_size>")]
pub fn api_category_report(
  corpus: &str,
  service: &str,
  severity: &str,
  offset: Option<i64>,
  page_size: Option<i64>,
  pool: &State<DbPool>,
) -> Result<Json<CategoryReportDto>, Status> {
  if !is_rollup_severity(severity) {
    return Err(Status::BadRequest);
  }
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let limit = page_size.unwrap_or(100);
  let offset = offset.unwrap_or(0);
  let categories = category_rollup(
    &mut connection,
    corpus.id,
    service.id,
    severity,
    limit,
    offset,
  )
  .unwrap_or_default()
  .into_iter()
  .map(|row| ReportRowDto {
    name: row.category,
    tasks: row.task_count,
    messages: row.message_count,
  })
  .collect();
  let (total_tasks, total_messages) =
    totals(severity_total(&mut connection, corpus.id, service.id, severity).unwrap_or_default());
  Ok(Json(CategoryReportDto {
    severity: severity.to_string(),
    total_tasks,
    total_messages,
    categories,
  }))
}

/// The `what` drill-down (agent twin of the category screen): one row per `what` within a category,
/// descending by task count, paginated. `400` on an unknown severity, `404` on an unknown
/// corpus/service.
#[get("/api/reports/<corpus>/<service>/<severity>/<category>?<offset>&<page_size>")]
pub fn api_what_report(
  corpus: &str,
  service: &str,
  severity: &str,
  category: &str,
  offset: Option<i64>,
  page_size: Option<i64>,
  pool: &State<DbPool>,
) -> Result<Json<WhatReportDto>, Status> {
  if !is_rollup_severity(severity) {
    return Err(Status::BadRequest);
  }
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let limit = page_size.unwrap_or(100);
  let offset = offset.unwrap_or(0);
  let whats = what_rollup(
    &mut connection,
    corpus.id,
    service.id,
    severity,
    category,
    limit,
    offset,
  )
  .unwrap_or_default()
  .into_iter()
  .map(|row| ReportRowDto {
    name: row.what.unwrap_or_default(),
    tasks: row.task_count,
    messages: row.message_count,
  })
  .collect();
  let (total_tasks, total_messages) = totals(
    category_total(&mut connection, corpus.id, service.id, severity, category).unwrap_or_default(),
  );
  Ok(Json(WhatReportDto {
    severity: severity.to_string(),
    category: category.to_string(),
    total_tasks,
    total_messages,
    whats,
  }))
}

/// Acknowledgement of a rerun: the scope that was marked and who marked it.
#[derive(Debug, Serialize)]
pub struct RerunAckDto {
  /// Corpus the rerun targeted.
  pub corpus: String,
  /// Service the rerun targeted.
  pub service: String,
  /// The authenticated initiator (the run's `owner`).
  pub actor: String,
  /// The recorded run description.
  pub description: String,
}

/// Marks the selected `(corpus, service[, severity, category, what])` scope for reprocessing — the
/// agent twin of the report screen's rerun action, and a new historical run. **Token-gated** via
/// the [`Actor`] guard (`X-Cortex-Token` header or `?token=`); `401` without a valid token, so
/// results can't be wiped by an unauthenticated caller. `400` on an unknown severity, `404` on an
/// unknown corpus/service. Returns `202 Accepted`.
#[post("/api/reports/<corpus>/<service>/rerun?<severity>&<category>&<what>&<description>")]
#[allow(clippy::too_many_arguments)]
pub fn rerun_report(
  corpus: &str,
  service: &str,
  severity: Option<&str>,
  category: Option<&str>,
  what: Option<&str>,
  description: Option<&str>,
  actor: Actor,
  database_url: &State<DatabaseUrl>,
  pool: &State<DbPool>,
) -> Result<(Status, Json<RerunAckDto>), Status> {
  if let Some(severity) = severity {
    if !is_rollup_severity(severity) {
      return Err(Status::BadRequest);
    }
  }
  let description = description.unwrap_or("rerun via API").to_string();
  // A fresh connection for this low-frequency, consequential admin action (mirrors the legacy
  // path).
  let mut backend = from_address(&database_url.0);
  let corpus_record =
    Corpus::find_by_name(corpus, &mut backend.connection).map_err(|_| Status::NotFound)?;
  let service_record =
    Service::find_by_name(service, &mut backend.connection).map_err(|_| Status::NotFound)?;
  backend
    .mark_rerun(RerunOptions {
      corpus: &corpus_record,
      service: &service_record,
      severity_opt: severity.map(str::to_string),
      category_opt: category.map(str::to_string),
      what_opt: what.map(str::to_string),
      description_opt: Some(description.clone()),
      owner_opt: Some(actor.owner.clone()),
    })
    .map_err(|_| Status::InternalServerError)?;
  // Reflect the rerun in reports without blocking this request: spawn the rollup refresh off the
  // request path (debounced, observable via `/api/jobs`). Best-effort — the rerun already
  // committed.
  let _ = jobs::spawn_report_refresh(pool.inner().clone(), &actor.owner);
  Ok((
    Status::Accepted,
    Json(RerunAckDto {
      corpus: corpus.to_string(),
      service: service.to_string(),
      actor: actor.owner,
      description,
    }),
  ))
}

/// Acknowledgement for a forced report-rollup refresh: the background [`crate::jobs`] handle to
/// poll.
#[derive(Serialize)]
pub struct RefreshAckDto {
  /// The spawned (or already-running, if debounced) refresh job's external uuid.
  pub job: String,
  /// Where to poll the job's status / health / duration.
  pub poll: String,
  /// The token-resolved actor recorded as the job's initiator.
  pub actor: String,
}

/// Forces a rebuild of the `report_summary` rollup that backs **every** report page, as a
/// background job — the rebuild is multi-minute at production scale, so it must not block the
/// request (see `docs/REPORT_FRESHNESS.md`). Returns the job handle immediately (`202 Accepted`);
/// poll `GET /api/jobs/<job>` for status/health. **Debounced:** a refresh already in flight is
/// reused rather than piled on. **Token-gated** via the [`Actor`] guard (`X-Cortex-Token` /
/// `?token=`); `401` without a valid token.
#[post("/api/reports/refresh")]
pub fn refresh_reports(
  actor: Actor,
  pool: &State<DbPool>,
) -> Result<(Status, Json<RefreshAckDto>), Status> {
  let job_uuid = jobs::spawn_report_refresh(pool.inner().clone(), &actor.owner)
    .map_err(|_| Status::InternalServerError)?;
  Ok((
    Status::Accepted,
    Json(RefreshAckDto {
      job: job_uuid.to_string(),
      poll: format!("/api/jobs/{job_uuid}"),
      actor: actor.owner,
    }),
  ))
}

/// The token field of the human "refresh reports" form on the jobs dashboard.
#[derive(rocket::FromForm)]
pub struct RefreshForm {
  /// A rerun token (the same admin tokens that gate writes); resolved to the job's actor.
  pub token: String,
}

/// The human twin of [`refresh_reports`]: the jobs-dashboard "Refresh reports now" button. Resolves
/// the submitted token to an actor (the `Actor` guard reads header/query, not a form field, so we
/// resolve it here), spawns the same debounced refresh job, and redirects to `/jobs` where the
/// admin watches it run — the async UI pattern (no blocking, no JS). `401` on an unknown token.
#[post("/reports/refresh", data = "<form>")]
pub fn refresh_reports_human(
  form: rocket::form::Form<RefreshForm>,
  pool: &State<DbPool>,
) -> Result<rocket::response::Redirect, Status> {
  let owner = crate::config::config()
    .auth
    .rerun_tokens
    .get(&form.token)
    .cloned()
    .ok_or(Status::Unauthorized)?;
  jobs::spawn_report_refresh(pool.inner().clone(), &owner)
    .map_err(|_| Status::InternalServerError)?;
  Ok(rocket::response::Redirect::to("/jobs"))
}

// --- The human report screens (HTML twins of the typed report API above) -----------------------
//
// These render the corpus/service report hierarchy (top → severity → category → `what` → task
// list) via the shared [`serve_report`] controller, now reading over a **pooled** connection
// instead of the prototype per-request `Backend::default()`. Relocated from `bin/frontend.rs` onto
// the library surface so they are testable and share the pool with the agent API. `404` on an
// unknown corpus/service; `503` if the pool is exhausted.

/// Checks out a pooled connection, mapping exhaustion to `503`.
fn pooled(pool: &State<DbPool>) -> Result<crate::backend::PooledConn, Status> {
  pool.get().map_err(|_| Status::ServiceUnavailable)
}

/// Top-level corpus/service report (overall progress).
#[get("/corpus/<corpus_name>/<service_name>")]
pub fn top_service_report(
  corpus_name: String,
  service_name: String,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let mut connection = pooled(pool)?;
  serve_report(
    &mut connection,
    corpus_name,
    service_name,
    None,
    None,
    None,
    None,
  )
}

/// Severity-level report: the categories carrying messages of `severity`.
#[get("/corpus/<corpus_name>/<service_name>/<severity>")]
pub fn severity_service_report(
  corpus_name: String,
  service_name: String,
  severity: String,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let mut connection = pooled(pool)?;
  serve_report(
    &mut connection,
    corpus_name,
    service_name,
    Some(severity),
    None,
    None,
    None,
  )
}

/// Severity-level report with paging/all-messages query params.
#[get("/corpus/<corpus_name>/<service_name>/<severity>?<params..>")]
pub fn severity_service_report_all(
  corpus_name: String,
  service_name: String,
  severity: String,
  params: Option<ReportParams>,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let mut connection = pooled(pool)?;
  serve_report(
    &mut connection,
    corpus_name,
    service_name,
    Some(severity),
    None,
    None,
    params,
  )
}

/// Category-level report: the `what` classes within a `(severity, category)`.
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>")]
pub fn category_service_report(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let mut connection = pooled(pool)?;
  serve_report(
    &mut connection,
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    None,
    None,
  )
}

/// Category-level report with paging/all-messages query params.
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>?<params..>")]
pub fn category_service_report_all(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  params: Option<ReportParams>,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let mut connection = pooled(pool)?;
  serve_report(
    &mut connection,
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    None,
    params,
  )
}

/// `what`-level report: the task list for a `(severity, category, what)`.
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>/<what>")]
pub fn what_service_report(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  what: String,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let mut connection = pooled(pool)?;
  serve_report(
    &mut connection,
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    Some(what),
    None,
  )
}

/// `what`-level report with paging/all-messages query params.
#[allow(clippy::too_many_arguments)]
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>/<what>?<params..>")]
pub fn what_service_report_all(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  what: String,
  params: Option<ReportParams>,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let mut connection = pooled(pool)?;
  serve_report(
    &mut connection,
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    Some(what),
    params,
  )
}

/// The route set for the reports capability (typed API + the human report screens).
pub fn routes() -> Vec<Route> {
  routes![
    api_category_report,
    api_what_report,
    rerun_report,
    refresh_reports,
    refresh_reports_human,
    top_service_report,
    severity_service_report,
    severity_service_report_all,
    category_service_report,
    category_service_report_all,
    what_service_report,
    what_service_report_all
  ]
}
