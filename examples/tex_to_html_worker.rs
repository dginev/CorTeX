// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate zmq;
extern crate rand;
extern crate tempfile;
extern crate pericortex;

use pericortex::worker::{Worker, TexToHtmlWorker};

fn main() {
  println!("Starting up TeX to HTML worker and awaiting jobs...");
  let _worker = TexToHtmlWorker::default().start(None);
}