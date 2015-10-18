// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate hyper;
#[macro_use] extern crate nickel;
extern crate cortex;
extern crate rustc_serialize;

use std::collections::HashMap;
// use std::path::Path;
// use std::fs;
// use std::io::Read;
use std::io::Error;
use nickel::{Nickel, Mountable, StaticFilesHandler, HttpRouter}; //, MediaType, JsonBody
use hyper::header::Location;
use nickel::status::StatusCode;
// use nickel::QueryString;
// use nickel::status::StatusCode;
// use hyper::header::Location;

use cortex::sysinfo;
use cortex::backend::{Backend};
use cortex::data::{Corpus, CortexORM, Service};


#[derive(RustcDecodable, RustcEncodable)]
struct Person {
    firstname: String,
    lastname:  String,
}

// fn slurp_file (path : &'static str) -> Result<String, Error> {
//   let mut f = try!(File::open(path));
//   let mut content = String::new();
//   try!(f.read_to_string(&mut content));
//   Ok(content)
// }

fn main() {
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
    // Root greeter
    server.get("/", middleware! { |_, response|
        let mut data = HashMap::new();
        let mut global = HashMap::new();
        global.insert("title", "Framework Overview".to_string());
        global.insert("description", "An analysis framework for corpora of TeX/LaTeX documents - overview.".to_string());
        
        let backend = Backend::default();
        let corpora = backend.corpora().iter()
        .map(|c| c.to_hash()).collect::<Vec<_>>();

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
      let corpus_name = request.param("corpus_name").unwrap();
      let corpus_result = Corpus{id: None, name: corpus_name.to_string(), path : String::new(), complex : true}.select_by_key(&backend.connection);
      match corpus_result {
        Ok(corpus_select) => {
          match corpus_select {
            Some(corpus) => {
              global.insert("title", "Registered services for ".to_string() + corpus_name.clone());
              global.insert("description", "An analysis framework for corpora of TeX/LaTeX documents - registered services for ".to_string()+ corpus_name.clone());
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

    server.get("/corpus/:corpus_name/:service_name", middleware! { |request, mut response|
      let mut data = HashMap::new();
      let mut global = HashMap::new();
      let backend = Backend::default();
      let corpus_name = request.param("corpus_name").unwrap();
      let service_name = request.param("service_name").unwrap();
      let corpus_result = Corpus{id: None, name: corpus_name.to_string(), path : String::new(), complex : true}.select_by_key(&backend.connection);
      match corpus_result { Ok(corpus_select) => {
      match corpus_select {Some(corpus) => {
        let service_result = Service{id: None, name: service_name.to_string(),  complex: true, version: 0.1, inputconverter: None, inputformat: String::new(), outputformat:String::new()}.select_by_key(&backend.connection);
        match service_result { Ok(service_select) => {
        match service_select {Some(service) => {
          global.insert("title".to_string(), "Corpus Report for ".to_string() + corpus_name.clone());
          global.insert("description".to_string(), "An analysis framework for corpora of TeX/LaTeX documents - statistical reports for ".to_string()+ corpus_name.clone());
          global.insert("corpus_name".to_string(), corpus_name.to_string());
          global.insert("service_name".to_string(), service_name.to_string());

          let report = backend.progress_report(&corpus, &service);
          for (key, val) in report.iter() {
            global.insert(key.clone(), val.to_string());
          }
          data.insert("global",vec![global]);
          return response.render("examples/assets/cortex-report.html", &data);
        },
        _=>{}}},
        _=>{}}},
        _=>{}}},
        _=>{}};

      // let message = "Error: Corpus ".to_string() + &corpus_name + " does not exist, aborting!";
      response.set(Location("/".into()));
      response.set(StatusCode::TemporaryRedirect);
      return response.send("")

    });

    server.listen("127.0.0.1:6767");
}