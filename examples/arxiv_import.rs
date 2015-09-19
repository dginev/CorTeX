// Copyright 2015 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate rustc_serialize;

use std::collections::HashMap;
use std::path::Path;
use std::fs;
// use std::io::Read;
use std::io::Error;

use cortex::sysinfo;
use cortex::backend::{Backend};
use cortex::data::{Corpus};

fn main() {
  println!("Importing arXiv...");
}