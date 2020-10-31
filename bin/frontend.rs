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
extern crate google_signin;

use rocket::request::Form;
use rocket::response::status::{Accepted, NotFound};
use rocket::response::{NamedFile, Redirect};
use rocket::Data;
use rocket_contrib::json::Json;
use rocket_contrib::templates::Template;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process;
use std::time::SystemTime;

use cortex::backend::Backend;
use cortex::concerns::CortexInsertable;
use cortex::frontend::concerns::{
  serve_entry, serve_entry_preview, serve_report, serve_rerun, UNKNOWN,
};
use cortex::frontend::cors::CORS;
use cortex::frontend::helpers::*;
use cortex::frontend::params::{AuthParams, ReportParams, RerunRequestParams, TemplateContext};
use cortex::models::{
  Corpus, HistoricalRun, NewCorpus, NewUser, RunMetadata, RunMetadataStack, Service,
  User,
};

#[get("/")]
fn root() -> Template {
  let mut context = TemplateContext::default();
  let mut global = global_defaults();
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

#[get("/dashboard?<params..>")]
fn admin_dashboard(params: Form<AuthParams>) -> Result<Template, Redirect> {
  // Recommended: Let the crate handle everything for you
  let mut current_user = None;
  let id_info = match verify_oauth(&params.token) {
    None => return Err(Redirect::to("/")),
    Some(id_info) => id_info,
  };
  let display = if let Some(ref name) = id_info.name {
    name.to_owned()
  } else {
    String::new()
  };
  let email = id_info.email.unwrap();
  // TODO: If we ever have too many users, this will be too slow. For now, simple enough.
  let backend = Backend::default();
  let users = backend.users();
  let message = if users.is_empty() {
    let first_admin = NewUser {
      admin: true,
      email: email.to_owned(),
      display,
      first_seen: SystemTime::now(),
      last_seen: SystemTime::now(),
    };
    if backend.add(&first_admin).is_ok() {
      format!("Registered admin user for {:?}", email)
    } else {
      format!("Failed to create user for {:?}", email)
    }
  } else {
    // is this user known?
    if let Ok(u) = User::find_by_email(&email, &backend.connection) {
      let is_admin = if u.admin { "(admin)" } else { "" };
      current_user = Some(u);
      format!("Signed in as {:?} {}", email, is_admin)
    } else {
      let new_viewer = NewUser {
        admin: false,
        email: email.to_owned(),
        display,
        first_seen: SystemTime::now(),
        last_seen: SystemTime::now(),
      };
      if backend.add(&new_viewer).is_ok() {
        format!("Registered viewer-level user for {:?}", email)
      } else {
        format!("Failed to create user for {:?}", email)
      }
    }
  };
  if current_user.is_none() {
    // did we end up registering a new user? If so, look it up
    if let Ok(u) = User::find_by_email(&email, &backend.connection) {
      current_user = Some(u);
    }
  }
  // having a registered user, mark as seen
  if let Some(ref u) = current_user {
    u.touch(&backend.connection).expect("DB ran away");
  }
  let mut global = global_defaults();
  global.insert("message".to_string(), message);
  global.insert("title".to_string(), "Admin Interface".to_string());
  global.insert(
    "description".to_string(),
    "An analysis framework for corpora of TeX/LaTeX documents - admin interface.".to_string(),
  );
  Ok(Template::render(
    "admin",
    dashboard_context(backend, current_user, global),
  ))
}

#[post(
  "/dashboard_task/add_corpus?<params..>",
  format = "application/json",
  data = "<corpus_spec>"
)]
fn dashboard_task_add_corpus(
  params: Form<AuthParams>,
  corpus_spec: Json<NewCorpus>,
) -> Result<Accepted<String>, NotFound<String>>
{
  println!("who: {:?}", params);
  let id_info = match verify_oauth(&params.token) {
    None => return Err(NotFound("could not verify OAuth login".to_owned())),
    Some(id_info) => id_info,
  };
  let backend = Backend::default();
  let user = match User::find_by_email(id_info.email.as_ref().unwrap(), &backend.connection) {
    Ok(u) => u,
    _ => return Err(NotFound("no registered user for your email".to_owned())),
  };
  if user.admin {
    println!("dashboard task data: {:?}", corpus_spec);
    let message = match corpus_spec.create(&backend.connection) {
      Ok(_) => "successfully added corpus to DB",
      Err(_) => "failed to create corpus in DB",
    };
    Ok(Accepted(Some(message.to_owned())))
  } else {
    Err(NotFound(
      "User must be admin to execute dashboard actions".to_owned(),
    ))
  }
}

#[get("/workers/<service_name>")]
fn worker_report(service_name: String) -> Result<Template, NotFound<String>> {
  let backend = Backend::default();
  let service_name = uri_unescape(Some(&service_name)).unwrap_or_else(|| UNKNOWN.to_string());
  if let Ok(service) = Service::find_by_name(&service_name, &backend.connection) {
    let mut global = global_defaults();
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
    global.insert("corpus_name".to_string(), "all".to_string());
    global.insert("service_name".to_string(), service_name.to_string());
    global.insert(
      "service_description".to_string(),
      service.description.clone(),
    );
    // uri links lead to root, since this is a global overview
    global.insert("corpus_name_uri".to_string(), "../".to_string());
    global.insert("service_name_uri".to_string(), "../".to_string());
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
    let mut global = global_defaults();
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
  let mut global = global_defaults();
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
  serve_entry_preview(corpus_name, service_name, entry_name)
}

#[post("/entry/<service_name>/<entry_id>", data = "<data>")]
fn entry_fetch(
  service_name: String,
  entry_id: usize,
  data: Data,
) -> Result<NamedFile, NotFound<String>>
{
  serve_entry(service_name, entry_id, data)
}

//Expire captchas
#[get("/expire_captcha")]
fn expire_captcha() -> Result<Template, NotFound<String>> {
  let mut context = TemplateContext::default();
  let mut global = global_defaults();
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
        admin_dashboard,
        dashboard_task_add_corpus,
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

fn main() -> Result<(), Box<dyn Error>> {
  let backend = Backend::default();
  // Ensure all cortex daemon services are running in parallel before we sping up the frontend
  // Redis cache expiration logic, for report pages
  let cw_opt = backend
    .ensure_daemon("cache_worker")
    .expect("Couldn't spin up cache worker");
  // Corpus registration init worker, should run on the machine storing the data (currently same as frontend machine)
  let initw_opt = backend
    .ensure_daemon("init_worker")
    .expect("Couldn't spin up init worker");

  // Dispatcher manager, for service execution logic
  let dispatcher_opt = backend
    .ensure_daemon("dispatcher")
    .expect("Couldn't spin up dispatcher");
  backend
    .override_daemon_record("frontend".to_owned(), process::id())
    .expect("Could not register the process id with the backend, aborting...");

  // Finally, start up the web service
  let rocket_error = rocket().launch();
  // If we failed to boot / exited dirty, destroy the children
  if let Some(mut cw) = cw_opt {
    cw.kill()?;
    cw.wait()?;
  }
  if let Some(mut iw) = initw_opt {
    iw.kill()?;
    iw.wait()?;
  }
  if let Some(mut dispatcher) = dispatcher_opt {
    dispatcher.kill()?;
    dispatcher.wait()?;
  }
  drop(rocket_error);
  Ok(())
}
