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
use serde::Serialize;

use crate::backend::{
  category_rollup, category_total, severity_total, what_rollup, DbPool, ReportSummaryRow,
};
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

/// The route set for the reports capability.
pub fn routes() -> Vec<Route> { routes![api_category_report, api_what_report] }
