extern crate hyper;
#[macro_use] extern crate nickel;
extern crate cortex;
extern crate rustc_serialize;

use std::collections::HashMap;
// use std::path::Path;
// use std::io::Read;
use std::io::Error;
use nickel::{Nickel, HttpRouter}; //, MediaType, JsonBody
use hyper::header::Location;
use nickel::status::StatusCode;
use nickel::QueryString;
// use nickel::status::StatusCode;
// use hyper::header::Location;

use cortex::sysinfo;
// use cortex::backend::{Corpus, Backend};

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
    // let backend = Backend::default();
    //middleware function logs each request to console
    server.utilize(middleware! { |request|
        println!("logging request: {:?}", request.origin.uri);
    });
    // Root greeter
    server.get("/", middleware! { |_, response|
        let mut data = HashMap::new();
        data.insert("title", "Framework Overview | CorTeX".to_string());
        return response.render("examples/assets/cortex-overview.html", &data);
    });
    server.get("/overview", middleware! { |_, response|
        let mut data = HashMap::new();
        data.insert("title", "Framework Overview | CorTeX".to_string());
        return response.render("examples/assets/cortex-overview.html", &data);
    });

    // Admin interface
    server.get("/admin", middleware! { |_, response|
      let mut data = HashMap::new();
      data.insert("title", "Admin Interface | CorTeX".to_string());
      match sysinfo::report(&mut data) {
        Ok(_) => {},
        Err(e) => println!("Sys report failed: {:?}", e)
      };
      return response.render("examples/assets/cortex-admin.html", &data);
    });

    server.get("/add_corpus", middleware! { |request, mut response| 
      let test = request.query();
      println!("{:?}", test);
      // let corpus_path = match request.param("path") {
      //   Some(p) => {
      //     println!("Got path: {:?}", p);
      //     p
      //   },
      //   None =>{
      //     let message = "Error: Please provide a path!";
      //     println!("{:?}", message);
      //     println!("{:?}", request.param("setup"));
          
      //     response.set(Location("/admin".into()));
      //     response.set(StatusCode::TemporaryRedirect);
      //     return response.send("")
      //   }
      // };
      // println!("Adding Path: {:?}", corpus_path);
      // let path = Path::new(corpus_path);
      // match fs::metadata(path) {
      //   Ok(_) => {},
      //   Err(_) => {
      //     let message = "Error: Path ".to_string() + corpus_path + " does not exist, aborting!";
      //     println!("{:?}", message);
          
      //     response.set(Location("/admin".into()));
      //     response.set(StatusCode::TemporaryRedirect);
      //     return response.send("")
      //   }
      // };
      // let corpus_name = path.file_stem().unwrap();
      // (corpus=>$corpus_name,entry=>$path,service=>'init',status=>-5)

      // let corpus_id = backend.corpus_id(corpus_name);
      // if ($overwrite || (!$corpus_exists)) {
      //   $backend->taskdb->delete_corpus($corpus_name);
      //   $backend->taskdb->register_corpus($corpus_name);
      //   $backend->docdb->delete_directory($path,$path); }
      // # Queue the corpus for import using the task database:
      // $backend->taskdb->queue(corpus=>$corpus_name,entry=>$path,service=>'init',status=>-5);
      // let message="Successfully Queued ".to_string() + corpus_path+ " for Import.";

      response.set(Location("/admin".into()));
      response.set(StatusCode::TemporaryRedirect);
      return response.send("")
    });

    server.listen("127.0.0.1:6767");
}