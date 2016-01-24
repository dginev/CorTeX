// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

extern crate cortex;
extern crate time;
extern crate libxml;

use cortex::backend::{Backend};
use cortex::importer::Importer;

/// Extends all corpora registered with the CorTeX backend, with any new available sources
///  (example usage: arXiv.org releases new source bundles every month, which warrant an update at the same frequency.)
fn main() {
