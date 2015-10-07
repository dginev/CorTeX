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
use std::path::Path;
use std::fs;
// use std::io::Read;
use std::io::Error;
use nickel::{Nickel, HttpRouter}; //, MediaType, JsonBody
use hyper::header::Location;
use nickel::status::StatusCode;
use nickel::QueryString;
// use nickel::status::StatusCode;
// use hyper::header::Location;

use cortex::sysinfo;
use cortex::backend::{Backend};
use cortex::data::{Corpus};

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
    
    //middleware function logs each request to console
    server.utilize(middleware! { |request|
        println!("logging request: {:?}", request.origin.uri);
    });
    // Root greeter
    server.get("/", middleware! { |_, response|
        let mut data = HashMap::new();
        data.insert("title", "Framework Overview".to_string());
        return response.render("examples/assets/cortex-overview.html", &data);
    });
    server.get("/overview", middleware! { |_, response|
        let mut data = HashMap::new();
        data.insert("title", "Framework Overview".to_string());
        return response.render("examples/assets/cortex-overview.html", &data);
    });

    // Admin interface
    server.get("/admin", middleware! { |_, response|
      let mut data = HashMap::new();
      data.insert("title", "Admin Interface".to_string());
      match sysinfo::report(&mut data) {
        Ok(_) => {},
        Err(e) => println!("Sys report failed: {:?}", e)
      };
      return response.render("examples/assets/cortex-admin.html", &data);
    });

    server.get("/add_corpus", middleware! { |request, mut response| 
      let backend = Backend::default();
      let mut data = HashMap::new();
      let mut message : String ;
      let mut corpus_path;
      let query = request.query();
      if let Some(p) = query.get("path") {
        corpus_path = p.to_string();
      } else {
        data.insert("message", "Error: Please provide a path!".to_string());
        return response.render("examples/assets/cortex-admin.html", &data);
      }
      println!("Adding Path: {:?}", corpus_path);
      let complex : bool = query.get("setup") != Some("canonical");
      let path = Path::new(&corpus_path);
      match fs::metadata(path) {
        Ok(_) => {},
        Err(_) => {
          message = "Error: Path ".to_string() + &corpus_path + " does not exist, aborting!";
          
          response.set(Location("/admin".into()));
          response.set(StatusCode::TemporaryRedirect);
          return response.send("")
        }
      };
      let corpus_name = path.file_stem().unwrap().to_str().unwrap().to_string();

      // Queue the corpus for import using the task database:
      let input_corpus = Corpus {
        id : None,
        name : corpus_name,
        path : corpus_path.clone(),
        complex : complex,
      };
      message = match backend.add(input_corpus) {
        Ok(_) => "Successfully Queued ".to_string() + &corpus_path+ " for Import.",
        Err(_) => "Failed to add corpus, please retry!".to_string()
      };
      data.insert("message", message);
      return response.render("examples/assets/cortex-admin.html", &data);
    });

    server.get("/corpus-report", middleware! { |_, response|
      let mut data = HashMap::new();
      data.insert("title", "Corpus Report".to_string());
      return response.render("examples/assets/cortex-report.html", &data);
    });

    server.listen("127.0.0.1:6767");
}