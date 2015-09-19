// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate rustc_serialize;

// use std::collections::HashMap;
// use std::path::Path;
// use std::fs;
use std::env;
// use std::io::Read;
// use std::io::Error;
// use cortex::sysinfo;
use cortex::backend::{Backend};
use cortex::data::{Corpus, Task, TaskStatus};

fn main() {
  let mut input_args = env::args();
  let corpus_path = match input_args.next() {
    Some(path) => path,
    None => "/arXMLiv/modern/".to_string()
  };
  let corpus_name = match input_args.next() {
    Some(name) => name,
    None => "arXMLiv".to_string()
  };
  println!("Importing arXiv...");
  let backend = Backend::default();

  let corpus = backend.add(
    Corpus {
      id : None,
      name : corpus_name,
      path : corpus_path.clone(),
      complex : true,
    }).unwrap();

  backend.add(
    Task {
      id : None,
      entry : corpus_path,
      serviceid : 1, // Init service always has id 1
      corpusid : corpus.id.unwrap().clone(),
      status : TaskStatus::TODO.raw()
    }).unwrap();
}