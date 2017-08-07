// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(plugin)]
#![plugin(rocket_codegen)]

extern crate rocket;
extern crate rocket_contrib;
extern crate url;
extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate tokio_core;

extern crate rustc_serialize; // TODO: Migrate FULLY to serde
extern crate serde_json;
#[macro_use] extern crate serde_derive;

extern crate cortex;
extern crate time;
extern crate regex;
extern crate redis;

use rocket::response::{NamedFile, Responder, Redirect};
use rocket::response::status::NotFound;
use rocket_contrib::Template;
use futures::{Future, Stream};
use tokio_core::reactor::Core;
use hyper::Client;
use hyper::{Method, Request};
use hyper::header::{ContentLength, ContentType};
use hyper_tls::HttpsConnector;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::{Read};
use std::str;

use std::thread;
use std::time::Duration;
use regex::Regex;
use redis::Commands;

use rustc_serialize::json;
use cortex::sysinfo;
use cortex::backend::Backend;
use cortex::data::{Corpus, CortexORM, Service, Task};

static UNKNOWN: &'static str = "_unknown_";

#[derive(RustcDecodable, RustcEncodable, Debug, Clone)]
struct CortexConfig {
  captcha_secret: String,
  rerun_tokens: HashMap<String, String>,
}

#[derive(Serialize)]
struct TemplateContext {
  global: HashMap<String, String>,
  corpora: Option<Vec<HashMap<String,String>>>,
  services: Option<Vec<HashMap<String,String>>>,
  entries: Option<Vec<HashMap<String,String>>>,
  categories: Option<Vec<HashMap<String,String>>>,
  whats: Option<Vec<HashMap<String,String>>>,
}
impl Default for TemplateContext {
  fn default() -> Self {
    TemplateContext {
      global: HashMap::new(),
      corpora: None,
      services: None,
      entries: None,
      categories: None,
      whats: None
    }
  }
}

fn aux_load_config() -> Result<CortexConfig, String> {
  let mut config_file = try!(File::open("examples/config.json").map_err(|e| e.to_string()));
  let mut config_buffer = String::new();
  try!(config_file.read_to_string(&mut config_buffer).map_err(|e| e.to_string()));

  json::decode(&config_buffer).map_err(|e| e.to_string())
}

#[get("/")]
fn root() -> Template {

  let mut context = TemplateContext::default();
  let mut global = HashMap::new();
  global.insert("title".to_string(), "Framework Overview".to_string());
  global.insert("description".to_string(), "An analysis framework for corpora of TeX/LaTeX documents - overview.".to_string());

  let backend = Backend::default();
  let corpora = backend.corpora().iter().map(|c| c.to_hash()).collect::<Vec<_>>();

  context.global = global;
  context.corpora = Some(corpora);
  aux_decorate_uri_encodings(&mut context);

  Template::render("cortex-overview", context)
}

// Admin interface
#[get("/admin")]
fn admin() -> Template {
  let mut global = HashMap::new();
  global.insert("title".to_string(), "Admin Interface".to_string());
  global.insert("description".to_string(), "An analysis framework for corpora of TeX/LaTeX documents - admin interface.".to_string());
  match sysinfo::report(&mut global) {
    Ok(_) => {},
    Err(e) => println!("Sys report failed: {:?}", e)
  };

  let context = TemplateContext {
    global: global,
    ..TemplateContext::default()
  };
  Template::render("cortex-admin", context)
}

#[get("/corpus/<corpus_name>")]
fn corpus(corpus_name: String) -> Result<Template, NotFound<String>> {
    let backend = Backend::default();
    let corpus_name = aux_uri_unescape(Some(&corpus_name)).unwrap_or(UNKNOWN.to_string());
    let corpus_result = Corpus{id: None, name: corpus_name.to_string(), path : String::new(), complex : true}.select_by_key(&backend.connection);
    if let Ok(Some(corpus)) = corpus_result {
      let mut global = HashMap::new();
      global.insert("title".to_string(), "Registered services for ".to_string() + &corpus_name);
      global.insert("description".to_string(), "An analysis framework for corpora of TeX/LaTeX documents - registered services for ".to_string()+ &corpus_name);
      global.insert("corpus_name".to_string(), corpus_name.to_string());
      let mut context = TemplateContext { global: global, ..TemplateContext::default()};

      let services_result = corpus.select_services(&backend.connection);
      if let Ok(backend_services) = services_result {
        let services = backend_services.iter()
                                       .map(|s| s.to_hash()).collect::<Vec<_>>();
        let mut service_reports = Vec::new();
        for mut service in services {
          service.insert("status".to_string(),"Running".to_string());
          service_reports.push(service);
        }
        context.services = Some(service_reports);
      }
      aux_decorate_uri_encodings(&mut context);
      return Ok(Template::render("cortex-services", context))
    }
  Err(NotFound(format!("Corpus {} is not registered", &corpus_name)))
}

#[get("/corpus/<corpus_name>/<service_name>")]
fn top_service_report(corpus_name: String, service_name: String) -> Result<Template, NotFound<String>> {
  serve_report(corpus_name, service_name, None, None, None)
}
#[get("/corpus/<corpus_name>/<service_name>/<severity>")]
fn severity_service_report(corpus_name: String, service_name: String, severity: String) -> Result<Template, NotFound<String>> {
  serve_report(corpus_name, service_name, Some(severity), None, None)
}
#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>")]
fn category_service_report(corpus_name: String, service_name: String, severity: String, category: String) -> Result<Template, NotFound<String>> {
  serve_report(corpus_name, service_name, Some(severity), Some(category), None)
}

#[get("/corpus/<corpus_name>/<service_name>/<severity>/<category>/<what>")]
fn what_service_report(corpus_name: String, service_name: String, severity: String, category: String, what: String) -> Result<Template, NotFound<String>> {
  serve_report(corpus_name, service_name, Some(severity), Some(category), Some(what))
}

// Note, the docs warn "data: Vec<u8>" is a DDoS vector - https://api.rocket.rs/rocket/data/trait.FromData.html
// since this is a research-first implementation, i will abstain from doing this perfectly now and run with the slurp.

#[post("/entry/<service_name>/<entry_id>", data="<data>")]
fn entry_fetch(service_name: String, entry_id: usize, data: Vec<u8>) -> Result<NamedFile, Redirect> {
  // Any secrets reside in examples/config.json
  let cortex_config = match aux_load_config() {
    Ok(cfg) => cfg,
    Err(_) => {
      println!("You need a well-formed JSON examples/config.json file to run the frontend.");
      return Err(Redirect::to("/"))
      // TODO: Need to figure out how to do a Result return value that can be NotFound and Redirect
      // NotFound(format!("Bad config, contact server administrator."))
    }
  };

  let g_recaptcha_response = if data.len() > 21 {
    str::from_utf8(&data[21..]).unwrap_or(&UNKNOWN)
  } else {
    UNKNOWN
  };
  // Check if we hve the g_recaptcha_response in Redis, then reuse
  let redis_opt;
  let quota : usize = match redis::Client::open("redis://127.0.0.1/") {
    Err(_) => {return Err(Redirect::to("/"))}// TODO: Err(NotFound(format!("redis unreachable")))},
    Ok(redis_client) => match redis_client.get_connection() {
      Err(_) => {return Err(Redirect::to("/"))}//TODO: Err(NotFound(format!("redis unreachable")))},
      Ok(redis_connection) => {
        let quota = redis_connection.get(g_recaptcha_response).unwrap_or(0);
        redis_opt = Some(redis_connection);
        quota
      }
    }
  };

  let captcha_verified = if quota > 0 {
    if quota == 1 {
      match &redis_opt {
        &Some(ref redis_connection) => {
          // Remove if last
          redis_connection.del(g_recaptcha_response).unwrap_or(());
          // We have quota available, decrement it
          redis_connection.set(g_recaptcha_response, quota-1).unwrap_or(());
        },
        &None => {} // compatibility mode: redis has ran away?
      };
    }
    // And allow operation
    true
  } else {
    let check_val = aux_check_captcha(g_recaptcha_response, &cortex_config.captcha_secret);
    if check_val {
      match &redis_opt {
        &Some(ref redis_connection) => {
          // Add a reuse quota if things check out, 19 more downloads
          redis_connection.set(g_recaptcha_response, 19).unwrap_or(());
        },
        &None => {}
      };
    }
    check_val
  };

  // If you are not human, you have no business here.
  if !captcha_verified {
    return Err(Redirect::to(&format!("/entry/{:?}/{:?}?expire_quotas", service_name, entry_id)));
  }
  println!("-- serving verified human request for entry {:?} download", entry_id);

  // let service_name = aux_uri_unescape(request.param("service_name")).unwrap_or(UNKNOWN.to_string());
  // let entry_taskid = aux_uri_unescape(request.param("entry")).unwrap_or(UNKNOWN.to_string());
  let placeholder_task = Task {
    id: Some(entry_id as i64),
    entry: String::new(),
    corpusid : 0,
    serviceid : 0,

    status : 0
  };
  let backend = Backend::default();
  let task = backend.sync(&placeholder_task).unwrap_or(placeholder_task); // TODO: Error-reporting

  let entry = task.entry;
  let zip_path = match service_name.as_str() {
    "import" => entry,
    _ => {
      let strip_name_regex = Regex::new(r"/[^/]+$").unwrap();
      strip_name_regex.replace(&entry,"") + "/" + &service_name + ".zip"
    }
  };
  if zip_path.is_empty() {
    Err(Redirect::to("/"))  // TODO : Err(NotFound(format!("Service {:?} does not have a result for entry {:?}", service_name, entry_id)))
  } else {
    NamedFile::open(&zip_path).map_err(|_| Redirect::to(&format!("/")))
  }
}


//   //Rerun queries
//   let rerun_config1 = cortex_config.clone();
//   server.post("/rerun/:corpus_name/:service_name",
//               middleware! { |request, response|
//     return serve_rerun(&rerun_config1, request, response)
//   });
//   let rerun_config2 = cortex_config.clone();
//   server.post("/rerun/:corpus_name/:service_name/:severity",
//               middleware! { |request, response|
//     return serve_rerun(&rerun_config2, request, response)
//   });
//   let rerun_config3 = cortex_config.clone();
//   server.post("/rerun/:corpus_name/:service_name/:severity/:category",
//               middleware! { |request, response|
//     return serve_rerun(&rerun_config3, request, response)
//   });
//   let rerun_config4 = cortex_config.clone(); // TODO: There has to be a better way...
//   server.post("/rerun/:corpus_name/:service_name/:severity/:category/:what",
//               middleware! { |request, response|
//     return serve_rerun(&rerun_config4, request, response)
//   });

//   if let Err(e) = server.listen("127.0.0.1:6767") {
//     println!("Couldn't start server: {:?}", e);
//   }
//   return;
// }



#[get("/public/<file..>")]
fn files(file: PathBuf) -> Result<NamedFile, NotFound<String>> {
  let path = Path::new("public/").join(file);
  NamedFile::open(&path).map_err(|_| NotFound(format!("Bad path: {:?}", path)))
}

fn rocket() -> rocket::Rocket {
  rocket::ignite().mount("/", routes![root, admin, corpus, files, top_service_report, severity_service_report, category_service_report, what_service_report, entry_fetch]).attach(Template::fairing())
}

fn main() {
  let _ = thread::spawn(move || {
    cache_worker();
  });

  rocket().launch();
}



fn serve_report(corpus_name: String, service_name: String, severity: Option<String>, category: Option<String>, what: Option<String>) -> Result<Template, NotFound<String>> {
   let mut context = TemplateContext::default();
   let mut global = HashMap::new();
   let backend = Backend::default();

//   let corpus_name = aux_uri_unescape(request.param("corpus_name")).unwrap_or(UNKNOWN.to_string());
//   let service_name = aux_uri_unescape(request.param("service_name")).unwrap_or(UNKNOWN.to_string());
//   let severity = aux_uri_unescape(request.param("severity"));
//   let category = aux_uri_unescape(request.param("category"));
//   let what = aux_uri_unescape(request.param("what"));

  let corpus_result = Corpus {
                        id: None,
                        name: corpus_name.clone(),
                        path: String::new(),
                        complex: true,
                      }
                      .select_by_key(&backend.connection);
  if let Ok(Some(corpus)) = corpus_result {
    let service_result = Service {
                           id: None,
                           name: service_name.clone(),
                           complex: true,
                           version: 0.1,
                           inputconverter: None,
                           inputformat: String::new(),
                           outputformat: String::new(),
                         }
                         .select_by_key(&backend.connection);
    if let Ok(Some(service)) = service_result {
      // Metadata in all reports
      global.insert("title".to_string(),
                    "Corpus Report for ".to_string() + &corpus_name);
      global.insert("description".to_string(),
                    "An analysis framework for corpora of TeX/LaTeX documents - statistical reports for ".to_string() + &corpus_name);
      global.insert("corpus_name".to_string(), corpus_name.clone());
      global.insert("service_name".to_string(), service_name.clone());
      global.insert("type".to_string(), "Conversion".to_string());
      global.insert("inputformat".to_string(), service.inputformat.clone());
      global.insert("outputformat".to_string(), service.outputformat.clone());
      match service.inputconverter {
        Some(ref ic_service_name) => global.insert("inputconverter".to_string(), ic_service_name.clone()),
        None => global.insert("inputconverter".to_string(), "missing?".to_string()),
      };

      let report;
      let template;
      let report_start = time::get_time();
      if severity.is_none() {
        // Top-level report
        report = backend.progress_report(&corpus, &service);
        // Record the report into the globals
        for (key, val) in report {
          global.insert(key.clone(), val.to_string());
        }
        global.insert("report_time".to_string(), time::now().rfc822().to_string());
        template = "cortex-report";
      } else if category.is_none() {
        // Severity-level report
        global.insert("severity".to_string(), severity.clone().unwrap());
        global.insert("highlight".to_string(),
                      aux_severity_highlight(&severity.clone().unwrap()).to_string());
        template = if severity.is_some() && (severity.clone().unwrap() == "no_problem") {
          let entries = aux_task_report(&mut global, &corpus, &service, severity, None, None);
          // Record the report into "entries" vector
          context.entries = Some(entries);
          // And set the task list template
          "cortex-report-task-list"
        } else {
          let categories = aux_task_report(&mut global, &corpus, &service, severity, None, None);
          // Record the report into "categories" vector
          context.categories = Some(categories);
          // And set the severity template
          "cortex-report-severity"
        };
      } else if what.is_none() {
        // Category-level report
        global.insert("severity".to_string(), severity.clone().unwrap());
        global.insert("highlight".to_string(),
                      aux_severity_highlight(&severity.clone().unwrap()).to_string());
        global.insert("category".to_string(), category.clone().unwrap());
        if category.is_some() && (category.clone().unwrap() == "no_messages") {
          let entries = aux_task_report(&mut global, &corpus, &service, severity, category, None);
          // Record the report into "entries" vector
          context.entries = Some(entries);
          // And set the task list template
          template = "cortex-report-task-list";
        } else {
          let whats = aux_task_report(&mut global, &corpus, &service, severity, category, None);
          // Record the report into "whats" vector
          context.whats = Some(whats);
          // And set the category template
          template = "cortex-report-category";
        }
      } else {
        // What-level report
        global.insert("severity".to_string(), severity.clone().unwrap());
        global.insert("highlight".to_string(),
                      aux_severity_highlight(&severity.clone().unwrap()).to_string());
        global.insert("category".to_string(), category.clone().unwrap());
        global.insert("what".to_string(), what.clone().unwrap());
        let entries = aux_task_report(&mut global, &corpus, &service, severity, category, what);
        // Record the report into "entries" vector
        context.entries = Some(entries);
        // And set the task list template
        template = "cortex-report-task-list";
      }

      // Report also the query times
      let report_end = time::get_time();
      let report_duration = (report_end - report_start).num_milliseconds();
      global.insert("report_duration".to_string(), report_duration.to_string());
      // Pass the globals(reports+metadata) onto the stash
      context.global = global;
      // And pass the handy lambdas
      // And render the correct template
      aux_decorate_uri_encodings(&mut context);
      Ok(Template::render(template, context))
    } else {
      Err(NotFound(format!("Service {} does not exist.", &service_name)))
    }
  } else {
    Err(NotFound(format!("Corpus {} does not exist.", &corpus_name)))
  }
}

// fn serve_rerun<'a, D>(config: &CortexConfig, request: &mut Request<D>, response: Response<'a, D>) -> MiddlewareResult<'a, D> {
//   let corpus_name = aux_uri_unescape(request.param("corpus_name")).unwrap_or(UNKNOWN.to_string());
//   let service_name = aux_uri_unescape(request.param("service_name")).unwrap_or(UNKNOWN.to_string());
//   let severity = aux_uri_unescape(request.param("severity"));
//   let category = aux_uri_unescape(request.param("category"));
//   let what = aux_uri_unescape(request.param("what"));

//   // Ensure we're given a valid rerun token to rerun, or anyone can wipe the cortex results
//   let mut body_bytes = vec![];
//   request.origin.read_to_end(&mut body_bytes).unwrap_or(0);
//   let token = from_utf8(&body_bytes).unwrap_or(UNKNOWN);
//   let user_opt = config.rerun_tokens.get(token);
//   let user = match user_opt {
//     None => return response.error(Forbidden, "Access denied"),
//     Some(user) => user,
//   };
//   println!("-- User {:?}: Mark for rerun on {:?}/{:?}/{:?}/{:?}/{:?}",
//            user,
//            corpus_name,
//            service_name,
//            severity,
//            category,
//            what);

//   // Run (and measure) the three rerun queries
//   let report_start = time::get_time();
//   let backend = Backend::default();
//   // Build corpus and service objects
//   let placeholder_corpus = Corpus {
//     id: None,
//     name: corpus_name.to_string(),
//     path: String::new(),
//     complex: true,
//   };
//   let corpus = match placeholder_corpus.select_by_key(&backend.connection) {
//     Err(_) => return response.error(Forbidden, "Access denied"),
//     Ok(corpus_opt) => {
//       match corpus_opt {
//         None => return response.error(Forbidden, "Access denied"),
//         Some(corpus) => corpus,
//       }
//     }
//   };
//   let placeholder_service = Service {
//     id: None,
//     name: service_name.clone(),
//     complex: true,
//     version: 0.1,
//     inputconverter: None,
//     inputformat: String::new(),
//     outputformat: String::new(),
//   };
//   let service = match placeholder_service.select_by_key(&backend.connection) {
//     Err(_) => return response.error(Forbidden, "Access denied"),
//     Ok(service_opt) => {
//       match service_opt {
//         None => return response.error(Forbidden, "Access denied"),
//         Some(service) => service,
//       }
//     }
//   };
//   let rerun_result = backend.mark_rerun(&corpus, &service, severity, category, what);
//   let report_end = time::get_time();
//   let report_duration = (report_end - report_start).num_milliseconds();
//   println!("-- User {:?}: Mark for rerun took {:?}ms",
//            user,
//            report_duration);
//   match rerun_result {
//     Err(_) => response.error(Forbidden, "Access denied"), // TODO: better error message?
//     Ok(_) => response.send(""),
//   }
// }

fn aux_severity_highlight(severity: &str) -> &str {
  match severity {// Bootstrap highlight classes
    "no_problem" => "success",
    "warning" => "warning",
    "error" => "error",
    "fatal" => "danger",
    "invalid" => "info",
    _ => "unknown",
  }
}
fn aux_uri_unescape(param: Option<&str>) -> Option<String> {
  match param {
    None => None,
    Some(param_encoded) => {
      let mut param_decoded: String = param_encoded.to_owned();
      // TODO: This could/should be done faster by using lazy_static!
      for &(original, replacement) in &[("%3A", ":"),
                                      ("%2F", "/"),
                                      ("%24", "$"),
                                      ("%2E", "."),
                                      ("%21", "!"),
                                      ("%40", "@")]
      {
        param_decoded = param_decoded.replace(original, replacement);
      }
      Some(url::percent_encoding::percent_decode(param_decoded.as_bytes()).decode_utf8_lossy().into_owned())
    }
  }
}
fn aux_uri_escape(param: Option<String>) -> Option<String> {
  match param {
    None => None,
    Some(param_pure) => {
      let mut param_encoded: String = url::percent_encoding::utf8_percent_encode(&param_pure, url::percent_encoding::DEFAULT_ENCODE_SET).collect::<String>();
      // TODO: This could/should be done faster by using lazy_static!
      for &(original, replacement) in &[( ":", "%3A"),
                                      ("/", "%2F"),
                                      ("\\", "%5C"),
                                      ("$", "%24"),
                                      (".", "%2E"),
                                      ("!", "%21"),
                                      ("@", "%40")]
      {
        param_encoded = param_encoded.replace(original, replacement);
      }
      // if param_pure != param_encoded {
      //   println!("Encoded {:?} to {:?}", param_pure, param_encoded);
      // } else {
      //   println!("No encoding needed: {:?}", param_pure);
      // }
      Some(param_encoded)
    }
  }
}
fn aux_decorate_uri_encodings(context: &mut TemplateContext) {
  for inner_vec in &mut[&mut context.corpora, &mut context.services, &mut context.entries, &mut context.categories, &mut context.whats] {
    if let &mut &mut Some(ref mut inner_vec_data) = inner_vec {
      for mut subhash in inner_vec_data {
        let mut uri_decorations = vec![];
        for (subkey, subval) in subhash.iter() {
          uri_decorations.push((subkey.to_string() + "_uri",
                                aux_uri_escape(Some(subval.to_string())).unwrap()));
        }
        for (decoration_key, decoration_val) in uri_decorations {
          subhash.insert(decoration_key, decoration_val);
        }
      }
    }
  }
  // global is handled separately
  let mut uri_decorations = vec![];
  for (subkey, subval) in context.global.iter() {
    uri_decorations.push((subkey.to_string() + "_uri",
                          aux_uri_escape(Some(subval.to_string())).unwrap()));
  }
  for (decoration_key, decoration_val) in uri_decorations {
    context.global.insert(decoration_key, decoration_val);
  }
}

#[derive(RustcDecodable)]
struct IsSuccess {
  success: bool,
}

fn aux_check_captcha(g_recaptcha_response: &str, captcha_secret: &str) -> bool {
  let mut core = match Core::new() {
    Ok(c) => c,
    _ => return false
  };
  let handle = core.handle();
  let client = Client::configure()
    .connector(HttpsConnector::new(4, &handle).unwrap())
    .build(&handle);

  let mut verified = false;
  let url_with_query = "https://www.google.com/recaptcha/api/siteverify?secret=".to_string() + captcha_secret + "&response=" + g_recaptcha_response;
  let json_str = format!("{{\"secret\":\"{:?}\",\"response\":\"{:?}\"}}", captcha_secret, g_recaptcha_response);
  let req_url = match url_with_query.parse() {
    Ok(parsed) => parsed,
    _ => return false
  };
  let mut req = Request::new(Method::Post, req_url);
  req.headers_mut().set(ContentType::json());
  req.headers_mut().set(ContentLength(json_str.len() as u64));
  req.set_body(json_str);

  let post = client.request(req).and_then(|res| {
      res.body().concat2()
  });
  let posted = match core.run(post) {
    Ok(posted_data) => match str::from_utf8(&posted_data) {
      Ok(posted_str) => posted_str.to_string(),
      Err(e) => {println!("err: {}",e);return false}
    }
    Err(e) => {println!("err: {}",e);return false}
  };
  let json_decoded: Result<IsSuccess, _> = json::decode(&posted);
  if let Ok(response_json) = json_decoded {
    if response_json.success {
      verified = true;
    }
  }

  verified
}

fn aux_task_report(global: &mut HashMap<String, String>, corpus: &Corpus, service: &Service, severity: Option<String>, category: Option<String>, what: Option<String>) -> Vec<HashMap<String, String>> {
  let key_tail = match severity.clone() {
    Some(severity) => {
      let cat_tail = match category.clone() {
        Some(category) => {
          let what_tail = match what.clone() {
            Some(what) => "_".to_string() + &what,
            None => String::new(),
          };
          "_".to_string() + &category + &what_tail
        }
        None => String::new(),
      };
      "_".to_string() + &severity + &cat_tail
    }
    None => String::new(),
  };
  let cache_key: String = corpus.id.unwrap().to_string() + "_" + &service.id.unwrap().to_string() + &key_tail;
  let cache_key_time = cache_key.clone() + "_time";
  let redis_client = redis::Client::open("redis://127.0.0.1/").unwrap(); // TODO: Better error handling
  let redis_connection = redis_client.get_connection().unwrap();
  let cache_val: Result<String, _> = redis_connection.get(cache_key.clone());
  let fetched_report;
  let time_val: String;

  match cache_val {
    Ok(cached_report_json) => {
      let cached_report = json::decode(&cached_report_json).unwrap_or(Vec::new());
      if cached_report.is_empty() {
        let backend = Backend::default();
        let report: Vec<HashMap<String, String>> = backend.task_report(corpus, service, severity, category, what.clone());
        let report_json: String = json::encode(&report).unwrap();
        // println!("SET {:?}", cache_key);
        if what.is_none() {
          // don't cache the task list pages
          let _: () = redis_connection.set(cache_key, report_json).unwrap();
        }
        time_val = time::now().rfc822().to_string();
        let _: () = redis_connection.set(cache_key_time, time_val.clone()).unwrap();
        fetched_report = report;
      } else {
        // Get the report time, so that the user knows where the data is coming from
        time_val = redis_connection.get(cache_key_time).unwrap_or(time::now().rfc822().to_string());
        fetched_report = cached_report;
      }
    }
    Err(_) => {
      let backend = Backend::default();
      let report = backend.task_report(corpus, service, severity, category, what.clone());
      let report_json: String = json::encode(&report).unwrap();
      // println!("SET2 {:?}", cache_key);
      if what.is_none() {
        // don't cache the task lists pages
        let _: () = redis_connection.set(cache_key, report_json).unwrap();
      }
      time_val = time::now().rfc822().to_string();
      let _: () = redis_connection.set(cache_key_time, time_val.clone()).unwrap();
      fetched_report = report;
    }
  }
  // Setup the return
  global.insert("report_time".to_string(), time_val);
  fetched_report
}

fn cache_worker() {
  let redis_client = match redis::Client::open("redis://127.0.0.1/") {
    Ok(client) => client,
    _ => panic!("Redis connection failed, please boot up redis and restart the frontend!"),
  };
  let redis_connection = match redis_client.get_connection() {
    Ok(conn) => conn,
    _ => panic!("Redis connection failed, please boot up redis and restart the frontend!"),
  };
  let mut queued_cache: HashMap<String, usize> = HashMap::new();
  loop {
    // Keep a fresh backend connection on each invalidation pass.
    let backend = Backend::default();
    let mut global_stub: HashMap<String, String> = HashMap::new();
    // each corpus+service (non-import)
    for corpus in &backend.corpora(){
      let services_result = corpus.select_services(&backend.connection);
      match services_result {
        Err(_) => {}
        Ok(services) => {
          for service in &services {
            if service.name == "import" {
              continue;
            }
            println!("[cache worker] Examining corpus {:?}, service {:?}",
                     corpus.name,
                     service.name);
            // Pages we'll cache:
            let report = backend.progress_report(corpus, service);
            let zero: f64 = 0.0;
            let huge: usize = 999999;
            let queued_count_f64: f64 = report.get("queued").unwrap_or(&zero) + report.get("todo").unwrap_or(&zero);
            let queued_count: usize = queued_count_f64 as usize;
            let key_base: String = corpus.id.unwrap_or(0).to_string() + "_" + &service.id.unwrap_or(0).to_string();
            // Only recompute the inner pages if we are seeing a change / first visit, on the top corpus+service level
            if *queued_cache.get(&key_base).unwrap_or(&huge) != queued_count {
              println!("[cache worker] state changed, invalidating ...");
              // first cache the count for the next check:
              queued_cache.insert(key_base.clone(), queued_count);
              // each reported severity (fatal, warning, error)
              for severity in &["invalid", "fatal", "error", "warning", "no_problem"] {
                // most importantly, DEL the key from Redis!
                let key_severity = key_base.clone() + "_" + severity;
                println!("[cache worker] DEL {:?}", key_severity);
                redis_connection.del(key_severity.clone()).unwrap_or(());
                if *report.get(*severity).unwrap_or(&zero) > 0.0 {
                  // cache category page
                  thread::sleep(Duration::new(1, 0)); // Courtesy sleep of 1 second.
                  let category_report = aux_task_report(&mut global_stub,
                                                        corpus,
                                                        service,
                                                        Some(severity.to_string()),
                                                        None,
                                                        None);
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
                    let _ = aux_task_report(&mut global_stub,
                                            corpus,
                                            service,
                                            Some(severity.to_string()),
                                            Some(category.to_string()),
                                            None);
                    // for each what, cache the "task list" page
                    // for what_hash in what_report.iter() {
                    //   let what = what_hash.get("name").unwrap_or(&string_empty);
                    //   if what.is_empty() || (what == "total") {continue;}
                    //   let key_what = key_category.clone() + "_" + what;
                    //   println!("[cache worker] DEL {:?}", key_what);
                    //   redis_connection.del(key_what).unwrap_or(());
                    //   let _entries = aux_task_report(&mut global_stub, &corpus, &service, Some(severity.to_string()), Some(category.to_string()), Some(what.to_string()));
                    // }
                  }
                }
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
