// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
#![feature(proc_macro_hygiene, decl_macro)]
#![allow(clippy::implicit_hasher, clippy::let_unit_value)]
#[macro_use]
extern crate rocket;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread;

use rocket::fs::NamedFile;
use rocket::futures::TryFutureExt;
use rocket::response::status::{Accepted, NotFound};
use rocket::serde::json::Json;
use rocket_dyn_templates::Template;

use cortex::backend::Backend;
use cortex::frontend::cached::cache_worker;
use cortex::frontend::concerns::{
  serve_entry, serve_entry_preview, serve_report, serve_rerun, serve_savetasks, UNKNOWN,
};
use cortex::frontend::cors::CORS;
use cortex::frontend::helpers::*;
use cortex::frontend::params::{ReportParams, RerunRequestParams, TemplateContext};
use cortex::models::{Corpus, HistoricalRun, RunMetadata, RunMetadataStack, Service};

#[get("/")]
fn root() -> Template {
  let mut context = TemplateContext::default();
  let mut global = HashMap::new();
  global.insert(
    "title".to_string(),
    "Overview of available Corpora".to_string(),
  );
  global.insert(
    "description".to_string(),
    "An analysis framework for corpora of TeX/LaTeX documents - overview page".to_string(),
  );

  let mut backend = Backend::default();
  let corpora = backend
    .corpora()
    .iter()
    .map(Corpus::to_hash)
    .collect::<Vec<_>>();

  context.global = global;
  context.corpora = Some(corpora);
  decorate_uri_encodings(&mut context);

  Template::render("overview", context)
}

#[get("/workers/<service_name>")]
fn worker_report(service_name: String) -> Result<Template, NotFound<String>> {
  let mut backend = Backend::default();
  let service_name = uri_unescape(Some(&service_name)).unwrap_or_else(|| UNKNOWN.to_string());
  if let Ok(service) = Service::find_by_name(&service_name, &mut backend.connection) {
    let mut global = HashMap::new();
    global.insert(
      "title".to_string(),
      format!("Worker report for service {} ", &service_name),
    );
    global.insert(
      "description".to_string(),
      format!(
        "Worker report for service {} as registered by the CorTeX dispatcher",
        &service_name
      ),
    );
    global.insert("service_name".to_string(), service_name.to_string());
    global.insert(
      "service_description".to_string(),
      service.description.clone(),
    );
    let mut context = TemplateContext {
      global,
      ..TemplateContext::default()
    };

    let workers = service
      .select_workers(&mut backend.connection)
      .unwrap()
      .into_iter()
      .map(Into::into)
      .collect();
    context.workers = Some(workers);
    Ok(Template::render("workers", context))
  } else {
    Err(NotFound(String::from("no such service")))
  }
}

#[get("/corpus/<corpus_name>")]
fn corpus(corpus_name: String) -> Result<Template, NotFound<String>> {
  let mut backend = Backend::default();
  let corpus_name = uri_unescape(Some(&corpus_name)).unwrap_or_else(|| UNKNOWN.to_string());
  let corpus_result = Corpus::find_by_name(&corpus_name, &mut backend.connection);
  if let Ok(corpus) = corpus_result {
    let mut global = HashMap::new();
    global.insert(
      "title".to_string(),
      "Registered services for ".to_string() + &corpus_name,
    );
    global.insert(
      "description".to_string(),
      "An analysis framework for corpora of TeX/LaTeX documents - registered services for "
        .to_string()
        + &corpus_name,
    );
    global.insert("corpus_name".to_string(), corpus_name);
    global.insert("corpus_description".to_string(), corpus.description.clone());
    let mut context = TemplateContext {
      global,
      ..TemplateContext::default()
    };

    let services_result = corpus.select_services(&mut backend.connection);
    if let Ok(backend_services) = services_result {
      let services = backend_services
        .iter()
        .map(Service::to_hash)
        .collect::<Vec<_>>();
      let mut service_reports = Vec::new();
      for service in services {
        // TODO: Report on the service status when we improve on the service report UX
        // service.insert("status".to_string(), "Running".to_string());
        service_reports.push(service);
      }
      context.services = Some(service_reports);
    }
    decorate_uri_encodings(&mut context);
    return Ok(Template::render("services", context));
  }
  Err(NotFound(format!(
    "Corpus {} is not registered",
    &corpus_name
  )))
}

#[get("/corpus/<corpus_name>/<service_name>")]
fn top_service_report(
  corpus_name: String,
  service_name: String,
) -> Result<Template, NotFound<String>> {
  serve_report(corpus_name, service_name, None, None, None, None)
}
#[get("/corpus/<corpus_name>/<service_name>/<severity>")]
fn severity_service_report(
  corpus_name: String,
  service_name: String,
  severity: String,
) -> Result<Template, NotFound<String>> {
  serve_report(corpus_name, service_name, Some(severity), None, None, None)
}
#[get("/corpus/<corpus_name>/<service_name>/<severity>?<params..>")]
fn severity_service_report_all(
  corpus_name: String,
  service_name: String,
  severity: String,
  params: Option<ReportParams>,
) -> Result<Template, NotFound<String>> {
  serve_report(
    corpus_name,
    service_name,
    Some(severity),
    None,
    None,
    params,
  )
}
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>")]
fn category_service_report(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
) -> Result<Template, NotFound<String>> {
  serve_report(
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    None,
    None,
  )
}
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>?<params..>")]
fn category_service_report_all(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  params: Option<ReportParams>,
) -> Result<Template, NotFound<String>> {
  serve_report(
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    None,
    params,
  )
}

#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>/<what>")]
fn what_service_report(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  what: String,
) -> Result<Template, NotFound<String>> {
  serve_report(
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    Some(what),
    None,
  )
}
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>/<what>?<params..>")]
fn what_service_report_all(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  what: String,
  params: Option<ReportParams>,
) -> Result<Template, NotFound<String>> {
  serve_report(
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    Some(what),
    params,
  )
}

#[get("/history/<corpus_name>/<service_name>")]
fn historical_runs(
  corpus_name: String,
  service_name: String,
) -> Result<Template, NotFound<String>> {
  let mut context = TemplateContext::default();
  let mut global = HashMap::new();
  let mut backend = Backend::default();
  let corpus_name = corpus_name.to_lowercase();
  if let Ok(corpus) = Corpus::find_by_name(&corpus_name, &mut backend.connection) {
    if let Ok(service) = Service::find_by_name(&service_name, &mut backend.connection) {
      if let Ok(runs) = HistoricalRun::find_by(&corpus, &service, &mut backend.connection) {
        let runs_meta = runs
          .into_iter()
          .map(Into::into)
          .collect::<Vec<RunMetadata>>();
        let runs_meta_stack: Vec<RunMetadataStack> = RunMetadataStack::transform(&runs_meta);
        context.history_serialized = Some(serde_json::to_string(&runs_meta_stack).unwrap());
        global.insert(
          "history_length".to_string(),
          runs_meta
            .iter()
            .filter(|run| !run.end_time.is_empty())
            .count()
            .to_string(),
        );
        context.history = Some(runs_meta);
      }
    }
  }

  // Pass the globals(reports+metadata) onto the stash
  global.insert(
    "description".to_string(),
    format!("Historical runs of service {service_name} over corpus {corpus_name}"),
  );
  global.insert("service_name".to_string(), service_name);
  global.insert("corpus_name".to_string(), corpus_name);

  context.global = global;
  // And pass the handy lambdas
  // And render the correct template
  decorate_uri_encodings(&mut context);

  // Report also the query times
  Ok(Template::render("history", context))
}

#[get("/diff-history/<corpus_name>/<service_name>")]
fn diff_historical_tasks(
  corpus_name: String,
  service_name: String,
) -> Result<Template, NotFound<String>> {
  let mut context = TemplateContext::default();
  let mut global = HashMap::new();
  let mut backend = Backend::default();
  let corpus_name = corpus_name.to_lowercase();
  if let Ok(corpus) = Corpus::find_by_name(&corpus_name, &mut backend.connection) {
    if let Ok(service) = Service::find_by_name(&service_name, &mut backend.connection) {
      context.diff_report = Some(backend.list_task_diffs(&corpus, &service));
    }
  }
  // Pass the globals(reports+metadata) onto the stash
  global.insert(
    "description".to_string(),
    format!(
      "Diffs for historical task severity runs of service {service_name} over corpus {corpus_name}"
    ),
  );
  global.insert("service_name".to_string(), service_name);
  global.insert("corpus_name".to_string(), corpus_name);

  context.global = global;
  // And pass the handy lambdas
  // And render the correct template
  decorate_uri_encodings(&mut context);
  // Report also the task diff statistics
  Ok(Template::render("diff-history", context))
}

#[get("/preview/<corpus_name>/<service_name>/<entry_name>")]
fn preview_entry(
  corpus_name: String,
  service_name: String,
  entry_name: String,
) -> Result<Template, NotFound<String>> {
  serve_entry_preview(corpus_name, service_name, entry_name)
}

#[post("/entry/<service_name>/<entry_id>")]
async fn entry_fetch(service_name: String, entry_id: usize) -> Result<NamedFile, NotFound<String>> {
  serve_entry(service_name, entry_id).await
}

#[post(
  "/rerun/<corpus_name>/<service_name>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_corpus(
  corpus_name: String,
  service_name: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  let corpus_name = corpus_name.to_lowercase();
  serve_rerun(corpus_name, service_name, None, None, None, rr)
}

#[post(
  "/rerun/<corpus_name>/<service_name>/<severity>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_severity(
  corpus_name: String,
  service_name: String,
  severity: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  serve_rerun(corpus_name, service_name, Some(severity), None, None, rr)
}

#[post(
  "/rerun/<corpus_name>/<service_name>/<severity>/<category>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_category(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  serve_rerun(
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    None,
    rr,
  )
}

#[post(
  "/rerun/<corpus_name>/<service_name>/<severity>/<category>/<what>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_what(
  corpus_name: String,
  service_name: String,
  severity: String,
  category: String,
  what: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  serve_rerun(
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    Some(what),
    rr,
  )
}

#[post(
  "/savetasks/<corpus_name>/<service_name>",
  format = "application/json",
  data = "<rr>"
)]
fn savetasks(
  corpus_name: String,
  service_name: String,
  rr: Json<RerunRequestParams>,
) -> Result<Accepted<String>, NotFound<String>> {
  let corpus_name = corpus_name.to_lowercase();
  serve_savetasks(corpus_name, service_name, rr)
}

#[get("/favicon.ico")]
async fn favicon() -> Result<NamedFile, NotFound<String>> {
  let path = Path::new("public/").join("favicon.ico");
  NamedFile::open(&path)
    .map_err(|_| NotFound(format!("Bad path: {path:?}")))
    .await
}

#[get("/robots.txt")]
async fn robots() -> Result<NamedFile, NotFound<String>> {
  let path = Path::new("public/").join("robots.txt");
  NamedFile::open(&path)
    .map_err(|_| NotFound(format!("Bad path: {path:?}")))
    .await
}

#[get("/public/<file..>")]
async fn files(file: PathBuf) -> Result<NamedFile, NotFound<String>> {
  let path = Path::new("public/").join(file);
  NamedFile::open(&path)
    .map_err(|_| NotFound(format!("Bad path: {path:?}")))
    .await
}

#[launch]
fn rocket() -> _ {
  // cache worker in parallel to the main service thread
  let _ = thread::spawn(move || {
    cache_worker();
  });
  rocket::build()
    .mount(
      "/",
      routes![
        root,
        corpus,
        favicon,
        robots,
        files,
        worker_report,
        top_service_report,
        severity_service_report,
        category_service_report,
        what_service_report,
        severity_service_report_all,
        category_service_report_all,
        what_service_report_all,
        preview_entry,
        entry_fetch,
        rerun_corpus,
        rerun_severity,
        rerun_category,
        rerun_what,
        historical_runs,
        diff_historical_tasks,
        savetasks
      ],
    )
    .attach(Template::fairing())
    .attach(CORS())
}
