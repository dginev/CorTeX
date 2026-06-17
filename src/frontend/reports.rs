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
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::Template;
use serde::Serialize;

use crate::backend::{
  category_rollup, category_total, from_address, progress_report, severity_total, task_messages,
  task_report, what_rollup, DatabaseUrl, DbPool, MessageCounts, ReportSummaryRow, RerunOptions,
  TaskReportOptions,
};
use crate::frontend::actor::{require_admin, Actor, AdminReject, AdminSession};
use crate::frontend::concerns::{serve_report, LiveReportLimiter};
use crate::frontend::params::{ReportParams, MAX_REPORT_OFFSET, MAX_REPORT_PAGE_SIZE};
use crate::helpers::TaskStatus;
use crate::jobs;
use crate::models::{Corpus, Service, Task};

/// One status bucket in the service overview: a conversion-status key with its task count and its
/// share of the valid-task total.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusCountDto {
  /// Status key: `no_problem` | `warning` | `error` | `fatal` | `invalid` | `todo` | `blocked` |
  /// `queued`.
  pub status: String,
  /// Tasks currently in this status.
  pub tasks: i64,
  /// Percentage of the valid-task total (invalids excluded from the denominator), 2-dp.
  pub percent: f64,
}

/// The service-overview hub (the macro top rung of the report ladder): the `(corpus, service)`
/// conversion-status breakdown an agent reads first, before drilling into a severity. The `status`
/// keys double as the `<severity>` path segment for the category report
/// (`GET /api/reports/<corpus>/<service>/<severity>`).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ServiceOverviewDto {
  /// Corpus name.
  pub corpus: String,
  /// Service name.
  pub service: String,
  /// Valid-task total (invalids excluded), the percentage denominator.
  pub total: i64,
  /// One bucket per conversion status, in canonical severity order.
  pub statuses: Vec<StatusCountDto>,
}

/// One report row: a category (in the category report) or a `what` class (in the drill-down), with
/// its distinct-task and message counts.
#[derive(Debug, Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Serialize, schemars::JsonSchema)]
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

/// One worker-log message behind a document's status: its severity and the `category`/`what`/
/// `details` triple parsed from the worker's `cortex.log`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MessageDto {
  /// `info` | `warning` | `error` | `fatal` | `invalid`.
  pub severity: String,
  /// Mid-level description (open set).
  pub category: String,
  /// Low-level description (open set).
  pub what: String,
  /// Technical details (e.g. localization info).
  pub details: String,
}

/// True per-severity message totals for a document (the real counts behind a possibly-sampled
/// `messages` list — see [`DocumentReportDto::messages`]).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MessageCountsDto {
  /// info-level messages
  pub info: i64,
  /// warning-level messages
  pub warning: i64,
  /// error-level messages
  pub error: i64,
  /// fatal-level messages
  pub fatal: i64,
  /// invalid-level messages
  pub invalid: i64,
  /// total across all severities
  pub total: i64,
}
impl From<MessageCounts> for MessageCountsDto {
  fn from(counts: MessageCounts) -> Self {
    MessageCountsDto {
      info: counts.info,
      warning: counts.warning,
      error: counts.error,
      fatal: counts.fatal,
      invalid: counts.invalid,
      total: counts.total(),
    }
  }
}

/// The per-article forensic report (the micro magnification): one document's conversion outcome for
/// a service, plus the messages behind it — the answer to "what are the errors of this article?".
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DocumentReportDto {
  /// Corpus name.
  pub corpus: String,
  /// Service name.
  pub service: String,
  /// The document's short name as queried (e.g. `0801.1234`).
  pub name: String,
  /// The document's source archive path (`tasks.entry`).
  pub entry: String,
  /// The task id for this `(corpus, service, document)`.
  pub task_id: i64,
  /// Conversion status key: `no_problem` | `warning` | `error` | `fatal` | `invalid` | `todo` | …
  pub status: String,
  /// The raw signed status code (see `helpers::TaskStatus`).
  pub status_code: i32,
  /// The document's messages, info → invalid — **sampled**: at most
  /// `backend::DOCUMENT_MESSAGE_CAP` per severity, so a pathological document (millions of
  /// messages) can't blow up the response. Use `message_counts` for the real magnitude and
  /// `messages_truncated` to know it was capped.
  pub messages: Vec<MessageDto>,
  /// The true per-severity message totals (before the sampling cap on `messages`).
  pub message_counts: MessageCountsDto,
  /// `true` when `messages` was capped (the document has more messages than are listed).
  pub messages_truncated: bool,
  /// Path to download the converted result archive.
  pub result_url: String,
  /// Path to the human preview page.
  pub preview_url: String,
}

/// Severities the rollup aggregates over (the four message severities plus the all-messages `info`
/// dimension). Anything else is a `400` rather than a silently-empty report.
fn is_rollup_severity(severity: &str) -> bool {
  matches!(severity, "warning" | "error" | "fatal" | "invalid" | "info")
}

/// Severities valid for a **rerun** filter — which differs from the report rollup, because rerun
/// filters *tasks* (or their messages), it doesn't aggregate a report (R-9). With a `category` the
/// scope is over log messages (`warning`/`error`/`fatal`/`invalid` + the all-messages `info`);
/// without one it is over task *status* (`no_problem`/`warning`/`error`/`fatal`/`invalid` —
/// `no_problem` IS a legitimate rerun scope, e.g. regenerating output after a worker upgrade, and
/// `info` is not a task status). One shared validator for the agent (`rerun_report`) and human
/// (`serve_rerun`) surfaces, so both reject the same typos instead of silently mis-scoping to
/// `no_problem`.
pub(crate) fn is_valid_rerun_severity(severity: &str, has_category: bool) -> bool {
  if has_category {
    matches!(severity, "warning" | "error" | "fatal" | "invalid" | "info")
  } else {
    matches!(
      severity,
      "no_problem" | "warning" | "error" | "fatal" | "invalid"
    )
  }
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

/// The service-overview hub (agent twin of the top report screen): the `(corpus, service)`
/// conversion-status breakdown — total + per-status counts/percentages — the macro entry point an
/// agent reads before drilling into a severity. Shares `backend::progress_report` with the HTML top
/// screen, so the numbers match. `404` on an unknown corpus/service.
#[rocket_okapi::openapi(tag = "Reports")]
#[get("/api/reports/<corpus>/<service>")]
pub fn api_service_overview(
  corpus: &str,
  service: &str,
  pool: &State<DbPool>,
) -> Result<Json<ServiceOverviewDto>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let stats = progress_report(&mut connection, corpus.id, service.id);
  let statuses = TaskStatus::keys()
    .into_iter()
    .map(|status| {
      let percent = stats
        .get(&format!("{status}_percent"))
        .copied()
        .unwrap_or(0.0);
      let tasks = stats.get(&status).copied().unwrap_or(0.0) as i64;
      StatusCountDto {
        status,
        tasks,
        percent,
      }
    })
    .collect();
  Ok(Json(ServiceOverviewDto {
    corpus: corpus.name,
    service: service.name,
    total: stats.get("total").copied().unwrap_or(0.0) as i64,
    statuses,
  }))
}

/// The category report (agent twin of the severity screen): one row per category, descending by
/// task count, paginated. `400` on an unknown severity, `404` on an unknown corpus/service.
#[rocket_okapi::openapi(tag = "Reports")]
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
  let limit = page_size.unwrap_or(100).clamp(1, MAX_REPORT_PAGE_SIZE);
  let offset = offset.unwrap_or(0).clamp(0, MAX_REPORT_OFFSET);
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
#[rocket_okapi::openapi(tag = "Reports")]
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
  let limit = page_size.unwrap_or(100).clamp(1, MAX_REPORT_PAGE_SIZE);
  let offset = offset.unwrap_or(0).clamp(0, MAX_REPORT_OFFSET);
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

/// One affected document in the entry list: its short name (paper id), task id, and the message
/// detail that placed it under the queried `(severity, category, what)`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EntryRowDto {
  /// The document's short name (e.g. `0801.1234`) — feed it to
  /// `GET /api/corpus/<c>/<svc>/document/<name>` for per-article forensics.
  pub name: String,
  /// The task id for this document under this service.
  pub task_id: i64,
  /// Technical detail of this entry's message for the queried `what` (e.g. localization context).
  pub details: String,
}

/// The deepest report rung: the **paginated list of documents** (paper ids) affected by a specific
/// `(severity, category, what)`. The agent twin of the entry-list screen and the bridge from the
/// macro `what`-breakdown counts to per-article forensics — "*which* papers have this issue?", so
/// an agent can enumerate and then drill into each via the document endpoint. Page with
/// `offset`/`page_size` (default 100, max 1000).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EntryListDto {
  /// Corpus name.
  pub corpus: String,
  /// Service name.
  pub service: String,
  /// The severity reported on.
  pub severity: String,
  /// The category drilled into.
  pub category: String,
  /// The `what` drilled into.
  pub what: String,
  /// Pagination offset echoed back.
  pub offset: i64,
  /// Page size echoed back (the cap actually applied).
  pub page_size: i64,
  /// The affected documents for this page.
  pub entries: Vec<EntryRowDto>,
}

/// Lists the documents affected by a `(corpus, service, severity, category, what)` — the agent twin
/// of the deepest report screen (the entry list). Paginated (`offset`/`page_size`, default 100, max
/// `MAX_REPORT_PAGE_SIZE`); `offset` is capped at `MAX_REPORT_OFFSET` (a `400` beyond it — see
/// P-4). `400` on an unknown severity, `404` on an unknown corpus/service.
#[rocket_okapi::openapi(tag = "Reports")]
#[get("/api/reports/<corpus>/<service>/<severity>/<category>/<what>?<offset>&<page_size>")]
#[allow(clippy::too_many_arguments)]
pub fn api_entry_list(
  corpus: &str,
  service: &str,
  severity: &str,
  category: &str,
  what: &str,
  offset: Option<i64>,
  page_size: Option<i64>,
  pool: &State<DbPool>,
) -> Result<Json<EntryListDto>, Status> {
  if !is_rollup_severity(severity) {
    return Err(Status::BadRequest);
  }
  let offset = offset.unwrap_or(0).max(0);
  // Bound the paginate depth: a deep `OFFSET` is a multi-second scan-and-discard that pins a pooled
  // connection — so reject it on this scriptable surface rather than risk pool saturation (P-4).
  if offset > MAX_REPORT_OFFSET {
    return Err(Status::BadRequest);
  }
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus_record, service_record) = resolve(corpus, service, &mut connection)?;
  // Clamp the page so a `?page_size=0` / huge value can't request an unbounded or empty scan.
  let page_size = page_size.unwrap_or(100).clamp(1, MAX_REPORT_PAGE_SIZE);
  // The entry list is per-task (not a rollup grain), so it comes from the live report path — the
  // same one the human entry-list screen renders, paginated identically.
  let rows = task_report(
    &mut connection,
    TaskReportOptions {
      corpus: &corpus_record,
      service: &service_record,
      severity_opt: Some(severity.to_string()),
      category_opt: Some(category.to_string()),
      what_opt: Some(what.to_string()),
      all_messages: false,
      offset,
      page_size,
    },
  );
  let entries = rows
    .iter()
    .map(|row| EntryRowDto {
      name: row.get("entry_name").cloned().unwrap_or_default(),
      task_id: row
        .get("entry_taskid")
        .and_then(|id| id.parse().ok())
        .unwrap_or(0),
      details: row.get("details").cloned().unwrap_or_default(),
    })
    .collect();
  Ok(Json(EntryListDto {
    corpus: corpus_record.name,
    service: service_record.name,
    severity: severity.to_string(),
    category: category.to_string(),
    what: what.to_string(),
    offset,
    page_size,
    entries,
  }))
}

/// Builds the [`DocumentReportDto`] for one `(corpus, service, document-name)` — the shared backend
/// of the agent endpoint and the human forensic screen, so both show identical status + messages.
/// `404` on an unknown corpus / service / document.
fn document_report(
  corpus: &str,
  service: &str,
  name: &str,
  connection: &mut diesel::PgConnection,
) -> Result<DocumentReportDto, Status> {
  let (corpus, service) = resolve(corpus, service, connection)?;
  let task =
    Task::find_by_name(name, &corpus, &service, connection).map_err(|_| Status::NotFound)?;
  let status = TaskStatus::from_raw(task.status);
  let (records, counts) = task_messages(connection, &task);
  let messages: Vec<MessageDto> = records
    .iter()
    .map(|message| MessageDto {
      severity: message.severity().to_string(),
      category: message.category().to_string(),
      what: message.what().to_string(),
      details: message.details().to_string(),
    })
    .collect();
  let messages_truncated = (messages.len() as i64) < counts.total();
  Ok(DocumentReportDto {
    corpus: corpus.name.clone(),
    service: service.name.clone(),
    name: name.to_string(),
    entry: task.entry.trim_end().to_string(),
    task_id: task.id,
    status: status.to_key(),
    status_code: status.raw(),
    messages,
    message_counts: counts.into(),
    messages_truncated,
    result_url: format!("/entry/{}/{}", service.name, task.id),
    preview_url: format!("/preview/{}/{}/{}", corpus.name, service.name, name),
  })
}

/// The per-article forensic report (agent micro-drill-down): a single document's status for a
/// service plus every worker-log message behind it — "what are the errors of this article?".
/// `<name>` is the document's short name as it appears in reports (e.g. `0801.1234`). `404` on an
/// unknown corpus / service / document.
#[rocket_okapi::openapi(tag = "Reports")]
#[get("/api/corpus/<corpus>/<service>/document/<name>")]
pub fn api_document(
  corpus: &str,
  service: &str,
  name: &str,
  pool: &State<DbPool>,
) -> Result<Json<DocumentReportDto>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  Ok(Json(document_report(
    corpus,
    service,
    name,
    &mut connection,
  )?))
}

/// The per-article forensic **screen** (HTML twin of [`api_document`]): a document's status and the
/// table of every worker-log message behind it. The fast structured view of "what are the errors of
/// this article?" — straight from the parsed `log_*` rows (no result-archive fetch), complementing
/// the rendered `/preview`. `404` on an unknown corpus / service / document. Lives at a top-level
/// `/document/...` path (a sibling of `/preview/...`) so it never collides with the same-shape
/// severity/category report route.
#[get("/document/<corpus>/<service>/<name>")]
pub fn document_report_page(
  corpus: &str,
  service: &str,
  name: &str,
  pool: &State<DbPool>,
) -> Result<(Status, Template), Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  match document_report(corpus, service, name, &mut connection) {
    Ok(report) => {
      let global = serde_json::json!({
        "title": format!("{} — {}/{}", report.name, report.corpus, report.service),
        "description": "Per-article conversion forensics",
      });
      Ok((
        Status::Ok,
        Template::render(
          "document-report",
          rocket_dyn_templates::context! { global, report },
        ),
      ))
    },
    // A human looking up an article by id should get a clear "not found" with a way back to the
    // report (and its search box), not the bare 404 catcher. (The agent twin keeps its plain 404.)
    Err(status) if status == Status::NotFound => {
      let message =
        format!("No article “{name}” in {corpus} / {service}. Check the paper id and try again.");
      let global = serde_json::json!({
        "title": format!("404 · {message}"),
        "description": message.clone(),
      });
      Ok((
        Status::NotFound,
        Template::render(
          "error",
          rocket_dyn_templates::context! {
            global,
            status: 404,
            message,
            back_url: format!("/corpus/{corpus}/{service}"),
            back_label: format!("Back to the {service} report"),
          },
        ),
      ))
    },
    Err(status) => Err(status),
  }
}

/// Query-style entry to the per-article forensic screen: `GET
/// /document/<corpus>/<service>?name=<id>` → **303** to the canonical path URL
/// `/document/<corpus>/<service>/<id>`. This is the **no-JS fallback** target for the
/// service-overview "look up an article" form (the JS enhancement navigates to the path URL
/// directly), so a human can jump to one article's forensics by id without drilling the report
/// ladder — even with scripting off. `400` on a blank `name`; the document itself is validated at
/// the path route (`404` there if unknown).
#[get("/document/<corpus>/<service>?<name>")]
pub fn document_lookup_redirect(
  corpus: &str,
  service: &str,
  name: Option<&str>,
) -> Result<Redirect, Status> {
  let trimmed = name.unwrap_or("").trim();
  if trimmed.is_empty() {
    return Err(Status::BadRequest);
  }
  Ok(Redirect::to(uri!(document_report_page(
    corpus = corpus,
    service = service,
    name = trimmed
  ))))
}

/// Acknowledgement of a rerun: the scope that was marked and who marked it.
#[derive(Debug, Serialize, schemars::JsonSchema)]
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
#[rocket_okapi::openapi(tag = "Reports")]
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
    if !is_valid_rerun_severity(severity, category.is_some()) {
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
  tracing::info!(actor = %actor.owner, corpus, service, severity = ?severity, category = ?category, what = ?what, "rerun via API");
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

/// Result of a pause/resume run-control action (the agent twin of the report screen's Pause/Resume
/// buttons).
#[derive(Serialize, schemars::JsonSchema)]
pub struct RunControlDto {
  /// The corpus acted on.
  pub corpus: String,
  /// The service acted on.
  pub service: String,
  /// `pause` or `resume`.
  pub action: String,
  /// Tasks transitioned — blocked (on pause) or returned to TODO (on resume).
  pub affected: usize,
  /// The token-resolved actor recorded as the initiator.
  pub actor: String,
}

/// Shared core of the agent pause/resume twins: gate via the token-resolved `owner`, run the shared
/// [`crate::frontend::concerns::serve_pause_resume`], wrap the count in a [`RunControlDto`].
fn api_run_control(
  corpus: &str,
  service: &str,
  owner: &str,
  pause: bool,
  pool: &State<DbPool>,
) -> Result<Json<RunControlDto>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let affected =
    crate::frontend::concerns::serve_pause_resume(&mut connection, corpus, service, owner, pause)?;
  Ok(Json(RunControlDto {
    corpus: corpus.to_string(),
    service: service.to_string(),
    action: if pause { "pause" } else { "resume" }.to_string(),
    affected,
    actor: owner.to_string(),
  }))
}

/// **Pause a run** — block every in-progress task (`status >= 0`) of a `(corpus, service)` so the
/// dispatcher stops leasing them. The agent twin of the report screen's "Pause run" button.
/// **Token-gated** via the [`Actor`] guard; `404` on an unknown corpus/service. Returns the count
/// blocked. Reversible with the resume twin.
#[rocket_okapi::openapi(tag = "Reports")]
#[post("/api/reports/<corpus>/<service>/pause")]
pub fn pause_run_api(
  corpus: &str,
  service: &str,
  actor: Actor,
  pool: &State<DbPool>,
) -> Result<Json<RunControlDto>, Status> {
  api_run_control(corpus, service, &actor.owner, true, pool)
}

/// **Resume a run** — return every Blocked task (`status < -5`) of a `(corpus, service)` to TODO so
/// the dispatcher picks them up again. The agent twin of the report screen's "Resume run" button.
/// **Token-gated** via the [`Actor`] guard; `404` on an unknown corpus/service. Returns the count
/// resumed.
#[rocket_okapi::openapi(tag = "Reports")]
#[post("/api/reports/<corpus>/<service>/resume")]
pub fn resume_run_api(
  corpus: &str,
  service: &str,
  actor: Actor,
  pool: &State<DbPool>,
) -> Result<Json<RunControlDto>, Status> {
  api_run_control(corpus, service, &actor.owner, false, pool)
}

/// Acknowledgement for a forced report-rollup refresh: the background [`crate::jobs`] handle to
/// poll.
#[derive(Serialize, schemars::JsonSchema)]
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
/// request (see `docs/archive/REPORT_FRESHNESS.md`). Returns the job handle immediately (`202
/// Accepted`); poll `GET /api/jobs/<job>` for status/health. **Debounced:** a refresh already in
/// flight is reused rather than piled on. **Token-gated** via the [`Actor`] guard (`X-Cortex-Token`
/// / `?token=`); `401` without a valid token.
#[rocket_okapi::openapi(tag = "Reports")]
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

/// The human twin of [`refresh_reports`]: the jobs-dashboard "Refresh reports now" button. **Gated
/// by the signed-in [`AdminSession`] cookie** (the jobs dashboard is signed-in-only; anonymous →
/// sign-in), spawns the same debounced refresh job, and redirects to `/jobs` where the admin
/// watches it run — the async UI pattern (no blocking, no JS).
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[post("/reports/refresh")]
pub fn refresh_reports_human(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<rocket::response::Redirect, AdminReject> {
  let session = require_admin(session)?;
  let uuid = jobs::spawn_report_refresh(pool.inner().clone(), &session.owner)
    .map_err(|_| Status::InternalServerError)?;
  Ok(rocket::response::Redirect::to(format!("/jobs/{uuid}")))
}

/// Acknowledgement for the report-footer "Force refresh": the spawned rebuild job's uuid, for the
/// page to poll `GET /api/jobs/<job>` and reload itself once the rollup is fresh.
#[derive(Serialize)]
pub struct ForceRefreshAck {
  /// The spawned (or debounced-reused) refresh job's external uuid.
  pub job: String,
}

/// Report-footer **"Force refresh"**: rebuild the `report_summary` rollup *now* and return the job
/// uuid as JSON so the page can poll it and reload with fresh numbers (the matview's normal cadence
/// is finalize-drain + at-least-daily). **Gated by the signed-in [`AdminSession`] cookie** — the
/// footer only shows the button to admins, and a missing session is a clean `401` for the XHR
/// rather than an HTML redirect. Debounced: a refresh already in flight is reused, not piled on.
#[post("/reports/refresh/now")]
pub fn force_refresh_reports(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Json<ForceRefreshAck>, Status> {
  let session = session.ok_or(Status::Unauthorized)?;
  let job = jobs::spawn_report_refresh(pool.inner().clone(), &session.owner)
    .map_err(|_| Status::InternalServerError)?;
  Ok(Json(ForceRefreshAck {
    job: job.to_string(),
  }))
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
  session: Option<AdminSession>,
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
    session.is_some(),
  )
}

/// Severity-level report: the categories carrying messages of `severity`.
#[get("/corpus/<corpus_name>/<service_name>/<severity>")]
pub fn severity_service_report(
  corpus_name: String,
  service_name: String,
  severity: String,
  session: Option<AdminSession>,
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
    session.is_some(),
  )
}

/// Severity-level report with paging/all-messages query params.
#[allow(clippy::too_many_arguments)]
#[get("/corpus/<corpus_name>/<service_name>/<severity>?<params..>")]
pub async fn severity_service_report_all(
  corpus_name: String,
  service_name: String,
  severity: String,
  params: Option<ReportParams>,
  session: Option<AdminSession>,
  limiter: &State<LiveReportLimiter>,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  // Gate the expensive live `?all=true` aggregation: acquire a permit BEFORE checking out a pooled
  // connection so a burst can't pin the pool and 503 others (P-2). Paged/non-all requests through
  // this route are rollup-fast and skip the permit.
  let _permit = match params.as_ref().and_then(|p| p.all) {
    Some(true) => Some(limiter.acquire().await?),
    _ => None,
  };
  let mut connection = pooled(pool)?;
  serve_report(
    &mut connection,
    corpus_name,
    service_name,
    Some(severity),
    None,
    None,
    params,
    session.is_some(),
  )
}

/// Category-level report: the `what` classes within a `(severity, category)`.
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>")]
pub fn category_service_report(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  session: Option<AdminSession>,
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
    session.is_some(),
  )
}

/// Category-level report with paging/all-messages query params.
#[allow(clippy::too_many_arguments)]
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>?<params..>")]
pub async fn category_service_report_all(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  params: Option<ReportParams>,
  session: Option<AdminSession>,
  limiter: &State<LiveReportLimiter>,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  // Gate the expensive live `?all=true` aggregation behind the limiter (P-2); see the severity
  // variant above. Cheap paged requests skip the permit.
  let _permit = match params.as_ref().and_then(|p| p.all) {
    Some(true) => Some(limiter.acquire().await?),
    _ => None,
  };
  let mut connection = pooled(pool)?;
  serve_report(
    &mut connection,
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    None,
    params,
    session.is_some(),
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
  session: Option<AdminSession>,
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
    session.is_some(),
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
  session: Option<AdminSession>,
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
    session.is_some(),
  )
}

/// The route set for the reports capability (typed API + the human report screens).
pub fn routes() -> Vec<Route> {
  // NB: the agent report routes (`api_category_report`, `api_what_report`, `rerun_report`,
  // `refresh_reports`) are mounted via `frontend::apidoc` (rocket_okapi).
  routes![
    refresh_reports_human,
    force_refresh_reports,
    top_service_report,
    severity_service_report,
    severity_service_report_all,
    category_service_report,
    category_service_report_all,
    what_service_report,
    what_service_report_all,
    document_report_page,
    document_lookup_redirect
  ]
}
