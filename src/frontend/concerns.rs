//! Common concerns for frontend routes
use diesel::PgConnection;
use rocket::fs::NamedFile;
use rocket::http::Status;
use rocket::response::status::{Accepted, NotFound};
use rocket::serde::json::Json;
use rocket::tokio::sync::{OwnedSemaphorePermit, Semaphore};
use rocket::{Route, State, get, post, routes};
use rocket_dyn_templates::Template;
use std::collections::HashMap;
use std::str;
use std::sync::Arc;

use crate::backend::{
  DbPool, PooledConn, RerunOptions, live_run_diff, mark_all_blocked, mark_blocked, mark_rerun,
  progress_report, resume_all_blocked, resume_blocked, save_historical_tasks,
};
use crate::frontend::actor::AdminSession;
use crate::frontend::helpers::*;
use crate::frontend::params::{ReportParams, RerunRequestParams, TemplateContext};
use crate::frontend::render::task_report;
use crate::models::{Corpus, HistoricalRun, Service, Task};

/// Placeholder word for unknown filters/fields
pub const UNKNOWN: &str = "_unknown_";

/// A live-computed (non-rollup) report taking at least this long held its pooled connection for the
/// whole query — past this it is logged at `warn` as a pool-saturation risk (KNOWN_ISSUES P-2). The
/// rollup-backed reports return in ~10–90 ms, so this only fires on the expensive `all=true` /
/// large-corpus live aggregations, not normal traffic.
const SLOW_REPORT_WARN_MS: i64 = 2000;

/// Default cap on concurrent expensive **live** (`?all=true`) report aggregations (KNOWN_ISSUES
/// P-2). Each such request holds a pooled connection for its whole multi-second run, so without a
/// bound a burst can exhaust the pool (default 32) and `503` every other request. Four lets a small
/// flurry through while leaving the bulk of the pool for cheap rollup-backed traffic.
pub const MAX_CONCURRENT_LIVE_REPORTS: usize = 4;

/// Bounds how many expensive live (`?all=true`) report aggregations run at once so a burst can't
/// saturate the frontend connection pool and `503` other requests (KNOWN_ISSUES P-2, owner-chosen
/// mitigation). A permit is acquired **before** a pooled connection is checked out, and the
/// `(N+1)`th expensive request **waits asynchronously** (no worker thread or DB connection held)
/// rather than being rejected — it changes no result and rejects nothing, pure blast-radius
/// isolation (DESIGN_PRINCIPLES #6). Cheap rollup-backed and paged reports never take a permit.
// ponytail: a const cap; promote to a `config.web` knob if a deployment needs to tune it.
pub struct LiveReportLimiter(Arc<Semaphore>);

impl LiveReportLimiter {
  /// Builds a limiter capping concurrent live reports at `max_concurrent` (clamped to ≥1).
  pub fn new(max_concurrent: usize) -> Self {
    LiveReportLimiter(Arc::new(Semaphore::new(max_concurrent.max(1))))
  }

  /// Acquires a permit, awaiting asynchronously if all are currently in use. The returned guard
  /// releases the permit on drop. Errs only if the semaphore were closed (it never is).
  pub async fn acquire(&self) -> Result<OwnedSemaphorePermit, Status> {
    self
      .0
      .clone()
      .acquire_owned()
      .await
      .map_err(|_| Status::ServiceUnavailable)
  }
}

impl Default for LiveReportLimiter {
  fn default() -> Self { Self::new(MAX_CONCURRENT_LIVE_REPORTS) }
}

/// Prepare a configurable report for a <corpus,server> pair, reading over the caller-supplied
/// (pooled) `connection` — no per-request fresh `Backend::default()`. `404` on unknown
/// corpus/service.
#[allow(clippy::too_many_arguments)]
pub fn serve_report(
  connection: &mut PgConnection,
  corpus_name: String,
  service_name: String,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  params: Option<ReportParams>,
  is_admin: bool,
) -> Result<Template, Status> {
  let report_start = chrono::Utc::now();
  // `is_admin` gates the footer's admin-only actions (Rerun / Save snapshot) — anonymous viewers
  // see a sign-in card instead.
  let mut context = TemplateContext {
    is_admin,
    ..TemplateContext::default()
  };
  let mut global = HashMap::new();

  let corpus_name = corpus_name.to_lowercase();
  let service_name = service_name.to_lowercase();
  let corpus_result = Corpus::find_by_name(&corpus_name, connection);
  if let Ok(corpus) = corpus_result {
    let service_result = Service::find_by_name(&service_name, connection);
    if let Ok(service) = service_result {
      // Metadata in all reports
      global.insert(
        "title".to_string(),
        "Corpus Report for ".to_string() + &corpus.name,
      );
      global.insert(
        "description".to_string(),
        "An analysis framework for corpora of TeX/LaTeX documents - statistical reports for "
          .to_string()
          + &corpus.name,
      );
      // Render the STORED names (case-insensitive lookup preserves the display case, e.g. `arXiv`),
      // not the lowercased URL params.
      global.insert("corpus_name".to_string(), corpus.name.clone());
      global.insert("corpus_description".to_string(), corpus.description.clone());
      global.insert("service_name".to_string(), service.name.clone());
      global.insert(
        "service_description".to_string(),
        service.description.clone(),
      );
      global.insert("type".to_string(), "Conversion".to_string());
      global.insert("inputformat".to_string(), service.inputformat.clone());
      global.insert("outputformat".to_string(), service.outputformat.clone());

      if let Ok(Some(historical_run)) = HistoricalRun::find_current(&corpus, &service, connection) {
        global.insert(
          "run_start_time".to_string(),
          crate::frontend::helpers::iso_utc(historical_run.start_time),
        );
        global.insert("run_owner".to_string(), historical_run.owner);
        global.insert("run_description".to_string(), historical_run.description);
      }
      let all_messages = match params {
        None => false,
        Some(ref params) => *params.all.as_ref().unwrap_or(&false),
      };
      global.insert("all_messages".to_string(), all_messages.to_string());
      if all_messages {
        // Handlebars has a weird limitation on its #if conditional, can only test for field
        // presence. So...
        global.insert("all_messages_true".to_string(), all_messages.to_string());
      }
      // Whether THIS report's data is matview-backed (the report_summary rollup) or live-computed —
      // the same oracle that gates the serving path. Captured here, before severity/category/what
      // are moved into `task_report` below, so the freshness footer stamps the matview time
      // *iff* the matview was actually used (else the data is current — "just now").
      let used_rollup = crate::backend::report_uses_rollup(
        severity.as_deref(),
        category.as_deref(),
        what.as_deref(),
        all_messages,
      );
      // Cloned so the freshness footer can look up this drill-down's cache slice *after*
      // `task_report` populates it below (the owned `severity` is moved into `task_report`).
      let report_severity = severity.clone();
      match service.inputconverter {
        Some(ref ic_service_name) => {
          global.insert("inputconverter".to_string(), ic_service_name.clone())
        },
        // No declared input converter ⇒ this service's input is the raw imported
        // source, which `serve_entry` serves under the `import` pseudo-service
        // (`/entry/import/<taskid>` → `task.entry`). Defaulting to "import" keeps the
        // report's source link live; the old "missing?" placeholder produced a
        // guaranteed 404 (`/entry/missing%3F/<id>` — no such result archive).
        None => global.insert("inputconverter".to_string(), "import".to_string()),
      };

      let report;
      let template;
      if severity.is_none() {
        // Top-level report
        report = progress_report(connection, corpus.id, service.id);
        // Derive the "Rerun Progress" view server-side, from the same counts: the
        // completed-so-far total and each severity's share of it. report.html.tera
        // renders this directly while a conversion is in progress, replacing the old
        // client-side `progress_report.js`, which scraped the formatted Full Corpus
        // cells and `parseInt`-ed them — truncating any count >= 1000 at the
        // thousands separator (parseInt("3,312") === 3).
        let count = |key: &str| *report.get(key).unwrap_or(&0.0);
        let (np, warn, err, fat) = (
          count("no_problem"),
          count("warning"),
          count("error"),
          count("fatal"),
        );
        let completed = np + warn + err + fat;
        let pct = |n: f64| {
          if completed > 0.0 {
            format!("{:.2}", 100.0 * n / completed)
          } else {
            "0.00".to_string()
          }
        };
        global.insert("rerun_completed".to_string(), completed.to_string());
        global.insert("rerun_no_problem_percent".to_string(), pct(np));
        global.insert("rerun_warning_percent".to_string(), pct(warn));
        global.insert("rerun_error_percent".to_string(), pct(err));
        global.insert("rerun_fatal_percent".to_string(), pct(fat));
        // Unified report's progress bar: share of the non-invalid corpus processed so far
        // (`total` is the non-invalid size — progress_report discounts invalids).
        let total = count("total");
        global.insert(
          "processed_percent".to_string(),
          if total > 0.0 {
            format!("{:.2}", 100.0 * completed / total)
          } else {
            "0.00".to_string()
          },
        );
        // Live run-diff: of the tasks completed so far, how many improved / regressed / stayed the
        // same vs the previous run's baseline snapshot. Read-through cached (the join is the only
        // expensive bit — the headline counts above stay live), shown only once there's a baseline
        // and something completed to compare.
        let ld = live_run_diff(connection, corpus.id, service.id);
        if ld.compared() > 0 {
          global.insert("livediff_improved".to_string(), ld.improved.to_string());
          global.insert("livediff_regressed".to_string(), ld.regressed.to_string());
          global.insert("livediff_unchanged".to_string(), ld.unchanged.to_string());
          global.insert(
            "livediff_reclassified".to_string(),
            ld.reclassified.to_string(),
          );
        }
        // Record the report into the globals
        for (key, val) in report {
          global.insert(key.clone(), val.to_string());
        }
        global.insert(
          "report_time".to_string(),
          crate::frontend::helpers::report_timestamp(),
        );
        template = "report";
      } else if category.is_none() {
        // Severity-level report
        global.insert("severity".to_string(), severity.clone().unwrap_or_default());
        global.insert(
          "highlight".to_string(),
          severity_highlight(&severity.clone().unwrap_or_default()).to_string(),
        );
        let no_problem_kind = match severity {
          Some(ref s) => s == "no_problem",
          None => false,
        };
        let fetched_report = task_report(
          connection,
          &mut global,
          &corpus,
          &service,
          severity,
          None,
          None,
          &params,
        );
        template = if no_problem_kind {
          // Record the report into "entries" vector
          context.entries = Some(fetched_report);
          // And set the task list template
          "task-list-report"
        } else {
          // Record the report into "categories" vector
          context.categories = Some(fetched_report);
          // And set the severity template
          "severity-report"
        };
      } else if what.is_none() {
        // Category-level report
        global.insert("severity".to_string(), severity.clone().unwrap_or_default());
        global.insert(
          "highlight".to_string(),
          severity_highlight(&severity.clone().unwrap_or_default()).to_string(),
        );
        global.insert("category".to_string(), category.clone().unwrap_or_default());
        let no_messages_kind = category.as_deref() == Some("no_messages");
        let fetched_report = task_report(
          connection,
          &mut global,
          &corpus,
          &service,
          severity,
          category,
          None,
          &params,
        );
        template = if no_messages_kind {
          // Record the report into "entries" vector
          context.entries = Some(fetched_report);
          // And set the task list template
          "task-list-report"
        } else {
          // Record the report into "whats" vector
          context.whats = Some(fetched_report);
          // And set the category template
          "category-report"
        };
      } else {
        // What-level report
        global.insert("severity".to_string(), severity.clone().unwrap_or_default());
        global.insert(
          "highlight".to_string(),
          severity_highlight(&severity.clone().unwrap_or_default()).to_string(),
        );
        global.insert("category".to_string(), category.clone().unwrap_or_default());
        global.insert("what".to_string(), what.clone().unwrap_or_default());
        let entries = task_report(
          connection,
          &mut global,
          &corpus,
          &service,
          severity,
          category,
          what,
          &params,
        );
        // Record the report into "entries" vector
        context.entries = Some(entries);
        // And set the task list template
        template = "task-list-report";
      }
      // Pass the globals(reports+metadata) onto the stash
      context.global = global;
      // And pass the handy lambdas
      // And render the correct template
      decorate_uri_encodings(&mut context);

      // Report also the query times
      let report_end = chrono::Utc::now();
      let report_duration = (report_end - report_start).num_milliseconds();
      context
        .global
        .insert("report_duration".to_string(), report_duration.to_string());
      // Observability for the expensive live-aggregation path (KNOWN_ISSUES P-2). A non-rollup
      // report (the `all=true` toggle, a per-task list, or the overview) computes live and holds
      // its pooled connection for the whole query, so a burst can exhaust the pool and 503
      // other requests. Emit the path + duration so an operator can measure how often the
      // slow `all=true` toggle is actually hit and how slow it runs in production — the
      // evidence the P-2 cost call needs — and `warn` past the pool-pinning-risk threshold.
      // Pure observability: no behaviour change. (The metric name/JSON stay stable; this is a
      // structured log via Rocket's tracing subscriber.)
      if !used_rollup {
        let corpus_l = context
          .global
          .get("corpus_name")
          .map(String::as_str)
          .unwrap_or("?");
        let service_l = context
          .global
          .get("service_name")
          .map(String::as_str)
          .unwrap_or("?");
        let severity_l = context
          .global
          .get("severity")
          .map(String::as_str)
          .unwrap_or("-");
        if report_duration >= SLOW_REPORT_WARN_MS {
          tracing::warn!(
            corpus = corpus_l,
            service = service_l,
            severity = severity_l,
            all_messages,
            duration_ms = report_duration,
            "slow live report held a pooled connection for the whole query (P-2 pool-saturation risk)"
          );
        } else {
          tracing::debug!(
            corpus = corpus_l,
            service = service_l,
            severity = severity_l,
            all_messages,
            duration_ms = report_duration,
            "live-computed (non-rollup) report"
          );
        }
      }
      // Report freshness = the **data's** age, and it must match where the data actually came from
      // (KNOWN_ISSUES): a matview-backed report is only as current as its last `report_summary`
      // refresh, but a live-computed one (all-severities `all=true`, per-task lists, the top-level
      // overview) is current as of *now*. Stamping a live report with the stale matview time lies
      // about freshness — so branch on `used_rollup`. The footer renders a colour-coded "data
      // refreshed N ago" from this epoch (localized to the viewer's zone).
      if used_rollup {
        // Cache-backed drill-down: the footer's "generated at" is the slice's `computed_at`.
        if let Some((epoch_ms, human)) = report_severity.as_deref().and_then(|sev| {
          crate::backend::report_cache_computed_at(connection, corpus.id, service.id, sev)
        }) {
          context.global.insert("report_time".to_string(), human);
          context
            .global
            .insert("report_time_epoch".to_string(), epoch_ms.to_string());
        }
      } else {
        // Live data: current as of this request — "just now".
        context.global.insert(
          "report_time".to_string(),
          crate::frontend::helpers::report_timestamp(),
        );
        context.global.insert(
          "report_time_epoch".to_string(),
          chrono::Utc::now().timestamp_millis().to_string(),
        );
      }
      Ok(Template::render(template, context))
    } else {
      Err(Status::NotFound)
    }
  } else {
    Err(Status::NotFound)
  }
}

/// Rerun a filtered subset of tasks for a <corpus,service> pair, over the caller-supplied (pooled)
/// `connection`, attributed to the already-authenticated `owner` (the signed-in admin — the route
/// gates on the [`crate::frontend::actor::AdminSession`] cookie, so there is no token here). `404`
/// on an unknown corpus/service, `500` if the rerun marking fails.
#[allow(clippy::too_many_arguments)]
pub fn serve_rerun(
  connection: &mut PgConnection,
  _pool: &DbPool,
  corpus_name: String,
  service_name: String,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  owner: &str,
  description: &str,
) -> Result<Accepted<String>, Status> {
  let corpus_name = corpus_name.to_lowercase();
  let service_name = service_name.to_lowercase();
  // Reject an out-of-scope / typo'd rerun severity (R-9) up front, instead of letting `mark_rerun`
  // silently mis-scope it to `no_problem` — the same guard the agent `rerun_report` applies, so the
  // human and agent surfaces accept/reject the same set.
  if let Some(ref severity) = severity
    && !crate::frontend::reports::is_valid_rerun_severity(severity, category.is_some())
  {
    return Err(Status::BadRequest);
  }
  // Structured admin-action log (the audit fairing also records actor + outcome to the DB; this is
  // the operational journal line). Emitted here, before the scope is moved into `RerunOptions`.
  tracing::info!(
    actor = owner,
    corpus = corpus_name,
    service = service_name,
    severity = ?severity,
    category = ?category,
    what = ?what,
    "rerun requested"
  );
  let corpus = Corpus::find_by_name(&corpus_name, connection).map_err(|_| Status::NotFound)?;
  let service = Service::find_by_name(&service_name, connection).map_err(|_| Status::NotFound)?;
  let report_start = chrono::Utc::now();
  let rerun_result = mark_rerun(
    connection,
    RerunOptions {
      corpus: &corpus,
      service: &service,
      severity_opt: severity,
      category_opt: category,
      what_opt: what,
      description_opt: Some(description.to_string()),
      owner_opt: Some(owner.to_string()),
    },
  );
  let report_duration = (chrono::Utc::now() - report_start).num_milliseconds();
  match rerun_result {
    Err(error) => {
      tracing::warn!(actor = owner, duration_ms = report_duration, %error, "rerun failed");
      Err(Status::InternalServerError)
    },
    Ok(_) => {
      tracing::info!(
        actor = owner,
        duration_ms = report_duration,
        "rerun committed"
      );
      // The reran (corpus, service) scope's report cache was already invalidated inside the rerun
      // transaction (`mark_rerun`), so its reports repopulate fresh on the next view — no separate,
      // globally-scoped refresh job needed.
      Ok(Accepted(String::default()))
    },
  }
}

/// **Pause** (`pause = true`) or **resume** a whole `(corpus, service)` run, over the
/// caller-supplied (pooled) `connection`, attributed to the authenticated `owner`. Pause blocks
/// every in-progress task (`status >= 0` → Blocked) so the dispatcher stops leasing them; resume
/// returns every Blocked task to TODO so the dispatcher picks them up again — the inverse pair.
/// `404` on an unknown corpus/service, `500` if the status update fails; on success returns the
/// number of tasks affected. Shared by the human `POST /{pause,resume}/<c>/<s>` (cookie) and the
/// agent `POST /api/reports/<c>/<s>/{pause,resume}` (token) — one core, identical effect.
pub fn serve_pause_resume(
  connection: &mut PgConnection,
  corpus_name: &str,
  service_name: &str,
  owner: &str,
  pause: bool,
) -> Result<usize, Status> {
  let corpus_name = corpus_name.to_lowercase();
  let service_name = service_name.to_lowercase();
  let action = if pause { "pause" } else { "resume" };
  // Operational-journal line (the audit fairing also records actor + outcome to the DB).
  tracing::info!(
    actor = owner,
    corpus = corpus_name,
    service = service_name,
    action,
    "run control requested"
  );
  let corpus = Corpus::find_by_name(&corpus_name, connection).map_err(|_| Status::NotFound)?;
  let service = Service::find_by_name(&service_name, connection).map_err(|_| Status::NotFound)?;
  let result = if pause {
    mark_blocked(connection, corpus.id, service.id)
  } else {
    resume_blocked(connection, corpus.id, service.id)
  };
  match result {
    Err(error) => {
      tracing::warn!(actor = owner, action, %error, "run control failed");
      Err(Status::InternalServerError)
    },
    Ok(count) => {
      // No rollup refresh: pause/resume only move tasks across TODO↔Blocked (neither is in the
      // completed-task severity matview); the report's live in-progress tally updates on next load.
      tracing::info!(
        actor = owner,
        action,
        affected = count,
        "run control committed"
      );
      Ok(count)
    },
  }
}

/// Human **pause run** — block every in-progress task of a `(corpus, service)` so the dispatcher
/// stops. Cookie-gated; a plain form POST that redirects back to the report so the admin sees the
/// new state (`401` without a session). The agent twin is `POST /api/reports/<c>/<s>/pause`.
#[post("/pause/<corpus_name>/<service_name>")]
pub fn pause_run(
  corpus_name: String,
  service_name: String,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<rocket::response::Redirect, Status> {
  let session = session.ok_or(Status::Unauthorized)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  serve_pause_resume(
    &mut connection,
    &corpus_name,
    &service_name,
    &session.owner,
    true,
  )?;
  Ok(rocket::response::Redirect::to(format!(
    "/corpus/{corpus_name}/{service_name}"
  )))
}

/// Human **resume run** — return every Blocked task of a `(corpus, service)` to TODO. Cookie-gated;
/// form POST that redirects back to the report. The agent twin is `POST
/// /api/reports/<c>/<s>/resume`.
#[post("/resume/<corpus_name>/<service_name>")]
pub fn resume_run(
  corpus_name: String,
  service_name: String,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<rocket::response::Redirect, Status> {
  let session = session.ok_or(Status::Unauthorized)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  serve_pause_resume(
    &mut connection,
    &corpus_name,
    &service_name,
    &session.owner,
    false,
  )?;
  Ok(rocket::response::Redirect::to(format!(
    "/corpus/{corpus_name}/{service_name}"
  )))
}

/// Pause or resume **all** conversions globally — the dashboard's "Pause/Resume all conversions".
/// The global twin of [`serve_pause_resume`]: across every `(corpus, service)`, pause blocks every
/// in-progress task and resume returns every Blocked task to TODO. Returns the number of tasks
/// moved. Audited by the fairing; fully reversible (the inverse action restores TODO).
pub fn serve_pause_resume_all(
  connection: &mut PgConnection,
  owner: &str,
  pause: bool,
) -> Result<usize, Status> {
  let action = if pause { "pause-all" } else { "resume-all" };
  tracing::info!(actor = owner, action, "global run control requested");
  let result = if pause {
    mark_all_blocked(connection)
  } else {
    resume_all_blocked(connection)
  };
  match result {
    Err(error) => {
      tracing::warn!(actor = owner, action, %error, "global run control failed");
      Err(Status::InternalServerError)
    },
    Ok(count) => {
      tracing::info!(
        actor = owner,
        action,
        affected = count,
        "global run control committed"
      );
      Ok(count)
    },
  }
}

/// Human **pause all conversions** — block every in-progress task fleet-wide so the dispatcher
/// stops leasing new work everywhere. Cookie-gated; redirects to `/admin`. Agent twin:
/// `POST /api/conversions/pause`.
#[post("/pause-all")]
pub fn pause_all(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<rocket::response::Redirect, Status> {
  let session = session.ok_or(Status::Unauthorized)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  serve_pause_resume_all(&mut connection, &session.owner, true)?;
  Ok(rocket::response::Redirect::to("/admin"))
}

/// Human **resume all conversions** — return every Blocked task fleet-wide to TODO. Cookie-gated;
/// redirects to `/admin`. Agent twin: `POST /api/conversions/resume`.
#[post("/resume-all")]
pub fn resume_all(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<rocket::response::Redirect, Status> {
  let session = session.ok_or(Status::Unauthorized)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  serve_pause_resume_all(&mut connection, &session.owner, false)?;
  Ok(rocket::response::Redirect::to("/admin"))
}

/// Save the historical tasks of a corpus run, for reference, for a <corpus,service> pair. Auth is
/// the signed-in [`crate::frontend::actor::AdminSession`] cookie (gated at the route). `404` on an
/// unknown corpus/service.
pub fn serve_savetasks(
  connection: &mut PgConnection,
  corpus_name: String,
  service_name: String,
) -> Result<Accepted<String>, Status> {
  let corpus_name = corpus_name.to_lowercase();
  let service_name = service_name.to_lowercase();
  let corpus = Corpus::find_by_name(&corpus_name, connection).map_err(|_| Status::NotFound)?;
  let service = Service::find_by_name(&service_name, connection).map_err(|_| Status::NotFound)?;
  // A snapshot taken mid-run is a moving target (the in-progress tasks will resolve to a different
  // status moments later), so refuse it while any task is still TODO or Queued (status >= 0). The
  // UI disables the button on the same condition; this is the authoritative guard for both the
  // human and agent paths.
  let progress = progress_report(connection, corpus.id, service.id);
  let in_progress =
    progress.get("todo").copied().unwrap_or(0.0) + progress.get("queued").copied().unwrap_or(0.0);
  if in_progress > 0.0 {
    return Err(Status::Conflict);
  }
  match save_historical_tasks(connection, &corpus, &service) {
    Err(_) => Err(Status::InternalServerError),
    Ok(count) => Ok(Accepted(format!("Saved {count} tasks"))),
  }
}

/// A file download served with a `Content-Disposition` filename derived from the document's **entry
/// name** (e.g. `0811.0417.zip`) instead of the opaque task id in the URL — so corpus curators get
/// an informative filename (UX request 2026-06-16). Wraps a [`NamedFile`] and adds the header.
pub struct EntryDownload {
  file: NamedFile,
  /// The download filename, already sanitised to a safe character set.
  filename: String,
}

impl<'r> rocket::response::Responder<'r, 'static> for EntryDownload {
  fn respond_to(self, request: &'r rocket::Request<'_>) -> rocket::response::Result<'static> {
    let mut response = self.file.respond_to(request)?;
    response.set_raw_header(
      "Content-Disposition",
      format!("attachment; filename=\"{}\"", self.filename),
    );
    Ok(response)
  }
}

/// Restricts a download filename to a safe set (alphanumerics + `.`/`-`/`_`), replacing any other
/// character with `_`. Prevents `Content-Disposition` header injection or odd filenames from a
/// hostile entry path; falls back to `download` if nothing safe remains.
fn sanitize_download_filename(name: &str) -> String {
  let safe: String = name
    .chars()
    .map(|c| {
      if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
        c
      } else {
        '_'
      }
    })
    .collect();
  if safe.is_empty() {
    "download".to_string()
  } else {
    safe
  }
}

/// The informative `Content-Disposition` filename for an entry download. The `import` service
/// serves the **source** archive, which keeps the bare document name (`0811.0417.zip`); every other
/// service serves a **result** archive, whose name appends the service
/// (`0811.0417_tex_to_html.zip`) so a curator can tell the source from a result and several
/// services' results for one document never collide. Sanitised to a safe charset
/// (header-injection-proof).
fn entry_download_filename(document_name: &str, service_name: &str, ext: &str) -> String {
  let stem = if service_name == "import" {
    document_name.to_string()
  } else {
    format!("{document_name}_{service_name}")
  };
  sanitize_download_filename(&format!("{stem}.{ext}"))
}

/// Provide a downloadable file for an entry, looking the task up over the caller-supplied (pooled)
/// `connection` (the borrow ends before the file open). The download is named from the report's
/// "Entry" name + the served file's real extension (not the opaque task id): `<document>.<ext>` for
/// the `import` source archive, `<document>_<service>.<ext>` for a result archive.
pub async fn serve_entry(
  connection: &mut PgConnection,
  service_name: String,
  entry_id: usize,
) -> Result<EntryDownload, NotFound<String>> {
  // Defense-in-depth path-traversal guard: `service_name` is a raw URL segment that gets
  // interpolated into the result-archive **filesystem path** (`{entry_dir}/{service_name}.zip` via
  // `result_archive_path`), with no service-registry lookup on this download path. A real service
  // name is a bare identifier; reject any path separator / `..` / NUL so a crafted segment can
  // never resolve outside the corpus's data directory (a `.zip`-scoped local-file read on a
  // public route).
  if service_name.contains('/')
    || service_name.contains('\\')
    || service_name.contains("..")
    || service_name.contains('\0')
  {
    return Err(NotFound("invalid service".to_string()));
  }
  match Task::find(entry_id as i64, connection) {
    Ok(task) => {
      // The informative download name (the report's "Entry" name), captured before `task.entry`
      // is consumed below.
      let document_name = crate::helpers::entry_document_name(&task.entry);
      let zip_path = if service_name == "import" {
        Some(std::path::PathBuf::from(&task.entry))
      } else {
        // A sandbox's results are name-scoped by its corpus id (F-6), so resolve the task's corpus
        // to match what the sink wrote. One lookup per file-serve request (not a hot path).
        let sandbox_id = Corpus::find_by_id(task.corpus_id, connection)
          .ok()
          .and_then(|corpus| corpus.sandbox_id());
        crate::helpers::result_archive_path(&task.entry, &service_name, sandbox_id)
      };
      match zip_path {
        Some(path) => {
          let file = NamedFile::open(&path)
            .await
            .map_err(|_| NotFound("Invalid Zip at path".to_string()))?;
          // Name the download from the served file's real extension (zip / gz / …). The `import`
          // service serves the **source** archive, which keeps the bare document name
          // (`0811.0417.zip`); any **result** archive appends its service so a curator can tell the
          // source from the `tex_to_html` output (`0811.0417_tex_to_html.zip`) and downloading
          // several services' results for one document never collides (UX request 2026-06-16).
          let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("zip");
          let filename = entry_download_filename(&document_name, &service_name, ext);
          Ok(EntryDownload { file, filename })
        },
        None => Err(NotFound(format!(
          "Service {service_name:?} does not have a result for entry {entry_id:?}"
        ))),
      }
    },
    // Don't echo the raw diesel error to the client (info disclosure on a public route); a missing
    // task is an unremarkable 404 with a generic body.
    Err(_) => Err(NotFound("Task not found".to_string())),
  }
}

/// Serves an entry as a `Template` instance to be preview via a client-side asset renderer, over
/// the caller-supplied (pooled) `connection`.
pub fn serve_entry_preview(
  connection: &mut PgConnection,
  corpus_name: String,
  service_name: String,
  entry_name: String,
) -> Result<Template, NotFound<String>> {
  let report_start = chrono::Utc::now();
  let corpus_name = corpus_name.to_lowercase();
  let mut context = TemplateContext::default();
  let mut global = HashMap::new();

  // Resolve corpus → service → document, each a clean *informative* 404. Previously an unknown
  // corpus/service fell through to render `task-preview` with half-populated globals, which Tera
  // then errored on → a 500 (e.g. the `tex-to-html` vs `tex_to_html` slug mismatch). Robustness
  // mandate: no 500 on the request path — a missing anything is a 404 that says what was missing.
  let corpus = Corpus::find_by_name(&corpus_name, connection)
    .map_err(|_| NotFound(format!("Unknown corpus: {corpus_name}")))?;
  let service = Service::find_by_name(&service_name, connection)
    .map_err(|_| NotFound(format!("Unknown service: {service_name}")))?;
  // Assemble the Download URL from where we will gather the page contents — first, the taskid.
  let task = Task::find_by_name(&entry_name, &corpus, &service, connection).map_err(|_| {
    NotFound(format!(
      "No '{entry_name}' document found in {corpus_name} / {service_name}"
    ))
  })?;
  let download_url = format!("/entry/{}/{}", service_name, task.id);
  global.insert("download_url".to_string(), download_url);

  // Metadata for preview page
  global.insert(
    "title".to_string(),
    "Corpus Report for ".to_string() + &corpus_name,
  );
  global.insert(
    "description".to_string(),
    "An analysis framework for corpora of TeX/LaTeX documents - statistical reports for "
      .to_string()
      + &corpus_name,
  );
  global.insert("corpus_description".to_string(), corpus.description);
  global.insert("service_name".to_string(), service_name);
  global.insert(
    "service_description".to_string(),
    service.description.clone(),
  );
  global.insert("type".to_string(), "Conversion".to_string());
  global.insert("inputformat".to_string(), service.inputformat.clone());
  global.insert("outputformat".to_string(), service.outputformat.clone());
  match service.inputconverter {
    Some(ref ic_service_name) => {
      global.insert("inputconverter".to_string(), ic_service_name.clone())
    },
    // A missing input converter means the input is the raw imported source, served under the
    // `import` pseudo-service. Default to "import" so the source link resolves instead of 404'ing.
    None => global.insert("inputconverter".to_string(), "import".to_string()),
  };
  global.insert(
    "report_time".to_string(),
    crate::frontend::helpers::report_timestamp(),
  );
  global.insert("corpus_name".to_string(), corpus_name);
  global.insert("severity".to_string(), entry_name.clone());
  global.insert("entry_name".to_string(), entry_name);

  // Pass the globals(reports+metadata) onto the stash
  context.global = global;
  // And pass the handy lambdas
  // And render the correct template
  decorate_uri_encodings(&mut context);

  // Report also the query times
  let report_end = chrono::Utc::now();
  let report_duration = (report_end - report_start).num_milliseconds();
  context
    .global
    .insert("report_duration".to_string(), report_duration.to_string());
  Ok(Template::render("task-preview", context))
}

/// Checks out a pooled connection for the document-serving routes, mapping pool exhaustion to their
/// shared `404` error type.
fn pooled(pool: &State<DbPool>) -> Result<PooledConn, NotFound<String>> {
  pool
    .get()
    .map_err(|_| NotFound("database unavailable".to_string()))
}

/// The per-article **preview** screen: renders a shell whose converted-document asset is fetched
/// client-side from the download URL. `404` on an unknown corpus / service / document.
#[get("/preview/<corpus_name>/<service_name>/<entry_name>")]
pub fn preview_entry(
  corpus_name: String,
  service_name: String,
  entry_name: String,
  pool: &State<DbPool>,
) -> Result<Template, NotFound<String>> {
  let mut connection = pooled(pool)?;
  serve_entry_preview(&mut connection, corpus_name, service_name, entry_name)
}

/// **Downloads** a converted document's result archive (streamed, so a large archive never loads
/// into memory). `404` when the task is unknown **or** the archive is missing/unreadable on `/data`
/// — a hostile or unmounted filesystem yields a clean `404`, never a panic or a `500`.
#[post("/entry/<service_name>/<entry_id>")]
pub async fn entry_fetch(
  service_name: String,
  entry_id: usize,
  pool: &State<DbPool>,
) -> Result<EntryDownload, NotFound<String>> {
  let mut connection = pooled(pool)?;
  serve_entry(&mut connection, service_name, entry_id).await
}

/// `GET` twin of [`entry_fetch`] for plain downloadable links (browser-native `<a download>`), used
/// by the run-diff "Task severity changes" table to offer each entry's source archive without the
/// jQuery/AJAX `entry-submit` downloader. Reuses [`serve_entry`]; same `404`-not-`500` discipline
/// on an unknown task or a missing/unreadable archive.
#[get("/entry/<service_name>/<entry_id>")]
pub async fn entry_download(
  service_name: String,
  entry_id: usize,
  pool: &State<DbPool>,
) -> Result<EntryDownload, NotFound<String>> {
  let mut connection = pooled(pool)?;
  serve_entry(&mut connection, service_name, entry_id).await
}

/// The document-serving route set (preview + archive download), migrated out of `bin/frontend.rs`
/// onto the pooled, testable library surface.
/// Shared core of the **human** (cookie-authed) rerun routes: require a signed-in admin, then mark
/// the `(corpus, service[, severity, category, what])` scope for reconversion — attributed to the
/// admin, spawning the debounced rollup refresh off the request path (the agent twin is the
/// token-gated `POST /api/reports/<c>/<s>/rerun`). `401` without a session; `404` on an unknown
/// corpus/service (in `serve_rerun`).
#[allow(clippy::too_many_arguments)]
fn human_rerun(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
  corpus_name: String,
  service_name: String,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  description: &str,
) -> Result<Accepted<String>, Status> {
  let session = session.ok_or(Status::Unauthorized)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  serve_rerun(
    &mut connection,
    pool.inner(),
    corpus_name,
    service_name,
    severity,
    category,
    what,
    &session.owner,
    description,
  )
}

/// Human rerun — whole `(corpus, service)`. Cookie-gated; the rerun modal XHRs a JSON
/// `{description}`.
#[post(
  "/rerun/<corpus_name>/<service_name>",
  format = "application/json",
  data = "<rr>"
)]
pub fn rerun_corpus(
  corpus_name: String,
  service_name: String,
  rr: Json<RerunRequestParams>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Accepted<String>, Status> {
  human_rerun(
    session,
    pool,
    corpus_name,
    service_name,
    None,
    None,
    None,
    &rr.description,
  )
}

/// Human rerun — scoped to a `severity`.
#[post(
  "/rerun/<corpus_name>/<service_name>/<severity>",
  format = "application/json",
  data = "<rr>"
)]
pub fn rerun_severity(
  corpus_name: String,
  service_name: String,
  severity: String,
  rr: Json<RerunRequestParams>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Accepted<String>, Status> {
  human_rerun(
    session,
    pool,
    corpus_name,
    service_name,
    Some(severity),
    None,
    None,
    &rr.description,
  )
}

/// Human rerun — scoped to a `severity`/`category`.
#[post(
  "/rerun/<corpus_name>/<service_name>/<severity>/<category>",
  format = "application/json",
  data = "<rr>"
)]
#[allow(clippy::too_many_arguments)]
pub fn rerun_category(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  rr: Json<RerunRequestParams>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Accepted<String>, Status> {
  human_rerun(
    session,
    pool,
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    None,
    &rr.description,
  )
}

/// Human rerun — scoped to a `severity`/`category`/`what`.
#[post(
  "/rerun/<corpus_name>/<service_name>/<severity>/<category>/<what>",
  format = "application/json",
  data = "<rr>"
)]
#[allow(clippy::too_many_arguments)]
pub fn rerun_what(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  what: String,
  rr: Json<RerunRequestParams>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Accepted<String>, Status> {
  human_rerun(
    session,
    pool,
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    Some(what),
    &rr.description,
  )
}

/// Human **save-snapshot**: freezes the current per-task statuses into `historical_tasks`.
/// Cookie-gated (`401` without a session); `404` on an unknown corpus/service.
#[post("/savetasks/<corpus_name>/<service_name>")]
pub fn savetasks(
  corpus_name: String,
  service_name: String,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Accepted<String>, Status> {
  if session.is_none() {
    return Err(Status::Unauthorized);
  }
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  serve_savetasks(&mut connection, corpus_name.to_lowercase(), service_name)
}

/// A stray **GET** to a rerun URL — which is a POST-only mutation (the rerun modal issues a
/// cookie-gated XHR `POST`) — used to dead-end at a confusing `404`: e.g. a copied/bookmarked form
/// action, or a stale tab navigated directly. Redirect it to the matching report page, where the
/// rerun control lives, instead. `303 See Other`, so the browser switches to a `GET`. The trailing
/// `<scope..>` captures the optional `severity[/category[/what]]` segments **and matches zero
/// segments too**, so the bare `/rerun/<c>/<s>` (rerun-all) is covered by this single route — a
/// separate 2-segment route would *collide* with it and abort Rocket at ignite.
#[get("/rerun/<corpus_name>/<service_name>/<scope..>")]
pub fn rerun_get_redirect(
  corpus_name: &str,
  service_name: &str,
  scope: std::path::PathBuf,
) -> rocket::response::Redirect {
  let scope = scope.display().to_string();
  let target = if scope.is_empty() {
    format!("/corpus/{corpus_name}/{service_name}")
  } else {
    format!("/corpus/{corpus_name}/{service_name}/{scope}")
  };
  rocket::response::Redirect::to(target)
}

/// The document-serving + human rerun/save-snapshot route set, migrated out of `bin/frontend.rs`
/// onto the pooled, testable library surface.
pub fn routes() -> Vec<Route> {
  routes![
    preview_entry,
    entry_fetch,
    entry_download,
    rerun_corpus,
    rerun_severity,
    rerun_category,
    rerun_what,
    rerun_get_redirect,
    pause_run,
    resume_run,
    pause_all,
    resume_all,
    savetasks
  ]
}

#[cfg(test)]
mod live_report_limiter_tests {
  use super::LiveReportLimiter;

  #[test]
  fn caps_then_releases_permits() {
    let limiter = LiveReportLimiter::new(1);
    assert_eq!(limiter.0.available_permits(), 1);
    {
      let _permit = limiter
        .0
        .clone()
        .try_acquire_owned()
        .expect("first permit available");
      assert!(
        limiter.0.clone().try_acquire_owned().is_err(),
        "exhausted at the cap of 1"
      );
    }
    // The guard dropped at the block end, so the permit is restored (RAII release).
    assert_eq!(limiter.0.available_permits(), 1, "permit released on drop");
    assert!(
      limiter.0.clone().try_acquire_owned().is_ok(),
      "reacquire succeeds after release"
    );
  }

  #[test]
  fn zero_is_clamped_to_one() {
    let limiter = LiveReportLimiter::new(0);
    assert_eq!(limiter.0.available_permits(), 1, "a 0 cap can't deadlock");
  }
}

#[cfg(test)]
mod download_filename_tests {
  use super::entry_download_filename;

  #[test]
  fn import_keeps_the_bare_document_name() {
    // The `import` service serves the source archive — no service suffix.
    assert_eq!(
      entry_download_filename("0811.0417", "import", "zip"),
      "0811.0417.zip"
    );
  }

  #[test]
  fn a_result_archive_appends_its_service() {
    // The UX request: /entry/tex_to_html/<id> downloads `<doc>_tex_to_html.zip`.
    assert_eq!(
      entry_download_filename("2105.13573", "tex_to_html", "zip"),
      "2105.13573_tex_to_html.zip"
    );
    // The real extension is honored (e.g. a gz source-derived result).
    assert_eq!(
      entry_download_filename("0801.1234", "ngram", "gz"),
      "0801.1234_ngram.gz"
    );
  }

  #[test]
  fn unsafe_characters_are_sanitised() {
    // A hostile document or service name can't inject into the Content-Disposition header.
    assert_eq!(
      entry_download_filename("0801.1234", "te x/t", "zip"),
      "0801.1234_te_x_t.zip"
    );
  }
}
