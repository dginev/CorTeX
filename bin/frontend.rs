// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
#![feature(proc_macro_hygiene, decl_macro)]
#![allow(clippy::implicit_hasher, clippy::let_unit_value)]
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate rocket;

use futures::{Future, Stream};
use hyper::header::{HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::Client;
use hyper::{Body, Method, Request};
use hyper_tls::HttpsConnector;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::Header;
use rocket::request::Form;
use rocket::response::status::{Accepted, NotFound};
use rocket::response::{NamedFile, Redirect};
use rocket::Data;
use rocket_contrib::json::Json;
use rocket_contrib::templates::Template;
use serde::{Deserialize, Serialize};
use serde_json;
use tokio_core::reactor::Core;

use std::collections::HashMap;
use std::fs::File;
use std::io::Cursor;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str;

use redis::Commands;
use regex::Regex;
use std::thread;
use std::time::Duration;

use cortex::backend::{Backend, RerunOptions, TaskReportOptions};
use cortex::models::{Corpus, HistoricalRun, RunMetadata, RunMetadataStack, Service, Task};
use cortex::sysinfo;

lazy_static! {
  static ref STRIP_NAME_REGEX: Regex = Regex::new(r"/[^/]+$").unwrap();
}

const TOKEN_LIMIT: u64 = 512;

pub struct CORS();

impl Fairing for CORS {
  fn info(&self) -> Info {
    Info {
      name: "Add CORS headers to requests",
      kind: Kind::Response,
    }
  }

  fn on_response(&self, request: &rocket::Request, response: &mut rocket::Response) {
    if request.method() == rocket::http::Method::Options
      || response.content_type() == Some(rocket::http::ContentType::JSON)
    {
      response.set_header(Header::new("Access-Control-Allow-Origin", "*"));
      response.set_header(Header::new(
        "Access-Control-Allow-Methods",
        "POST, GET, OPTIONS",
      ));
      response.set_header(Header::new("Access-Control-Allow-Headers", "Content-Type"));
      response.set_header(Header::new("Access-Control-Allow-Credentials", "true"));
      response.set_header(Header::new(
        "Content-Security-Policy-Report-Only",
        "default-src https:; report-uri /csp-violation-report-endpoint/",
      ));
    }

    if request.method() == rocket::http::Method::Options {
      response.set_header(rocket::http::ContentType::Plain);
      response.set_sized_body(Cursor::new(""));
    }
  }
}

static UNKNOWN: &str = "_unknown_";

#[derive(Deserialize, Serialize, Debug, Clone)]
struct CortexConfig {
  captcha_secret: String,
  rerun_tokens: HashMap<String, String>,
}

#[derive(Serialize)]
struct TemplateContext {
  global: HashMap<String, String>,
  corpora: Option<Vec<HashMap<String, String>>>,
  services: Option<Vec<HashMap<String, String>>>,
  entries: Option<Vec<HashMap<String, String>>>,
  categories: Option<Vec<HashMap<String, String>>>,
  whats: Option<Vec<HashMap<String, String>>>,
  workers: Option<Vec<HashMap<String, String>>>,
  history: Option<Vec<RunMetadata>>,
  history_serialized: Option<String>,
}
impl Default for TemplateContext {
  fn default() -> Self {
    TemplateContext {
      global: HashMap::new(),
      corpora: None,
      services: None,
      entries: None,
      categories: None,
      whats: None,
      workers: None,
      history: None,
      history_serialized: None,
    }
  }
}

fn aux_load_config() -> CortexConfig {
  let mut config_file = match File::open("config.json") {
    Ok(cfg) => cfg,
    Err(e) => panic!(
      "You need a well-formed JSON config.json file to run the frontend. Error: {}",
      e
    ),
  };
  let mut config_buffer = String::new();
  match config_file.read_to_string(&mut config_buffer) {
    Ok(_) => {},
    Err(e) => panic!(
      "You need a well-formed JSON config.json file to run the frontend. Error: {}",
      e
    ),
  };

  match serde_json::from_str(&config_buffer) {
    Ok(decoded) => decoded,
    Err(e) => panic!(
      "You need a well-formed JSON config.json file to run the frontend. Error: {}",
      e
    ),
  }
}

#[derive(FromForm)]
struct ReportParams {
  all: Option<bool>,
  offset: Option<i64>,
  page_size: Option<i64>,
}

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

  let backend = Backend::default();
  let corpora = backend
    .corpora()
    .iter()
    .map(Corpus::to_hash)
    .collect::<Vec<_>>();

  context.global = global;
  context.corpora = Some(corpora);
  aux_decorate_uri_encodings(&mut context);

  Template::render("overview", context)
}

// Admin interface
#[get("/admin")]
fn admin() -> Template {
  let mut global = HashMap::new();
  global.insert("title".to_string(), "Admin Interface".to_string());
  global.insert(
    "description".to_string(),
    "An analysis framework for corpora of TeX/LaTeX documents - admin interface.".to_string(),
  );
  match sysinfo::report(&mut global) {
    Ok(_) => {},
    Err(e) => println!("Sys report failed: {:?}", e),
  };

  let context = TemplateContext {
    global,
    ..TemplateContext::default()
  };
  Template::render("admin", context)
}

#[get("/workers/<service_name>")]
fn worker_report(service_name: String) -> Result<Template, NotFound<String>> {
  let backend = Backend::default();
  let service_name = aux_uri_unescape(Some(&service_name)).unwrap_or_else(|| UNKNOWN.to_string());
  if let Ok(service) = Service::find_by_name(&service_name, &backend.connection) {
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
      .select_workers(&backend.connection)
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
  let backend = Backend::default();
  let corpus_name = aux_uri_unescape(Some(&corpus_name)).unwrap_or_else(|| UNKNOWN.to_string());
  let corpus_result = Corpus::find_by_name(&corpus_name, &backend.connection);
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

    let services_result = corpus.select_services(&backend.connection);
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
    aux_decorate_uri_encodings(&mut context);
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
) -> Result<Template, NotFound<String>>
{
  serve_report(corpus_name, service_name, None, None, None, None)
}
#[get("/corpus/<corpus_name>/<service_name>/<severity>")]
fn severity_service_report(
  corpus_name: String,
  service_name: String,
  severity: String,
) -> Result<Template, NotFound<String>>
{
  serve_report(corpus_name, service_name, Some(severity), None, None, None)
}
#[get("/corpus/<corpus_name>/<service_name>/<severity>?<params..>")]
fn severity_service_report_all(
  corpus_name: String,
  service_name: String,
  severity: String,
  params: Option<Form<ReportParams>>,
) -> Result<Template, NotFound<String>>
{
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
) -> Result<Template, NotFound<String>>
{
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
  params: Option<Form<ReportParams>>,
) -> Result<Template, NotFound<String>>
{
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
) -> Result<Template, NotFound<String>>
{
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
  params: Option<Form<ReportParams>>,
) -> Result<Template, NotFound<String>>
{
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
) -> Result<Template, NotFound<String>>
{
  let mut context = TemplateContext::default();
  let mut global = HashMap::new();
  let backend = Backend::default();
  let corpus_name = corpus_name.to_lowercase();
  if let Ok(corpus) = Corpus::find_by_name(&corpus_name, &backend.connection) {
    if let Ok(service) = Service::find_by_name(&service_name, &backend.connection) {
      if let Ok(runs) = HistoricalRun::find_by(&corpus, &service, &backend.connection) {
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
    format!(
      "Historical runs of service {} over corpus {}",
      service_name, corpus_name
    ),
  );
  global.insert("service_name".to_string(), service_name);
  global.insert("corpus_name".to_string(), corpus_name);

  context.global = global;
  // And pass the handy lambdas
  // And render the correct template
  aux_decorate_uri_encodings(&mut context);

  // Report also the query times
  Ok(Template::render("history", context))
}

#[get("/preview/<corpus_name>/<service_name>/<entry_name>")]
fn preview_entry(
  corpus_name: String,
  service_name: String,
  entry_name: String,
) -> Result<Template, NotFound<String>>
{
  let report_start = time::get_time();
  let corpus_name = corpus_name.to_lowercase();
  let mut context = TemplateContext::default();
  let mut global = HashMap::new();
  let backend = Backend::default();

  let corpus_result = Corpus::find_by_name(&corpus_name, &backend.connection);
  if let Ok(corpus) = corpus_result {
    let service_result = Service::find_by_name(&service_name, &backend.connection);
    if let Ok(service) = service_result {
      // Assemble the Download URL from where we will gather the page contents (after captcha is
      // confirmed) First, we need the taskid
      let task = match Task::find_by_name(&entry_name, &corpus, &service, &backend.connection) {
        Ok(t) => t,
        Err(e) => return Err(NotFound(e.to_string())),
      };
      let download_url = format!("/entry/{}/{}", service_name, task.id.to_string());
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
      global.insert("report_time".to_string(), time::now().rfc822().to_string());
    }
    global.insert("severity".to_string(), entry_name.clone());
    global.insert("entry_name".to_string(), entry_name);
  }

  // Pass the globals(reports+metadata) onto the stash
  context.global = global;
  // And pass the handy lambdas
  // And render the correct template
  aux_decorate_uri_encodings(&mut context);

  // Report also the query times
  let report_end = time::get_time();
  let report_duration = (report_end - report_start).num_milliseconds();
  context
    .global
    .insert("report_duration".to_string(), report_duration.to_string());
  Ok(Template::render("task-preview", context))
}

#[post("/entry/<service_name>/<entry_id>", data = "<data>")]
fn entry_fetch(service_name: String, entry_id: usize, data: Data) -> Result<NamedFile, Redirect> {
  // Any secrets reside in config.json
  let cortex_config = aux_load_config();
  let data = safe_data_to_string(data).unwrap_or_default(); // reuse old code by setting data to the String
  let g_recaptcha_response_string = if data.len() > 21 {
    let data = &data[21..];
    data.replace("&g-recaptcha-response=", "")
  } else {
    UNKNOWN.to_owned()
  };
  let g_recaptcha_response = &g_recaptcha_response_string;
  // Check if we hve the g_recaptcha_response in Redis, then reuse
  let mut redis_opt;
  let quota: usize = match redis::Client::open("redis://127.0.0.1/") {
    Err(_) => return Err(Redirect::to("/")), // TODO: Err(NotFound(format!("redis unreachable")))},
    Ok(redis_client) => match redis_client.get_connection() {
      Err(_) => return Err(Redirect::to("/")), /* TODO: Err(NotFound(format!("redis
                                                 * unreachable")))}, */
      Ok(mut redis_connection) => {
        let quota = redis_connection.get(g_recaptcha_response).unwrap_or(0);
        redis_opt = Some(redis_connection);
        quota
      },
    },
  };

  println!("Response: {:?}", g_recaptcha_response);
  println!("Quota: {:?}", quota);
  let captcha_verified = if quota > 0 {
    if let Some(ref mut redis_connection) = redis_opt {
      println!("Using local redis quota.");
      if quota == 1 {
        // Remove if last
        redis_connection.del(g_recaptcha_response).unwrap_or(());
      } else {
        // We have quota available, decrement it
        redis_connection
          .set(g_recaptcha_response, quota - 1)
          .unwrap_or(());
      }
      // And allow operation
      true
    } else {
      false // no redis, no access.
    }
  } else {
    // expired quota, check with google
    let check_val = aux_check_captcha(g_recaptcha_response, &cortex_config.captcha_secret);
    println!("Google validity: {:?}", check_val);
    if check_val {
      if let Some(ref mut redis_connection) = redis_opt {
        // Add a reuse quota if things check out, 19 more downloads
        redis_connection.set(g_recaptcha_response, 19).unwrap_or(());
      }
    }
    check_val
  };
  println!("Captcha validity: {:?}", captcha_verified);

  // If you are not human, you have no business here.
  if !captcha_verified {
    if g_recaptcha_response != UNKNOWN {
      return Err(Redirect::to("/expire_captcha"));
    } else {
      return Err(Redirect::to("/"));
    }
  }

  let backend = Backend::default();
  match Task::find(entry_id as i64, &backend.connection) {
    Ok(task) => {
      let entry = task.entry;
      let zip_path = match service_name.as_str() {
        "import" => entry,
        _ => STRIP_NAME_REGEX.replace(&entry, "").to_string() + "/" + &service_name + ".zip",
      };
      if zip_path.is_empty() {
        Err(Redirect::to("/")) // TODO : Err(NotFound(format!("Service {:?} does not have a result
                               // for entry {:?}", service_name,
                               // entry_id)))
      } else {
        NamedFile::open(&zip_path).map_err(|_| Redirect::to("/"))
      }
    },
    Err(e) => {
      dbg!(e); // TODO: Handle these better
      Err(Redirect::to("/"))
    },
  }
}

//Expire captchas
#[get("/expire_captcha")]
fn expire_captcha() -> Result<Template, NotFound<String>> {
  let mut context = TemplateContext::default();
  let mut global = HashMap::new();
  global.insert(
    "description".to_string(),
    "Expire captcha cache for CorTeX.".to_string(),
  );
  context.global = global;
  Ok(Template::render("expire_captcha", context))
}

// Rerun queries
#[derive(Serialize, Deserialize)]
pub struct RerunRequest {
  pub token: String,
  pub description: String,
}

#[post(
  "/rerun/<corpus_name>/<service_name>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_corpus(
  corpus_name: String,
  service_name: String,
  rr: Json<RerunRequest>,
) -> Result<Accepted<String>, NotFound<String>>
{
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
  rr: Json<RerunRequest>,
) -> Result<Accepted<String>, NotFound<String>>
{
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
  rr: Json<RerunRequest>,
) -> Result<Accepted<String>, NotFound<String>>
{
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
  rr: Json<RerunRequest>,
) -> Result<Accepted<String>, NotFound<String>>
{
  serve_rerun(
    corpus_name,
    service_name,
    Some(severity),
    Some(category),
    Some(what),
    rr,
  )
}

#[get("/favicon.ico")]
fn favicon() -> Result<NamedFile, NotFound<String>> {
  let path = Path::new("public/").join("favicon.ico");
  NamedFile::open(&path).map_err(|_| NotFound(format!("Bad path: {:?}", path)))
}

#[get("/robots.txt")]
fn robots() -> Result<NamedFile, NotFound<String>> {
  let path = Path::new("public/").join("robots.txt");
  NamedFile::open(&path).map_err(|_| NotFound(format!("Bad path: {:?}", path)))
}

#[get("/public/<file..>")]
fn files(file: PathBuf) -> Result<NamedFile, NotFound<String>> {
  let path = Path::new("public/").join(file);
  NamedFile::open(&path).map_err(|_| NotFound(format!("Bad path: {:?}", path)))
}

fn rocket() -> rocket::Rocket {
  rocket::ignite()
    .mount(
      "/",
      routes![
        root,
        admin,
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
        expire_captcha,
        historical_runs
      ],
    )
    .attach(Template::fairing())
    .attach(CORS())
}

fn main() {
  let _ = thread::spawn(move || {
    cache_worker();
  });
  rocket().launch();
}

fn serve_report(
  corpus_name: String,
  service_name: String,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  params: Option<Form<ReportParams>>,
) -> Result<Template, NotFound<String>>
{
  let report_start = time::get_time();
  let mut context = TemplateContext::default();
  let mut global = HashMap::new();
  let backend = Backend::default();

  let corpus_name = corpus_name.to_lowercase();
  let service_name = service_name.to_lowercase();
  let corpus_result = Corpus::find_by_name(&corpus_name, &backend.connection);
  if let Ok(corpus) = corpus_result {
    let service_result = Service::find_by_name(&service_name, &backend.connection);
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

      if let Ok(Some(historical_run)) =
        HistoricalRun::find_current(&corpus, &service, &backend.connection)
      {
        global.insert(
          "run_start_time".to_string(),
          historical_run
            .start_time
            .format("%Y-%m-%d %H:%M:%S")
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
        report = backend.progress_report(&corpus, &service);
        // Record the report into the globals
        for (key, val) in report {
          global.insert(key.clone(), val.to_string());
        }
        global.insert("report_time".to_string(), time::now().rfc822().to_string());
        template = "report";
      } else if category.is_none() {
        // Severity-level report
        global.insert("severity".to_string(), severity.clone().unwrap());
        global.insert(
          "highlight".to_string(),
          aux_severity_highlight(&severity.clone().unwrap()).to_string(),
        );
        template = if severity.is_some() && (severity.as_ref().unwrap() == "no_problem") {
          let entries = aux_task_report(
            &mut global,
            &corpus,
            &service,
            severity,
            None,
            None,
            &params,
          );
          // Record the report into "entries" vector
          context.entries = Some(entries);
          // And set the task list template
          "task-list-report"
        } else {
          let categories = aux_task_report(
            &mut global,
            &corpus,
            &service,
            severity,
            None,
            None,
            &params,
          );
          // Record the report into "categories" vector
          context.categories = Some(categories);
          // And set the severity template
          "severity-report"
        };
      } else if what.is_none() {
        // Category-level report
        global.insert("severity".to_string(), severity.clone().unwrap());
        global.insert(
          "highlight".to_string(),
          aux_severity_highlight(&severity.clone().unwrap()).to_string(),
        );
        global.insert("category".to_string(), category.clone().unwrap());
        if category.is_some() && (category.as_ref().unwrap() == "no_messages") {
          let entries = aux_task_report(
            &mut global,
            &corpus,
            &service,
            severity,
            category,
            None,
            &params,
          );
          // Record the report into "entries" vector
          context.entries = Some(entries);
          // And set the task list template
          template = "task-list-report";
        } else {
          let whats = aux_task_report(
            &mut global,
            &corpus,
            &service,
            severity,
            category,
            None,
            &params,
          );
          // Record the report into "whats" vector
          context.whats = Some(whats);
          // And set the category template
          template = "category-report";
        }
      } else {
        // What-level report
        global.insert("severity".to_string(), severity.clone().unwrap());
        global.insert(
          "highlight".to_string(),
          aux_severity_highlight(&severity.clone().unwrap()).to_string(),
        );
        global.insert("category".to_string(), category.clone().unwrap());
        global.insert("what".to_string(), what.clone().unwrap());
        let entries = aux_task_report(
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
      aux_decorate_uri_encodings(&mut context);

      // Report also the query times
      let report_end = time::get_time();
      let report_duration = (report_end - report_start).num_milliseconds();
      context
        .global
        .insert("report_duration".to_string(), report_duration.to_string());
      Ok(Template::render(template, context))
    } else {
      Err(NotFound(format!(
        "Service {} does not exist.",
        &service_name
      )))
    }
  } else {
    Err(NotFound(format!("Corpus {} does not exist.", &corpus_name)))
  }
}

fn serve_rerun(
  corpus_name: String,
  service_name: String,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  rr: Json<RerunRequest>,
) -> Result<Accepted<String>, NotFound<String>>
{
  let token = rr.token.clone();
  let description = rr.description.clone();
  let config = aux_load_config();
  let corpus_name = corpus_name.to_lowercase();
  let service_name = service_name.to_lowercase();

  // Ensure we're given a valid rerun token to rerun, or anyone can wipe the cortex results
  // let token = safe_data_to_string(data).unwrap_or_else(|_| UNKNOWN.to_string()); // reuse old
  // code by setting data to the String
  let user_opt = config.rerun_tokens.get(&token);
  let user = match user_opt {
    None => return Err(NotFound("Access Denied".to_string())), /* TODO: response.
                                                                 * error(Forbidden, */
    // "Access denied"),
    Some(user) => user,
  };
  println!(
    "-- User {:?}: Mark for rerun on {:?}/{:?}/{:?}/{:?}/{:?}",
    user, corpus_name, service_name, severity, category, what
  );

  // Run (and measure) the three rerun queries
  let report_start = time::get_time();
  let backend = Backend::default();
  // Build corpus and service objects
  let corpus = match Corpus::find_by_name(&corpus_name, &backend.connection) {
    Err(_) => return Err(NotFound("Access Denied".to_string())), /* TODO: response.
                                                                   * error(Forbidden, */
    // "Access denied"),
    Ok(corpus) => corpus,
  };

  let service = match Service::find_by_name(&service_name, &backend.connection) {
    Err(_) => return Err(NotFound("Access Denied".to_string())), /* TODO: response.
                                                                   * error(Forbidden, */
    // "Access denied"),
    Ok(service) => service,
  };
  let rerun_result = backend.mark_rerun(RerunOptions {
    corpus: &corpus,
    service: &service,
    severity_opt: severity,
    category_opt: category,
    what_opt: what,
    description_opt: Some(description),
    owner_opt: Some(user.to_string()),
  });
  let report_end = time::get_time();
  let report_duration = (report_end - report_start).num_milliseconds();
  println!(
    "-- User {:?}: Mark for rerun took {:?}ms",
    user, report_duration
  );
  match rerun_result {
    Err(_) => Err(NotFound("Access Denied".to_string())), // TODO: better error message?
    Ok(_) => Ok(Accepted(None)),
  }
}

fn aux_severity_highlight(severity: &str) -> &str {
  match severity {
    // Bootstrap highlight classes
    "no_problem" => "success",
    "warning" => "warning",
    "error" => "error",
    "fatal" => "danger",
    "invalid" => "info",
    _ => "info",
  }
}
fn aux_uri_unescape(param: Option<&str>) -> Option<String> {
  match param {
    None => None,
    Some(param_encoded) => {
      let mut param_decoded: String = param_encoded.to_owned();
      // TODO: This could/should be done faster by using lazy_static!
      for &(original, replacement) in &[
        ("%3A", ":"),
        ("%2F", "/"),
        ("%24", "$"),
        ("%2E", "."),
        ("%21", "!"),
        ("%40", "@"),
      ] {
        param_decoded = param_decoded.replace(original, replacement);
      }
      Some(
        percent_encoding::percent_decode(param_decoded.as_bytes())
          .decode_utf8_lossy()
          .into_owned(),
      )
    },
  }
}
fn aux_uri_escape(param: Option<String>) -> Option<String> {
  match param {
    None => None,
    Some(param_pure) => {
      let mut param_encoded: String =
        percent_encoding::utf8_percent_encode(&param_pure, percent_encoding::NON_ALPHANUMERIC)
          .collect::<String>();
      // TODO: This could/should be done faster by using lazy_static!
      for &(original, replacement) in &[
        (":", "%3A"),
        ("/", "%2F"),
        ("\\", "%5C"),
        ("$", "%24"),
        (".", "%2E"),
        ("!", "%21"),
        ("@", "%40"),
      ] {
        param_encoded = param_encoded.replace(original, replacement);
      }
      // if param_pure != param_encoded {
      //   println!("Encoded {:?} to {:?}", param_pure, param_encoded);
      // } else {
      //   println!("No encoding needed: {:?}", param_pure);
      // }
      Some(param_encoded)
    },
  }
}
fn aux_decorate_uri_encodings(context: &mut TemplateContext) {
  for inner_vec in &mut [
    &mut context.corpora,
    &mut context.services,
    &mut context.entries,
    &mut context.categories,
    &mut context.whats,
  ] {
    if let Some(ref mut inner_vec_data) = **inner_vec {
      for subhash in inner_vec_data {
        let mut uri_decorations = vec![];
        for (subkey, subval) in subhash.iter() {
          uri_decorations.push((
            subkey.to_string() + "_uri",
            aux_uri_escape(Some(subval.to_string())).unwrap(),
          ));
        }
        for (decoration_key, decoration_val) in uri_decorations {
          subhash.insert(decoration_key, decoration_val);
        }
      }
    }
  }
  // global is handled separately
  let mut uri_decorations = vec![];
  for (subkey, subval) in &context.global {
    uri_decorations.push((
      subkey.to_string() + "_uri",
      aux_uri_escape(Some(subval.to_string())).unwrap(),
    ));
  }
  for (decoration_key, decoration_val) in uri_decorations {
    context.global.insert(decoration_key, decoration_val);
  }
  let mut current_link = String::new();
  {
    if let Some(corpus_name) = context.global.get("corpus_name_uri") {
      if let Some(service_name) = context.global.get("service_name_uri") {
        current_link = format!("/corpus/{}/{}/", corpus_name, service_name);
        if let Some(severity) = context.global.get("severity_uri") {
          current_link.push_str(severity);
          current_link.push('/');
          if let Some(category) = context.global.get("category_uri") {
            current_link.push_str(category);
            current_link.push('/');
            if let Some(what) = context.global.get("what_uri") {
              current_link.push_str(what);
            }
          }
        }
      }
    }
  }
  if !current_link.is_empty() {
    context
      .global
      .insert("current_link_uri".to_string(), current_link);
  }
}

#[derive(Deserialize)]
struct IsSuccess {
  success: bool,
}

fn aux_check_captcha(g_recaptcha_response: &str, captcha_secret: &str) -> bool {
  let mut core = match Core::new() {
    Ok(c) => c,
    _ => return false,
  };
  let https = HttpsConnector::new(4).expect("TLS initialization failed");
  let client = Client::builder().build::<_, hyper::Body>(https);

  let mut verified = false;
  let url_with_query = "https://www.google.com/recaptcha/api/siteverify?secret=".to_string()
    + captcha_secret
    + "&response="
    + g_recaptcha_response;
  let json_str = format!(
    "{{\"secret\":\"{:?}\",\"response\":\"{:?}\"}}",
    captcha_secret, g_recaptcha_response
  );
  let req_url = match url_with_query.parse() {
    Ok(parsed) => parsed,
    _ => return false,
  };
  let json_len = json_str.len();
  let mut req = Request::new(Body::from(json_str));
  *req.method_mut() = Method::POST;
  *req.uri_mut() = req_url;
  req.headers_mut().insert(
    CONTENT_TYPE,
    HeaderValue::from_static("application/javascript"),
  );
  req
    .headers_mut()
    .insert(CONTENT_LENGTH, HeaderValue::from(json_len));

  let post = client
    .request(req)
    .and_then(|res| res.into_body().concat2());
  let posted = match core.run(post) {
    Ok(posted_data) => match str::from_utf8(&posted_data) {
      Ok(posted_str) => posted_str.to_string(),
      Err(e) => {
        println!("err: {}", e);
        return false;
      },
    },
    Err(e) => {
      println!("err: {}", e);
      return false;
    },
  };
  let json_decoded: Result<IsSuccess, _> = serde_json::from_str(&posted);
  if let Ok(response_json) = json_decoded {
    if response_json.success {
      verified = true;
    }
  }

  verified
}

fn aux_task_report(
  global: &mut HashMap<String, String>,
  corpus: &Corpus,
  service: &Service,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  params: &Option<Form<ReportParams>>,
) -> Vec<HashMap<String, String>>
{
  let all_messages = match params {
    None => false,
    Some(ref params) => *params.all.as_ref().unwrap_or(&false),
  };
  let offset = match params {
    None => 0,
    Some(ref params) => *params.offset.as_ref().unwrap_or(&0),
  };
  let page_size = match params {
    None => 100,
    Some(ref params) => *params.page_size.as_ref().unwrap_or(&100),
  };
  let fetched_report;
  let mut time_val: String = time::now().rfc822().to_string();

  let mut redis_connection = match redis::Client::open("redis://127.0.0.1/") {
    Ok(redis_client) => match redis_client.get_connection() {
      Ok(rc) => Some(rc),
      _ => None,
    },
    _ => None,
  };

  let mut cache_key = String::new();
  let mut cache_key_time = String::new();
  let cached_report: Vec<HashMap<String, String>> =
    if what.is_some() || severity == Some("no_problem".to_string()) {
      vec![]
    } else {
      // Levels 1-3 get cached, except no_problem pages
      let key_tail = match severity.clone() {
        Some(severity) => {
          let cat_tail = match category.clone() {
            Some(category) => {
              let what_tail = match what.clone() {
                Some(what) => "_".to_string() + &what,
                None => String::new(),
              };
              "_".to_string() + &category + &what_tail
            },
            None => String::new(),
          };
          "_".to_string() + &severity + &cat_tail
        },
        None => String::new(),
      } + if all_messages { "_all_messages" } else { "" };
      cache_key = corpus.id.to_string() + "_" + &service.id.to_string() + &key_tail;
      cache_key_time = cache_key.clone() + "_time";
      let cache_val: String = if let Some(ref mut rc) = redis_connection {
        rc.get(cache_key.clone()).unwrap_or_default()
      } else {
        String::new()
      };
      if cache_val.is_empty() {
        vec![]
      } else {
        serde_json::from_str(&cache_val).unwrap_or_default()
      }
    };

  if cached_report.is_empty() {
    let backend = Backend::default();
    fetched_report = backend.task_report(TaskReportOptions {
      corpus,
      service,
      severity_opt: severity.clone(),
      category_opt: category,
      what_opt: what.clone(),
      all_messages,
      offset,
      page_size,
    });
    if what.is_none() && severity != Some("no_problem".to_string()) {
      let report_json: String = serde_json::to_string(&fetched_report).unwrap();
      // don't cache the task list pages

      if let Some(ref mut rc) = redis_connection {
        let _: () = rc.set(cache_key, report_json).unwrap();
      }

      if let Some(ref mut rc) = redis_connection {
        let _: () = rc.set(cache_key_time, time_val.clone()).unwrap();
      }
    }
  } else {
    // Get the report time, so that the user knows where the data is coming from
    time_val = if let Some(ref mut rc) = redis_connection {
      match rc.get(cache_key_time) {
        Ok(tval) => tval,
        Err(_) => time::now().rfc822().to_string(),
      }
    } else {
      time::now().rfc822().to_string()
    };
    fetched_report = cached_report;
  }

  // Setup the return

  let from_offset = offset;
  let to_offset = offset + page_size;
  global.insert("from_offset".to_string(), from_offset.to_string());
  if from_offset >= page_size {
    // TODO: properly do tera ifs?
    global.insert("offset_min_false".to_string(), "true".to_string());
    global.insert(
      "prev_offset".to_string(),
      (from_offset - page_size).to_string(),
    );
  }

  if fetched_report.len() >= page_size as usize {
    global.insert("offset_max_false".to_string(), "true".to_string());
  }
  global.insert(
    "next_offset".to_string(),
    (from_offset + page_size).to_string(),
  );

  global.insert("offset".to_string(), offset.to_string());
  global.insert("page_size".to_string(), page_size.to_string());
  global.insert("to_offset".to_string(), to_offset.to_string());
  global.insert("report_time".to_string(), time_val);

  fetched_report
}

fn cache_worker() {
  let redis_client = match redis::Client::open("redis://127.0.0.1/") {
    Ok(client) => client,
    _ => panic!("Redis connection failed, please boot up redis and restart the frontend!"),
  };
  let mut redis_connection = match redis_client.get_connection() {
    Ok(conn) => conn,
    _ => panic!("Redis connection failed, please boot up redis and restart the frontend!"),
  };
  let mut queued_cache: HashMap<String, usize> = HashMap::new();
  loop {
    // Keep a fresh backend connection on each invalidation pass.
    let backend = Backend::default();
    let mut global_stub: HashMap<String, String> = HashMap::new();
    // each corpus+service (non-import)
    for corpus in &backend.corpora() {
      if let Ok(services) = corpus.select_services(&backend.connection) {
        for service in &services {
          if service.name == "import" {
            continue;
          }
          println!(
            "[cache worker] Examining corpus {:?}, service {:?}",
            corpus.name, service.name
          );
          // Pages we'll cache:
          let report = backend.progress_report(corpus, service);
          let zero: f64 = 0.0;
          let huge: usize = 999_999;
          let queued_count_f64: f64 =
            report.get("queued").unwrap_or(&zero) + report.get("todo").unwrap_or(&zero);
          let queued_count: usize = queued_count_f64 as usize;
          let key_base: String = corpus.id.to_string() + "_" + &service.id.to_string();
          // Only recompute the inner pages if we are seeing a change / first visit, on the top
          // corpus+service level
          if *queued_cache.get(&key_base).unwrap_or(&huge) != queued_count {
            println!("[cache worker] state changed, invalidating ...");
            // first cache the count for the next check:
            queued_cache.insert(key_base.clone(), queued_count);
            // each reported severity (fatal, warning, error)
            for severity in &["invalid", "fatal", "error", "warning", "no_problem", "info"] {
              // most importantly, DEL the key from Redis!
              let key_severity = key_base.clone() + "_" + severity;
              println!("[cache worker] DEL {:?}", key_severity);
              redis_connection.del(key_severity.clone()).unwrap_or(());
              // also the combined-severity page for this category
              let key_severity_all = key_severity.clone() + "_all_messages";
              println!("[cache worker] DEL {:?}", key_severity_all);
              redis_connection.del(key_severity_all.clone()).unwrap_or(());
              if "no_problem" == *severity {
                continue;
              }

              // cache category page
              thread::sleep(Duration::new(1, 0)); // Courtesy sleep of 1 second.
              let category_report = aux_task_report(
                &mut global_stub,
                corpus,
                service,
                Some(severity.to_string()),
                None,
                None,
                &None,
              );
              // for each category, cache the what page
              for cat_hash in &category_report {
                let string_empty = String::new();
                let category = cat_hash.get("name").unwrap_or(&string_empty);
                if category.is_empty() || (category == "total") {
                  continue;
                }

                let key_category = key_severity.clone() + "_" + category;
                println!("[cache worker] DEL {:?}", key_category);
                redis_connection.del(key_category.clone()).unwrap_or(());
                // also the combined-severity page for this `what` class
                let key_category_all = key_category + "_all_messages";
                println!("[cache worker] DEL {:?}", key_category_all);
                redis_connection.del(key_category_all.clone()).unwrap_or(());

                let _ = aux_task_report(
                  &mut global_stub,
                  corpus,
                  service,
                  Some(severity.to_string()),
                  Some(category.to_string()),
                  None,
                  &None,
                );
              }
            }
          }
        }
      }
    }
    // Take two minutes before we recheck.
    thread::sleep(Duration::new(120, 0));
  }
}

fn safe_data_to_string(data: Data) -> Result<String, std::io::Error> {
  let mut stream = data.open().take(TOKEN_LIMIT);
  let mut string = String::with_capacity((TOKEN_LIMIT / 2) as usize);
  stream.read_to_string(&mut string)?; // do we need str::from_utf8(token_bytes)
  Ok(string)
}
