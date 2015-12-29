// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate url;
extern crate hyper;
#[macro_use] extern crate nickel;
extern crate cortex;
extern crate rustc_serialize;
extern crate time;
extern crate regex;
extern crate redis;

use std::collections::HashMap;
// use std::path::Path;
// use std::fs;
use std::str::*;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::thread;
use std::time::Duration;
use regex::Regex;
use nickel::{Nickel, Mountable, StaticFilesHandler, HttpRouter, Request, Response, MiddlewareResult};
use hyper::header::Location;
use nickel::extensions::{Referer, Redirect};
use hyper::Client;
use nickel::status::StatusCode::{self, Forbidden};
// use nickel::QueryString;
// use nickel::status::StatusCode;
// use hyper::header::Location;
use redis::Commands;

use rustc_serialize::json;
use cortex::sysinfo;
use cortex::backend::{Backend};
use cortex::data::{Corpus, CortexORM, Service, Task};

static UNKNOWN: &'static str = "_unknown_";

#[derive(RustcDecodable, RustcEncodable, Debug, Clone)]
struct CortexConfig {
  captcha_secret : String,
  rerun_tokens : HashMap<String, String>
}

// fn slurp_file (path : &'static str) -> Result<String, Error> {
//   let mut f = try!(File::open(path));
//   let mut content = String::new();
//   try!(f.read_to_string(&mut content));
//   Ok(content)
// }

fn aux_load_config() -> Result<CortexConfig, String> {
  let mut config_file = try!(File::open("examples/config.json").map_err(|e| e.to_string()));
  let mut config_buffer = String::new();
  try!(config_file.read_to_string(&mut config_buffer).map_err(|e| e.to_string()));
  let cortex_config : Result<CortexConfig,String> = json::decode(&config_buffer).map_err(|e| e.to_string());
  return cortex_config
}

fn main() {
  let _ = thread::spawn(move || { cache_worker(); });

  // Any secrets reside in examples/config.json
  let cortex_config = match aux_load_config() {
    Ok(cfg) => cfg,
    Err(_) => {
      println!("You need a well-formed JSON examples/config.json file to run the frontend.");
      return;
    }
  };
  let mut server = Nickel::new();
  /*
   * Fall-through behaviour, if StaticFilesHandler does not find a matching file,
   * the request uri must be reset so that it can be matched against other middleware.
   */
  server.mount("/public/", StaticFilesHandler::new("public/"));
  //middleware function logs each request to console
  server.utilize(middleware! { |request|
      println!("{:?} {:?}", request.origin.method, request.origin.uri);
      println!("    from {:?}", request.origin.remote_addr);
  });

  server.get("/robots.txt", middleware! { |_, mut response|
    response.set(Location("/public/robots.txt".into()));
    response.set(StatusCode::PermanentRedirect);
    return response.send("")
  });

  server.get("/", middleware! { |_, response|
    let mut data = HashMap::new();
    let mut global = HashMap::new();
    global.insert("title".to_string(), "Framework Overview".to_string());
    global.insert("description".to_string(), "An analysis framework for corpora of TeX/LaTeX documents - overview.".to_string());

    let backend = Backend::default();
    let corpora = backend.corpora().iter().map(|c| c.to_hash()).collect::<Vec<_>>();

    data.insert("global".to_string(),vec![global]);
    data.insert("corpora".to_string(),corpora);
    aux_decorate_uri_encodings(&mut data);

    return response.render("examples/assets/cortex-overview.html", &data)
  });

  // Admin interface
  server.get("/admin", middleware! { |_, response|
    let mut data = HashMap::new();
    let mut global = HashMap::new();
    global.insert("title".to_string(), "Admin Interface".to_string());
    global.insert("description".to_string(), "An analysis framework for corpora of TeX/LaTeX documents - admin interface.".to_string());
    match sysinfo::report(&mut global) {
      Ok(_) => {},
      Err(e) => println!("Sys report failed: {:?}", e)
    };
    data.insert("global".to_string(),vec![global]);
    aux_decorate_uri_encodings(&mut data);
    return response.render("examples/assets/cortex-admin.html", &data)
  });

  // server.get("/add_corpus", middleware! { |request, mut response| 
  //   let backend = Backend::default();
  //   let mut data = HashMap::new();
  //   let mut message : String ;
  //   let mut corpus_path;
  //   let query = request.query();
  //   if let Some(p) = query.get("path") {
  //     corpus_path = p.to_string();
  //   } else {
  //     data.insert("message", "Error: Please provide a path!".to_string());
  //     return response.render("examples/assets/cortex-admin.html", &data);
  //   }
  //   println!("Adding Path: {:?}", corpus_path);
  //   let complex : bool = query.get("setup") != Some("canonical");
  //   let path = Path::new(&corpus_path);
  //   match fs::metadata(path) {
  //     Ok(_) => {},
  //     Err(_) => {
  //       message = "Error: Path ".to_string() + &corpus_path + " does not exist, aborting!";
        
  //       response.set(Location("/admin".into()));
  //       response.set(StatusCode::TemporaryRedirect);
  //       return response.send("")
  //     }
  //   };
  //   let corpus_name = path.file_stem().unwrap().to_str().unwrap().to_string();

  //   // Queue the corpus for import using the task database:
  //   let input_corpus = Corpus {
  //     id : None,
  //     name : corpus_name,
  //     path : corpus_path.clone(),
  //     complex : complex,
  //   };
  //   message = match backend.add(input_corpus) {
  //     Ok(_) => "Successfully Queued ".to_string() + &corpus_path+ " for Import.",
  //     Err(_) => "Failed to add corpus, please retry!".to_string()
  //   };
  //   data.insert("message", message);
  //   return response.render("examples/assets/cortex-admin.html", &data);
  // });

  server.get("/corpus/:corpus_name", middleware! { |request, mut response|
    let mut data = HashMap::new();
    let mut global = HashMap::new();
    let backend = Backend::default();
    let corpus_name = aux_uri_unescape(request.param("corpus_name")).unwrap_or(UNKNOWN.to_string());
    let corpus_result = Corpus{id: None, name: corpus_name.to_string(), path : String::new(), complex : true}.select_by_key(&backend.connection);
    match corpus_result {
      Ok(corpus_select) => {
        match corpus_select {
          Some(corpus) => {
            global.insert("title".to_string(), "Registered services for ".to_string() + &corpus_name);
            global.insert("description".to_string(), "An analysis framework for corpora of TeX/LaTeX documents - registered services for ".to_string()+ &corpus_name);
            global.insert("corpus_name".to_string(), corpus_name.to_string());
            data.insert("global".to_string(),vec![global]);

            let services_result = corpus.select_services(&backend.connection);
            match services_result {
              Ok(backend_services) => {
                let services = backend_services.iter()
                              .map(|s| s.to_hash()).collect::<Vec<_>>();
                let mut service_reports = Vec::new();
                for mut service in services.into_iter() {
                  service.insert("status".to_string(),"Running".to_string());
                  service_reports.push(service);
                }
                data.insert("services".to_string(), service_reports);
              },
              _ => {}
            };
            aux_decorate_uri_encodings(&mut data);
            return response.render("examples/assets/cortex-services.html", &data);
          },
          None => {}
        }
      },
      _ => {}
    }
    // let message = "Error: Corpus ".to_string() + &corpus_name + " does not exist, aborting!";
    response.set(Location("/".into()));
    response.set(StatusCode::TemporaryRedirect);
    return response.send("")
  });

  server.get("/corpus/:corpus_name/:service_name", middleware! { |request, response|
    return serve_report(request, response)
  });
  server.get("/corpus/:corpus_name/:service_name/:severity", middleware! { |request, response|
    return serve_report(request, response)
  });
  server.get("/corpus/:corpus_name/:service_name/:severity/:category", middleware! { |request, response|
    return serve_report(request, response)
  });
  server.get("/corpus/:corpus_name/:service_name/:severity/:category/:what", middleware! { |request, response|
    return serve_report(request, response)
  });

  let cortex_config2 = cortex_config.clone();
  server.post("/entry/:service_name/:entry", middleware! { |request, response|
    let mut body_bytes = vec![];
    request.origin.read_to_end(&mut body_bytes).unwrap_or(0);
    let g_recaptcha_response = from_utf8(&body_bytes[21..]).unwrap_or(&UNKNOWN);
    // Check if we hve the g_recaptcha_response in Redis, then reuse
    let mut redis_opt = None;
    let quota : usize = match redis::Client::open("redis://127.0.0.1/") {
      Err(_) => 0,
      Ok(redis_client) => match redis_client.get_connection() {
        Err(_) => 0,
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
            let _ : () = redis_connection.del(g_recaptcha_response).unwrap_or(());
            // We have quota available, decrement it
            let _ : () = redis_connection.set(g_recaptcha_response, quota-1).unwrap_or(());
          },
          &None => {} // compatibility mode: redis has ran away?
        };
      }
      // And allow operation
      true
    } else {
      let check_val = aux_check_captcha(g_recaptcha_response, &cortex_config2.captcha_secret);
      if check_val {
        match &redis_opt {
          &Some(ref redis_connection) => {
            // Add a reuse quota if things check out, 19 more downloads
            let _ : () = redis_connection.set(g_recaptcha_response, 19).unwrap_or(());
          },
          &None => {} 
        };
      }
      check_val
    };

    // If you are not human, you have no business here.
    if !captcha_verified {
      let back = request.referer().unwrap_or("/");
      return response.redirect(back.to_string()+"?expire_quotas")
    }
    println!("-- serving verified human request for entry download");

    let service_name = aux_uri_unescape(request.param("service_name")).unwrap_or(UNKNOWN.to_string());
    let entry_taskid = aux_uri_unescape(request.param("entry")).unwrap_or(UNKNOWN.to_string());
    let placeholder_task = Task {
      id: Some(entry_taskid.parse::<i64>().unwrap_or(0)),
      entry: String::new(),
      corpusid : 0,
      serviceid : 0,
      status : 0
    };
    let backend = Backend::default();
    let task = backend.sync(&placeholder_task).unwrap_or(placeholder_task); // TODO: Error-reporting
    let entry = task.entry;
    let zip_path = if service_name == "import" {
      entry }
    else {
      let strip_name_regex = Regex::new(r"/[^/]+$").unwrap();
      strip_name_regex.replace(&entry,"") + "/" + &service_name + ".zip"
    };
    return response.send_file(Path::new(&zip_path))
  });

  // Rerun queries
  let rerun_config1 = cortex_config.clone();
  server.post("/rerun/:corpus_name/:service_name", middleware! { |request, response|
    return serve_rerun(&rerun_config1, request, response)
  });
  let rerun_config2 = cortex_config.clone();
  server.post("/rerun/:corpus_name/:service_name/:severity", middleware! { |request, response|
    return serve_rerun(&rerun_config2, request, response)
  });
  let rerun_config3 = cortex_config.clone();
  server.post("/rerun/:corpus_name/:service_name/:severity/:category", middleware! { |request, response|
    return serve_rerun(&rerun_config3, request, response)
  });
  let rerun_config4 = cortex_config.clone(); // TODO: There has to be a better way...
  server.post("/rerun/:corpus_name/:service_name/:severity/:category/:what", middleware! { |request, response|
    return serve_rerun(&rerun_config4, request, response)
  });

  server.listen("127.0.0.1:6767");
}

fn serve_report<'a, D>(request: &mut Request<D>, response: Response<'a, D>) -> MiddlewareResult<'a, D>  {
  let mut data = HashMap::new();
  let mut global = HashMap::new();
  let backend = Backend::default();

  let corpus_name = aux_uri_unescape(request.param("corpus_name")).unwrap_or(UNKNOWN.to_string());
  let service_name = aux_uri_unescape(request.param("service_name")).unwrap_or(UNKNOWN.to_string());
  let severity = aux_uri_unescape(request.param("severity"));
  let category = aux_uri_unescape(request.param("category"));
  let what = aux_uri_unescape(request.param("what"));

  let corpus_result = Corpus{id: None, name: corpus_name.clone(), path : String::new(), complex : true}.select_by_key(&backend.connection);
  match corpus_result { Ok(corpus_select) => {
  match corpus_select {Some(corpus) => {
    let service_result = Service{id: None, name: service_name.clone(),  complex: true, version: 0.1, inputconverter: None, inputformat: String::new(), outputformat:String::new()}.select_by_key(&backend.connection);
    match service_result { Ok(service_select) => {
    match service_select {Some(service) => {
      // Metadata in all reports
      global.insert("title".to_string(), "Corpus Report for ".to_string() + &corpus_name);
      global.insert("description".to_string(), "An analysis framework for corpora of TeX/LaTeX documents - statistical reports for ".to_string()+ &corpus_name);
      global.insert("corpus_name".to_string(), corpus_name.clone());
      global.insert("service_name".to_string(), service_name.clone());
      global.insert("type".to_string(),"Conversion".to_string());
      global.insert("inputformat".to_string(),service.inputformat.clone());
      global.insert("outputformat".to_string(),service.outputformat.clone());
      match &service.inputconverter {
        &Some(ref ic_service_name) => global.insert("inputconverter".to_string(), ic_service_name.clone()),
        &None => global.insert("inputconverter".to_string(), "missing?".to_string()),
      };
      
      let report;
      let template;
      let report_start = time::get_time();
      if severity.is_none() { // Top-level report
        report = backend.progress_report(&corpus, &service);
        // Record the report into the globals
        for (key, val) in report.iter() {
          global.insert(key.clone(), val.to_string());
        }
        template = "examples/assets/cortex-report.html";
      }
      else if category.is_none() { // Severity-level report
        global.insert("severity".to_string(),severity.clone().unwrap());
        global.insert("highlight".to_string(), aux_severity_highlight(&severity.clone().unwrap()).to_string());
        template = if severity.is_some() && (severity.clone().unwrap() == "no_problem") {
          let entries = aux_task_report(&mut global, &corpus, &service, severity, None, None);
          // Record the report into "entries" vector
          data.insert("entries".to_string(),entries);
          // And set the task list template
          "examples/assets/cortex-report-task-list.html"
        } else {
          let categories = aux_task_report(&mut global, &corpus, &service, severity, None, None);
          // Record the report into "categories" vector
          data.insert("categories".to_string(),categories);
          // And set the severity template
          "examples/assets/cortex-report-severity.html"
        };
      }
      else if what.is_none() { // Category-level report
        global.insert("severity".to_string(),severity.clone().unwrap());
        global.insert("highlight".to_string(), aux_severity_highlight(&severity.clone().unwrap()).to_string());
        global.insert("category".to_string(),category.clone().unwrap());
        let whats = aux_task_report(&mut global, &corpus, &service, severity, category, None);
        // Record the report into "whats" vector
        data.insert("whats".to_string(), whats);
        // And set the category template
        template = "examples/assets/cortex-report-category.html";
      }
      else { // What-level report
        global.insert("severity".to_string(),severity.clone().unwrap());
        global.insert("highlight".to_string(), aux_severity_highlight(&severity.clone().unwrap()).to_string());
        global.insert("category".to_string(),category.clone().unwrap());
        global.insert("what".to_string(),what.clone().unwrap());
        let entries = aux_task_report(&mut global, &corpus, &service, severity, category, what);
        // Record the report into "entries" vector
        data.insert("entries".to_string(),entries);
        // And set the task list template
        template = "examples/assets/cortex-report-task-list.html";
      }

      // Report also the query times
      let report_end = time::get_time();
      let report_duration = (report_end - report_start).num_milliseconds();
      global.insert("report_duration".to_string(),report_duration.to_string());
      // Pass the globals(reports+metadata) onto the stash
      data.insert("global".to_string(),vec![global]);
      // And pass the handy lambdas
      // And render the correct template
      aux_decorate_uri_encodings(&mut data);
      return response.render(template, &data)
    },
    _=>{}}},
    _=>{}}},
    _=>{}}},
    _=>{}};

  // let message = "Error: Corpus ".to_string() + &corpus_name + " does not exist, aborting!";
  return response.send("")
}

fn serve_rerun<'a, D>(config : &CortexConfig, request: &mut Request<D>, response: Response<'a, D>) -> MiddlewareResult<'a, D>  {
  let corpus_name = aux_uri_unescape(request.param("corpus_name")).unwrap_or(UNKNOWN.to_string());
  let service_name = aux_uri_unescape(request.param("service_name")).unwrap_or(UNKNOWN.to_string());
  let severity = aux_uri_unescape(request.param("severity"));
  let category = aux_uri_unescape(request.param("category"));
  let what = aux_uri_unescape(request.param("what"));

  // Ensure we're given a valid rerun token to rerun, or anyone can wipe the cortex results
  let mut body_bytes = vec![];
  request.origin.read_to_end(&mut body_bytes).unwrap_or(0);
  let token = from_utf8(&body_bytes).unwrap_or(&UNKNOWN);
  let user_opt = config.rerun_tokens.get(token);
  let user = match user_opt {
    None => return response.error(Forbidden, "Access denied"),
    Some(user) => user
  };
  println!("-- User {:?}: Mark for rerun on {:?}/{:?}/{:?}/{:?}/{:?}", user, corpus_name, service_name, severity, category, what);
  
  // Run (and measure) the three rerun queries
  let report_start = time::get_time();
  let backend = Backend::default();
  // Build corpus and service objects
  let placeholder_corpus = Corpus{id: None, name: corpus_name.to_string(), path : String::new(), complex : true};
  let corpus = match placeholder_corpus.select_by_key(&backend.connection) {
    Err(_) => return response.error(Forbidden, "Access denied"),
    Ok(corpus_opt) => match corpus_opt {
      None => return response.error(Forbidden, "Access denied"),
      Some(corpus) => corpus
    }
  };
  let placeholder_service = Service{id: None, name: service_name.clone(),  complex: true, version: 0.1, inputconverter: None, inputformat: String::new(), outputformat:String::new()};
  let service = match placeholder_service.select_by_key(&backend.connection) {
    Err(_) => return response.error(Forbidden, "Access denied"),
    Ok(service_opt) => match service_opt {
      None => return response.error(Forbidden, "Access denied"),
      Some(service) => service
    }
  };
  let rerun_result = backend.mark_rerun(&corpus, &service, severity, category, what);
  let report_end = time::get_time();
  let report_duration = (report_end - report_start).num_milliseconds();
  println!("-- User {:?}: Mark for rerun took {:?}ms", user, report_duration);
  return match rerun_result {
    Err(_) => response.error(Forbidden, "Access denied"), // TODO: better error message?
    Ok(_) => response.send("")
  }
}

fn aux_severity_highlight<'highlight>(severity : &'highlight str) -> &'highlight str {
   match severity {// Bootstrap highlight classes
    "no_problem" => "success",
    "warning" => "warning",
    "error" => "error",
    "fatal" => "danger",
    _ => "unknown"
  }
}
fn aux_uri_unescape<'unescape>(param : Option<&'unescape str>) -> Option<String> {
  match param {
    None => None,
    Some(param_encoded) => {
      let colon_regex = Regex::new(r"%3A").unwrap();
      let slash_regex = Regex::new(r"%2F").unwrap();
      let decoded_colon = colon_regex.replace_all(&param_encoded,":");
      let decoded_slash = slash_regex.replace_all(&decoded_colon,"/");
      Some(url::percent_encoding::lossy_utf8_percent_decode(decoded_slash.as_bytes()))
    }
  }
}
fn aux_uri_escape(param : Option<String>) -> Option<String> {
  match param {
    None => None,
    Some(param_pure) => {
      let colon_regex = Regex::new(r"[:]").unwrap();
      let slash_regex = Regex::new(r"[/]").unwrap();
      let lib_encoded = url::percent_encoding::utf8_percent_encode(&param_pure, url::percent_encoding::DEFAULT_ENCODE_SET);
      let encoded_colon = colon_regex.replace_all(&lib_encoded,"%3A");
      let encoded_slash = slash_regex.replace_all(&encoded_colon,"%2F");

      let encoded_final = encoded_slash;
      Some(encoded_final)
    }
  }
}
fn aux_decorate_uri_encodings(data : &mut HashMap<String, Vec<HashMap<String,String>>>) {
  for (_, inner_vec) in data.into_iter() {
    for mut subhash in inner_vec.into_iter() {
      let mut uri_decorations = vec![];
      for (subkey, subval) in subhash.iter() {
        uri_decorations.push((subkey.to_string() + "_uri", aux_uri_escape(Some(subval.to_string())).unwrap()));
      }
      for decoration in uri_decorations.into_iter() {
        match decoration {
          (decoration_key, decoration_val) => subhash.insert(decoration_key, decoration_val)
        };
      }
    }
  }
}

#[derive(RustcDecodable)]
struct IsSuccess {
  success: bool
}

fn aux_check_captcha(g_recaptcha_response : &str, captcha_secret: &str) -> bool {
  let client = Client::new();
  let mut verified = false;
  let url_with_query = "https://www.google.com/recaptcha/api/siteverify?secret=".to_string() + captcha_secret + "&response=" + g_recaptcha_response;
  let mut res = client.post(&url_with_query)
    .send()
    .unwrap();

  let mut buffer = String::new();
  match res.read_to_string(&mut buffer) {
    Ok(_) => {
      let json_decoded : Result<IsSuccess,_> = json::decode(&mut buffer);
      match json_decoded { Ok(response_json) => {
        if response_json.success {
          verified = true;
        }
      },
      _ => {}};
    },
    _ => {}
  };
  return verified
}

fn aux_task_report(global: &mut HashMap<String, String>, corpus: &Corpus, service: &Service, severity : Option<String>, category: Option<String>, what :Option<String>) -> Vec<HashMap<String, String>>{
  let key_tail = match severity.clone() {
    Some(severity) => {
      let cat_tail = match category.clone() {
        Some(category) => {
          let what_tail = match what.clone() {
            Some(what) => "_".to_string() + &what,
            None => String::new()
          };
          "_".to_string() + &category + &what_tail
        },
        None => String::new()
      };
      "_".to_string() + &severity + &cat_tail
    },
    None => String::new()
  };
  let cache_key : String = corpus.id.unwrap().to_string() + "_" + &service.id.unwrap().to_string() + &key_tail;
  let cache_key_time = cache_key.clone() + "_time";
  let redis_client = redis::Client::open("redis://127.0.0.1/").unwrap(); // TODO: Better error handling
  let redis_connection = redis_client.get_connection().unwrap();
  let cache_val : Result<String,_> = redis_connection.get(cache_key.clone());
  let fetched_report;
  let time_val : String;

  match cache_val {
    Ok(cached_report_json) => {
      let cached_report = json::decode(&cached_report_json).unwrap_or(Vec::new());
      if cached_report.is_empty() {
        let backend = Backend::default();
        let report : Vec<HashMap<String, String>> = backend.task_report(corpus, service, severity, category, what);
        let report_json : String = json::encode(&report).unwrap();
        // println!("SET {:?}", cache_key);
        let _ : () = redis_connection.set(cache_key, report_json).unwrap();
        time_val = time::now().rfc822().to_string();
        let _ : () = redis_connection.set(cache_key_time, time_val.clone()).unwrap();
        fetched_report = report;
      } else {
        // Get the report time, so that the user knows where the data is coming from
        time_val = redis_connection.get(cache_key_time).unwrap_or(time::now().rfc822().to_string());
        fetched_report = cached_report;
      }
    },
    Err(_) => {
      let backend = Backend::default();
      let report = backend.task_report(corpus, service, severity, category, what);
      let report_json : String = json::encode(&report).unwrap();
      // println!("SET2 {:?}", cache_key);
      let _ : () = redis_connection.set(cache_key, report_json).unwrap();
      time_val = time::now().rfc822().to_string();
      let _ : () = redis_connection.set(cache_key_time, time_val.clone()).unwrap();
      fetched_report = report;
    }
  }
  // Setup the return
  global.insert("report_time".to_string(), time_val);
  return fetched_report;
}

fn cache_worker() {
  let redis_client = match redis::Client::open("redis://127.0.0.1/") {
    Ok(client) => client,
    _ => panic!("Redis connection failed, please boot up redis and restart the frontend!")
  };
  let redis_connection = match redis_client.get_connection() { 
    Ok(conn) => conn,
    _ => panic!("Redis connection failed, please boot up redis and restart the frontend!")
  };
  let backend = Backend::default();
  let mut global_stub : HashMap<String,String> = HashMap::new();
  let mut queued_cache : HashMap<String, usize> = HashMap::new();
  loop {
    // each corpus+service (non-import)
    for corpus in backend.corpora().iter() {
      let services_result = corpus.select_services(&backend.connection);
      match services_result {
        Err(_) => {},
        Ok(services) => {
          for service in services.iter() {
            if service.name == "import" {continue;}
            // Pages we'll cache:
            let report = backend.progress_report(corpus, service);
            let zero : f64 = 0.0;
            let huge : usize = 999999;
            let queued_count_f64 : f64 = report.get("queued").unwrap_or(&zero) + report.get("todo").unwrap_or(&zero);
            let queued_count : usize = queued_count_f64 as usize; 
            let key_base : String = corpus.id.unwrap_or(0).to_string() + "_" + &service.id.unwrap_or(0).to_string();
            // Only recompute the inner pages if we are seeing a change / first visit, on the top corpus+service level
            if *queued_cache.get(&key_base).unwrap_or(&huge) != queued_count {
              // first cache the count for the next check:
              queued_cache.insert(key_base.clone(), queued_count);
              // each reported severity (fatal, warning, error)
              for severity in vec!["fatal".to_string(), "error".to_string(), "warning".to_string()].iter() {
                // most importantly, DEL the key from Redis!
                let key_severity = key_base.clone() + "_" + severity;
                //println!("DEL {:?}", key_severity);
                let _ : () = redis_connection.del(key_severity.clone()).unwrap_or(());
                if *report.get(severity).unwrap_or(&zero) > 0.0 {
                  // cache category page
                  thread::sleep(Duration::new(1,0)); // Courtesy sleep of 1 second.
                  let category_report = aux_task_report(&mut global_stub, &corpus, &service, Some(severity.to_string()), None, None);
                  // for each category, cache the what page
                  for cat_hash in category_report.iter() {
                    let string_unknown = UNKNOWN.to_string();
                    let category = cat_hash.get("name").unwrap_or(&string_unknown);
                    if category == "total" {continue;}
                    let key_category = key_severity.clone() + "_" + category;
                    // println!("  DEL {:?}", key_category);
                    let _ : () = redis_connection.del(key_category).unwrap_or(());
                    thread::sleep(Duration::new(1,0)); // Courtesy sleep of 1 second.
                    let _what_report = aux_task_report(&mut global_stub, &corpus, &service, Some(severity.to_string()), Some(category.to_string()), None);
                  }
                }
              }
            }
          }
        }
      }
    }
    // Take two minutes before we recheck.
    thread::sleep(Duration::new(120,0));
  }
}