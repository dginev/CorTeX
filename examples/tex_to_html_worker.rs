// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

use pericortex::worker::{TexToHtmlWorker, Worker};

fn main() {
  println!("Starting up TeX to HTML worker and awaiting jobs...");
  let _worker = TexToHtmlWorker::default().start(None);
}
