extern crate rustc_serialize;
#[macro_use] extern crate nickel;

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::io::Error;
use nickel::{Nickel, HttpRouter, JsonBody};


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

fn main() {
    let mut server = Nickel::new();

    
    let pre : String = slurp_file("examples/assets/layout-cortex-pre.html").unwrap();
    let post : String = slurp_file("examples/assets/layout-cortex-post.html").unwrap();

    // server.post("/a/post/request", middleware! { |request, response|
    //     let person = request.json_as::<Person>().unwrap();
    //     format!("Hello {} {}", person.firstname, person.lastname)
    // });

    server.get("/", middleware! { |_, response|
        let mut data = HashMap::new();
        data.insert("title", "CorTeX Framework - Overview");
        data.insert("layout-cortex-pre", &pre);
        data.insert("layout-cortex-post", &post);
        return response.render("examples/assets/cortex-overview.html", &data);
    });

    server.listen("127.0.0.1:6767");
}