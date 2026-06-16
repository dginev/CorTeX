//! Common concerns for frontend routes
use crate::rocket::futures::TryFutureExt;
use diesel::PgConnection;
use rocket::fs::NamedFile;
use rocket::http::Status;
use rocket::response::status::{Accepted, NotFound};
use rocket::{get, post, routes, Route, State};
use rocket_dyn_templates::Template;
use std::collections::HashMap;
use std::str;

use crate::backend::{
  mark_rerun, progress_report, save_historical_tasks, DbPool, PooledConn, RerunOptions,
};
use crate::frontend::helpers::*;
use crate::frontend::params::{ReportParams, TemplateContext};
use crate::frontend::render::task_report;
use crate::models::{Corpus, HistoricalRun, Service, Task};

/// Placeholder word for unknown filters/fields
pub const UNKNOWN: &str = "_unknown_";

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
        "Corpus Report for ".to_string() + &corpus_name,
      );
      global.insert(
        "description".to_string(),
        "An analysis framework for corpora of TeX/LaTeX documents - statistical reports for "
          .to_string()
          + &corpus_name,
      );
      global.insert("corpus_name".to_string(), corpus_name);
      global.insert("corpus_description".to_string(), corpus.description.clone());
      global.insert("service_name".to_string(), service_name);
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
      match service.inputconverter {
        Some(ref ic_service_name) => {
          global.insert("inputconverter".to_string(), ic_service_name.clone())
        },
        None => global.insert("inputconverter".to_string(), "missing?".to_string()),
      };

      let report;
      let template;
      if severity.is_none() {
        // Top-level report
        report = progress_report(connection, corpus.id, service.id);
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
        global.insert("severity".to_string(), severity.clone().unwrap());
        global.insert(
          "highlight".to_string(),
          severity_highlight(&severity.clone().unwrap()).to_string(),
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
        global.insert("severity".to_string(), severity.clone().unwrap());
        global.insert(
          "highlight".to_string(),
          severity_highlight(&severity.clone().unwrap()).to_string(),
        );
        global.insert("category".to_string(), category.clone().unwrap());
        let no_messages_kind = category.is_some() && (category.as_ref().unwrap() == "no_messages");
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
        global.insert("severity".to_string(), severity.clone().unwrap());
        global.insert(
          "highlight".to_string(),
          severity_highlight(&severity.clone().unwrap()).to_string(),
        );
        global.insert("category".to_string(), category.clone().unwrap());
        global.insert("what".to_string(), what.clone().unwrap());
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
      // Report freshness = the **data's** age, and it must match where the data actually came from
      // (KNOWN_ISSUES): a matview-backed report is only as current as its last `report_summary`
      // refresh, but a live-computed one (all-severities `all=true`, per-task lists, the top-level
      // overview) is current as of *now*. Stamping a live report with the stale matview time lies
      // about freshness — so branch on `used_rollup`. The footer renders a colour-coded "data
      // refreshed N ago" from this epoch (localized to the viewer's zone).
      if used_rollup {
        if let Some((epoch_ms, human)) = crate::backend::report_summary_refreshed_at(connection) {
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
  pool: &DbPool,
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
  println!(
    "-- User {owner:?}: Mark for rerun on {corpus_name}/{service_name}/{severity:?}/{category:?}/{what:?}");
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
  println!("-- User {owner:?}: Mark for rerun took {report_duration:?}ms");
  match rerun_result {
    Err(_) => Err(Status::InternalServerError),
    Ok(_) => {
      // Reflect the rerun in reports without blocking this request: refresh the rollup off the
      // request path (debounced, observable via `/api/jobs`). Best-effort — the rerun committed.
      let _ = crate::jobs::spawn_report_refresh(pool.clone(), owner);
      Ok(Accepted(String::default()))
    },
  }
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
  match save_historical_tasks(connection, &corpus, &service) {
    Err(_) => Err(Status::InternalServerError),
    Ok(count) => Ok(Accepted(format!("Saved {count} tasks"))),
  }
}

/// Provide a `NamedFile` for an entry, looking the task up over the caller-supplied (pooled)
/// `connection` (the borrow ends before the file open).
pub async fn serve_entry(
  connection: &mut PgConnection,
  service_name: String,
  entry_id: usize,
) -> Result<NamedFile, NotFound<String>> {
  match Task::find(entry_id as i64, connection) {
    Ok(task) => {
      let zip_path = if service_name == "import" {
        Some(std::path::PathBuf::from(task.entry))
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
          NamedFile::open(&path)
            .map_err(|_| NotFound("Invalid Zip at path".to_string()))
            .await
        },
        None => Err(NotFound(format!(
          "Service {service_name:?} does not have a result for entry {entry_id:?}"
        ))),
      }
    },
    Err(e) => Err(NotFound(format!("Task not found: {e}"))),
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

  let corpus_result = Corpus::find_by_name(&corpus_name, connection);
  if let Ok(corpus) = corpus_result {
    let service_result = Service::find_by_name(&service_name, connection);
    if let Ok(service) = service_result {
      // Assemble the Download URL from where we will gather the page contents
      // First, we need the taskid
      let task = match Task::find_by_name(&entry_name, &corpus, &service, connection) {
        Ok(t) => t,
        Err(e) => return Err(NotFound(e.to_string())),
      };
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
      global.insert("corpus_name".to_string(), corpus_name);
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
        None => global.insert("inputconverter".to_string(), "missing?".to_string()),
      };
      global.insert(
        "report_time".to_string(),
        crate::frontend::helpers::report_timestamp(),
      );
    }
    global.insert("severity".to_string(), entry_name.clone());
    global.insert("entry_name".to_string(), entry_name);
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
) -> Result<NamedFile, NotFound<String>> {
  let mut connection = pooled(pool)?;
  serve_entry(&mut connection, service_name, entry_id).await
}

/// The document-serving route set (preview + archive download), migrated out of `bin/frontend.rs`
/// onto the pooled, testable library surface.
pub fn routes() -> Vec<Route> { routes![preview_entry, entry_fetch] }
