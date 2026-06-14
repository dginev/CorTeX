//! Common concerns for frontend routes
use crate::rocket::futures::TryFutureExt;
use diesel::PgConnection;
use regex::Regex;
use rocket::fs::NamedFile;
use rocket::http::Status;
use rocket::response::status::{Accepted, NotFound};
use rocket::serde::json::Json;
use rocket_dyn_templates::Template;
use std::collections::HashMap;
use std::str;

use crate::backend::{mark_rerun, progress_report, save_historical_tasks, RerunOptions};
use crate::frontend::cached::task_report;
use crate::frontend::helpers::*;
use crate::frontend::params::{ReportParams, RerunRequestParams, TemplateContext};
use crate::models::{Corpus, HistoricalRun, Service, Task};

lazy_static! {
  static ref STRIP_NAME_REGEX: Regex = Regex::new(r"/[^/]+$").unwrap();
}
/// Placeholder word for unknown filters/fields
pub const UNKNOWN: &str = "_unknown_";

/// Prepare a configurable report for a <corpus,server> pair, reading over the caller-supplied
/// (pooled) `connection` — no per-request fresh `Backend::default()`. `404` on unknown
/// corpus/service.
pub fn serve_report(
  connection: &mut PgConnection,
  corpus_name: String,
  service_name: String,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  params: Option<ReportParams>,
) -> Result<Template, Status> {
  let report_start = chrono::Utc::now();
  let mut context = TemplateContext::default();
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
          historical_run
            .start_time
            .format("%Y-%m-%d %H:%M:%S%.f")
            .to_string(),
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
      Ok(Template::render(template, context))
    } else {
      Err(Status::NotFound)
    }
  } else {
    Err(Status::NotFound)
  }
}

/// Rerun a filtered subset of tasks for a <corpus,service> pair, over the caller-supplied (pooled)
/// `connection`.
pub fn serve_rerun(
  connection: &mut PgConnection,
  corpus_name: String,
  service_name: String,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  let token = rr.token.clone();
  let description = rr.description.clone();
  let auth = &crate::config::config().auth;
  let corpus_name = corpus_name.to_lowercase();
  let service_name = service_name.to_lowercase();

  // Ensure we're given a valid rerun token to rerun, or anyone can wipe the cortex results
  // let token = safe_data_to_string(data).unwrap_or_else(|_| UNKNOWN.to_string()); // reuse old
  // code by setting data to the String
  let user_opt = auth.rerun_tokens.get(&token);
  let user = match user_opt {
    None => return Err(NotFound("Access Denied".to_string())), /* TODO: response.
                                                                 * error(Forbidden, */
    // "Access denied"),
    Some(user) => user,
  };
  println!(
    "-- User {user:?}: Mark for rerun on {corpus_name}/{service_name}/{severity:?}/{category:?}/{what:?}");

  // Run (and measure) the three rerun queries
  let report_start = chrono::Utc::now();
  // Build corpus and service objects
  let corpus = match Corpus::find_by_name(&corpus_name, connection) {
    Err(_) => return Err(NotFound("Access Denied".to_string())), /* TODO: response.
                                                                   * error(Forbidden, */
    // "Access denied"),
    Ok(corpus) => corpus,
  };

  let service = match Service::find_by_name(&service_name, connection) {
    Err(_) => return Err(NotFound("Access Denied".to_string())), /* TODO: response.
                                                                   * error(Forbidden, */
    // "Access denied"),
    Ok(service) => service,
  };
  let rerun_result = mark_rerun(
    connection,
    RerunOptions {
      corpus: &corpus,
      service: &service,
      severity_opt: severity,
      category_opt: category,
      what_opt: what,
      description_opt: Some(description),
      owner_opt: Some(user.to_string()),
    },
  );
  let report_end = chrono::Utc::now();
  let report_duration = (report_end - report_start).num_milliseconds();
  println!("-- User {user:?}: Mark for rerun took {report_duration:?}ms");
  match rerun_result {
    Err(_) => Err(NotFound("Access Denied".to_string())), // TODO: better error message?
    Ok(_) => Ok(Accepted(String::default())),
  }
}

/// Save the historical tasks of a corpus run, for reference, for a <corpus,service> pair
pub fn serve_savetasks(
  connection: &mut PgConnection,
  corpus_name: String,
  service_name: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  let token = rr.token.clone();
  let auth = &crate::config::config().auth;
  let corpus_name = corpus_name.to_lowercase();
  let service_name = service_name.to_lowercase();

  // Ensure we're given a valid rerun token to rerun, or anyone can wipe the cortex results
  // let token = safe_data_to_string(data).unwrap_or_else(|_| UNKNOWN.to_string()); // reuse old
  // code by setting data to the String
  let user_opt = auth.rerun_tokens.get(&token);
  let user = match user_opt {
    None => return Err(NotFound("Access Denied".to_string())),
    Some(user) => user,
  };
  println!("-- User {user:?}: Saving tasks on {corpus_name}/{service_name}");

  // Build corpus and service objects
  let corpus = match Corpus::find_by_name(&corpus_name, connection) {
    Err(e) => return Err(NotFound(format!("{e}"))),
    Ok(corpus) => corpus,
  };

  let service = match Service::find_by_name(&service_name, connection) {
    Err(_) => return Err(NotFound("Access Denied".to_string())),
    Ok(service) => service,
  };
  match save_historical_tasks(connection, &corpus, &service) {
    Err(e) => Err(NotFound(format!("{e}"))),
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
      let entry = task.entry;
      let zip_path = match service_name.as_str() {
        "import" => entry,
        _ => STRIP_NAME_REGEX.replace(&entry, "").to_string() + "/" + &service_name + ".zip",
      };
      if zip_path.is_empty() {
        Err(NotFound(format!(
          "Service {service_name:?} does not have a result
                               for entry {entry_id:?}"
        )))
      } else {
        NamedFile::open(&zip_path)
          .map_err(|_| NotFound("Invalid Zip at path".to_string()))
          .await
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
