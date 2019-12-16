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

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::str;
use std::thread;

use redis::Commands;
use regex::Regex;

use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::Header;
use rocket::request::Form;
use rocket::response::status::{Accepted, NotFound};
use rocket::response::{NamedFile, Redirect};
use rocket::Data;
use rocket_contrib::json::Json;
use rocket_contrib::templates::Template;

use cortex::backend::Backend;
use cortex::frontend::cached::cache_worker;
use cortex::frontend::captcha::{check_captcha, safe_data_to_string};
use cortex::frontend::concerns::{serve_report, serve_rerun};
use cortex::frontend::helpers::*;
use cortex::frontend::params::{ReportParams, RerunRequestParams, TemplateContext};
use cortex::models::{Corpus, HistoricalRun, RunMetadata, RunMetadataStack, Service, Task};
use cortex::sysinfo;

lazy_static! {
  static ref STRIP_NAME_REGEX: Regex = Regex::new(r"/[^/]+$").unwrap();
}

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
  decorate_uri_encodings(&mut context);

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
  let service_name = uri_unescape(Some(&service_name)).unwrap_or_else(|| UNKNOWN.to_string());
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
  let corpus_name = uri_unescape(Some(&corpus_name)).unwrap_or_else(|| UNKNOWN.to_string());
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
  decorate_uri_encodings(&mut context);

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
  decorate_uri_encodings(&mut context);

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
  let cortex_config = load_config();
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
    let check_val = check_captcha(g_recaptcha_response, &cortex_config.captcha_secret);
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

#[post(
  "/rerun/<corpus_name>/<service_name>",
  format = "application/json",
  data = "<rr>"
)]
fn rerun_corpus(
  corpus_name: String,
  service_name: String,
  rr: Json<RerunRequestParams>,
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
  rr: Json<RerunRequestParams>,
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
  rr: Json<RerunRequestParams>,
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
  rr: Json<RerunRequestParams>,
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
  // cache worker in parallel to the main service thread
  let _ = thread::spawn(move || {
    cache_worker();
  });
  rocket().launch();
}
