// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

use cortex::backend::Backend;
use cortex::models::Corpus;
use regex::Regex;
use std::env;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use Archive::*;

/// Reads a lit of arXiv ids given on input, and packages the respective `CorTeX` entries into a
/// new sandbox.
fn main() {
  // Read input arguments
  let mut input_args = env::args();
  let _ = input_args.next(); // skip process name
  let ids_filepath = match input_args.next() {
    Some(path) => path,
    None => "arxiv_ids.txt".to_string(),
  };
  let sandbox_path = match input_args.next() {
    Some(path) => path,
    None => "sandbox.zip".to_string(),
  };

  // Fish out the arXiv root directory from CorTeX
  let backend = Backend::default();
  let sandbox_start = time::get_time();
  let corpus = match Corpus::find_by_name("arxmliv", &backend.connection) {
    Ok(corpus) => corpus,
    _ => {
      println!("--  The arXMLiv corpus isn't registered in the CorTeX backend, aborting.");
      return;
    },
  };
  let corpus_path = corpus.path;
  println!("-- using arXiv path: {:?}", corpus_path);

  // Prepare to read in the arXiv ids we will sandbox
  let ids_fh = match File::open(&ids_filepath) {
    Ok(fh) => fh,
    _ => {
      println!(
        "-- Couldn't read file with arXiv ids {:?}, aborting.",
        ids_filepath
      );
      return;
    },
  };

  // Prepare a sandbox archive file writer
  let mut sandbox_writer = Writer::new()
    .unwrap()
    .set_compression(ArchiveFilter::None)
    .set_format(ArchiveFormat::Zip);
  sandbox_writer.open_filename(&sandbox_path).unwrap();

  // Read in ids, and whenever a source exists, write it to the sandbox archive
  let reader = BufReader::new(&ids_fh);
  let no_version = Regex::new(r"v\d+$").unwrap();
  let old_style = Regex::new(r"^([^/]+)/(\d\d\d\d)(.+)$").unwrap();
  let new_style = Regex::new(r"^(\d\d\d\d)\.(.+)$").unwrap();

  let mut counter = 0;

  for line in reader.lines() {
    let mut id = line.unwrap();
    id = no_version.replace(&id, "").to_string();
    // We have two styles of ids:
    //   - old, such as "cond-mat/0306509", which map to ""
    //   - new, such as "1511.03528", which map to "/rootpath/idmonth/id/id.zip"
    let entry = match old_style.captures(&id) {
      None => {
        match new_style.captures(&id) {
          None => {
            println!("-- Malformed arxiv id: {:?}", id);
            None
          },
          Some(caps) => {
            // Obtain new-style entry path
            let month = caps.get(1).unwrap().as_str();
            let paper = caps.get(0).unwrap().as_str();
            Some(month.to_owned() + "/" + paper + "/" + paper + ".zip")
          },
        }
      },
      Some(caps) => {
        // Obtain old-style entry path
        let month = caps.get(2).unwrap().as_str();
        let paper =
          caps.get(1).unwrap().as_str().to_owned() + month + caps.get(3).unwrap().as_str();
        Some(month.to_owned() + "/" + &paper + "/" + &paper + ".zip")
      },
    };
    if entry.is_none() {
      continue;
    }
    let relative_entry_path = entry.unwrap();
    let entry_path = corpus_path.clone() + "/" + &relative_entry_path;
    match File::open(entry_path) {
      Err(_) => println!("-- missing arXiv source for {:?}", id),
      Ok(mut entry_fh) => {
        let mut buffer = Vec::new();
        if entry_fh.read_to_end(&mut buffer).is_ok() {
          // Everything looks ok with this paper, adding it to the sandbox:
          counter += 1;
          match sandbox_writer.write_header_new(&relative_entry_path, buffer.len() as i64) {
            Ok(_) => {},
            Err(e) => {
              println!("Couldn't write header {:?}: {:?}", relative_entry_path, e);
              continue;
            },
          };
          match sandbox_writer.write_data(buffer) {
            Ok(_) => {},
            Err(e) => println!(
              "Failed to write data to {:?} because {:?}",
              relative_entry_path.clone(),
              e
            ),
          };
        }
      },
    };
  }

  let sandbox_end = time::get_time();
  let sandbox_duration = (sandbox_end - sandbox_start).num_milliseconds();
  println!(
    "-- Sandboxing {:?} arXiv papers took took {:?}ms",
    counter, sandbox_duration
  );
}
