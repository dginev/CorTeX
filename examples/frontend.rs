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

use std::collections::HashMap;
// use std::path::Path;
// use std::fs;
use std::str::*;
use std::fs::File;
use std::io::Read;
use std::io::Error;
use std::path::Path;
use nickel::{Nickel, Mountable, StaticFilesHandler, HttpRouter, Request, Response, MiddlewareResult};
use hyper::header::Location;
use hyper::Client;
use nickel::status::StatusCode;
// use nickel::QueryString;
// use nickel::status::StatusCode;
// use hyper::header::Location;


use rustc_serialize::json;
use cortex::sysinfo;
use cortex::backend::{Backend};
use cortex::data::{Corpus, CortexORM, Service};


#[derive(RustcDecodable, RustcEncodable, Debug, Clone)]
struct CortexConfig {
  captcha_secret : String
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
      println!("logging request: {:?}", request.origin.uri);
  });

  server.get("/robots.txt", middleware! { |_, mut response|
    response.set(Location("/public/robots.txt".into()));
    response.set(StatusCode::PermanentRedirect);
    return response.send("")
  });
  // Root greeter
  server.get("/", middleware! { |_, response|
    let mut data = HashMap::new();
    let mut global = HashMap::new();
    global.insert("title", "Framework Overview".to_string());
    global.insert("description", "An analysis framework for corpora of TeX/LaTeX documents - overview.".to_string());
    
    let backend = Backend::default();
    let corpora = backend.corpora().iter().map(|c| c.to_hash()).collect::<Vec<_>>();

    data.insert("global",vec![global]);
    data.insert("corpora",corpora);
    return response.render("examples/assets/cortex-overview.html", &data);
  });

  // Admin interface
  server.get("/admin", middleware! { |_, response|
    let mut data = HashMap::new();
    let mut global = HashMap::new();
    global.insert("title", "Admin Interface".to_string());
    global.insert("description", "An analysis framework for corpora of TeX/LaTeX documents - admin interface.".to_string());
    match sysinfo::report(&mut global) {
      Ok(_) => {},
      Err(e) => println!("Sys report failed: {:?}", e)
    };
    data.insert("global",vec![global]);
    return response.render("examples/assets/cortex-admin.html", &data);
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
    let corpus_name = aux_uri_unescape(request.param("corpus_name")).unwrap();
    let corpus_result = Corpus{id: None, name: corpus_name.to_string(), path : String::new(), complex : true}.select_by_key(&backend.connection);
    match corpus_result {
      Ok(corpus_select) => {
        match corpus_select {
          Some(corpus) => {
            global.insert("title", "Registered services for ".to_string() + &corpus_name);
            global.insert("description", "An analysis framework for corpora of TeX/LaTeX documents - registered services for ".to_string()+ &corpus_name);
            global.insert("corpus_name", corpus_name.to_string());
            data.insert("global",vec![global]);

            let services_result = corpus.select_services(&backend.connection);
            match services_result {
              Ok(backend_services) => {
                let services = backend_services.iter()
                              .map(|s| s.to_hash()).collect::<Vec<_>>();
                let mut service_reports = Vec::new();
                for mut service in services.into_iter() {
                  service.insert("status","Running".to_string());
                  service_reports.push(service);
                }
                data.insert("services", service_reports);
              },
              _ => {}
            };
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
  server.post("/entry/:corpus_name/:service_name/:entry", middleware! { |request, mut response|
    let mut body_bytes = vec![];
    request.origin.read_to_end(&mut body_bytes).unwrap();
    let g_recaptcha_response = from_utf8(&body_bytes[21..]).unwrap();
    let captcha_verified = aux_check_captcha(g_recaptcha_response, &cortex_config2.captcha_secret);

    // If you are not human, you have no business here.
    if !captcha_verified {
      response.set(Location("/".into()));
      response.set(StatusCode::TemporaryRedirect);
      return response.send("");
    }
    println!("-- serving verified human request for entry download");

    let corpus_name = aux_uri_unescape(request.param("corpus_name")).unwrap();
    let service_name = aux_uri_unescape(request.param("service_name")).unwrap();
    let entry = aux_uri_unescape(request.param("entry")).unwrap();

    let backend = Backend::default();
    let corpus_result = Corpus{id: None, name: corpus_name.to_string(), path : String::new(), complex : true}.select_by_key(&backend.connection);
    match corpus_result {
      Ok(corpus_select) => {
        match corpus_select {
          Some(corpus) => {
            let zip_path = if service_name == "import" {
              corpus.path + "/" + &entry + "/" + &entry + ".zip" }
            else {
              corpus.path + "/" + &entry + "/" + &service_name + ".zip"
            };
            return response.send_file(Path::new(&zip_path));
          },
          _ => {}
        }
      },
      _ => {}
    };
  });

  server.listen("127.0.0.1:6767");
}

fn serve_report<'a, D>(request: &mut Request<D>, response: Response<'a, D>) -> MiddlewareResult<'a, D>  {
  let mut data = HashMap::new();
  let mut global = HashMap::new();
  let backend = Backend::default();
  let corpus_name = aux_uri_unescape(request.param("corpus_name")).unwrap();
  let service_name = aux_uri_unescape(request.param("service_name")).unwrap();
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
          let entries = backend.task_report(&corpus, &service, severity, None, None);
          // Record the report into "entries" vector
          data.insert("entries",entries);
          // And set the task list template
          "examples/assets/cortex-report-task-list.html"
        } else {
          let categories = backend.task_report(&corpus, &service, severity, None, None);
          // Record the report into "categories" vector
          data.insert("categories",categories);
          // And set the severity template
          "examples/assets/cortex-report-severity.html"
        };
      }
      else if what.is_none() { // Category-level report
        global.insert("severity".to_string(),severity.clone().unwrap());
        global.insert("highlight".to_string(), aux_severity_highlight(&severity.clone().unwrap()).to_string());
        global.insert("category".to_string(),category.clone().unwrap());
        let whats = backend.task_report(&corpus, &service, severity, category, None);
        // Record the report into "whats" vector
        data.insert("whats",whats);
        // And set the category template
        template = "examples/assets/cortex-report-category.html";
      }
      else { // What-level report
        global.insert("severity".to_string(),severity.clone().unwrap());
        global.insert("highlight".to_string(), aux_severity_highlight(&severity.clone().unwrap()).to_string());
        global.insert("category".to_string(),category.clone().unwrap());
        global.insert("what".to_string(),what.clone().unwrap());
        let entries = backend.task_report(&corpus, &service, severity, category, what);
        // Record the report into "entries" vector
        data.insert("entries",entries);
        // And set the task list template
        template = "examples/assets/cortex-report-task-list.html";
      }

      // Report also the query times
      let report_end = time::get_time();
      let report_duration = (report_end - report_start).num_milliseconds();
      global.insert("report_duration".to_string(),report_duration.to_string());
      // Pass the globals(reports+metadata) onto the stash
      data.insert("global",vec![global]);
      // And render the correct template
      return response.render(template, &data)
    },
    _=>{}}},
    _=>{}}},
    _=>{}}},
    _=>{}};

  // let message = "Error: Corpus ".to_string() + &corpus_name + " does not exist, aborting!";
  return response.send("")
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
    Some(param_encoded) => Some(url::percent_encoding::lossy_utf8_percent_decode(param_encoded.as_bytes()))
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