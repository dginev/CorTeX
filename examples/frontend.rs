extern crate hyper;
#[macro_use] extern crate nickel;
extern crate rustc_serialize;

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::io::Error;
use nickel::{Nickel, HttpRouter, JsonBody, MediaType};
use nickel::status::StatusCode;
use hyper::header::Location;


#[derive(RustcDecodable, RustcEncodable)]
struct Person {
    firstname: String,
    lastname:  String,
}

fn slurp_file (path : &'static str) -> Result<String, Error> {
  let mut f = try!(File::open(path));
  let mut content = String::new();
  try!(f.read_to_string(&mut content));
  Ok(content)
}
fn default_layout() -> HashMap<&'static str, String> {
  let mut data = HashMap::new();
  // Can be loaded from Redis at some point, load from file for now, doesn't matter:
  let pre : String = slurp_file("examples/assets/layout-cortex-pre.html").unwrap();
  let post : String = slurp_file("examples/assets/layout-cortex-post.html").unwrap();
  data.insert("layout-cortex-pre", pre);
  data.insert("layout-cortex-post", post);
  data }

fn main() {
    let mut server = Nickel::new();
    // Root greeter
    server.get("/", middleware! { |_, response|
        let mut data = default_layout();
        data.insert("title", "Framework Overview | CorTeX".to_string());
        return response.render("examples/assets/cortex-overview.html", &data);
    });
    server.get("/overview", middleware! { |_, response|
        let mut data = default_layout();
        data.insert("title", "Framework Overview | CorTeX".to_string());
        return response.render("examples/assets/cortex-overview.html", &data);
    });

    // Admin interface
    server.get("/admin", middleware! { |_, response|
      let mut data = default_layout();
      data.insert("title", "Admin Interface | CorTeX".to_string());
      return response.render("examples/assets/cortex-admin.html", &data);
    });

    server.listen("127.0.0.1:6767");
}