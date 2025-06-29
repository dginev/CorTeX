// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
use diesel::result::Error;
use diesel::*;
use std::env;
use std::path::PathBuf;
use std::str;
use Archive::*;

use cortex::backend::Backend;
use cortex::concerns::CortexInsertable;
use cortex::helpers::TaskStatus;
use cortex::helpers::{NewTaskMessage, LOADING_LINE_REGEX};
use cortex::models::{Corpus, Service};

static MESSAGE_BUFFER_SIZE: usize = 1_000;

/// traverse a (corpus,service) pair's results into a self-contained redistributable dataset.
fn main() -> Result<(), Error> {
  let start_traverse = time::get_time();
  let chunk_size = 10_240;
  // Setup CorTeX backend data
  let mut backend = Backend::default();
  let mut input_args = env::args();
  let _ = input_args.next(); // discard the script filename
  let corpus_name = match input_args.next() {
    Some(name) => name,
    None => "arxmliv".to_string(),
  };
  let service_name = match input_args.next() {
    Some(name) => name,
    None => "tex_to_html".to_string(),
  };
  let corpus = Corpus::find_by_name(&corpus_name, &mut backend.connection)?;
  let service = Service::find_by_name(&service_name, &mut backend.connection)?;
  let service_filename = format!("{service_name}.zip");

  let mut total_entries = 0;
  let mut messages = Vec::new(); // persist MESSAGE_BUFFER_SIZE messages at a time
                                 // Traverse each status code with produced HTML:
  for status in [
    TaskStatus::NoProblem,
    TaskStatus::Warning,
    TaskStatus::Error,
  ] {
    let tasks = backend.tasks(&corpus, &service, &status);
    println!(
      "Entries found for severity {:?}: {:?}",
      status.to_key(),
      tasks.len()
    );
    for task in tasks {
      total_entries += 1;
      let entry = task.entry;
      let mut dir = PathBuf::from(entry);
      dir.pop();
      dir.push(&service_filename);
      let service_entry = dir.to_string_lossy();
      // Let's open the zip file and grab the result from it
      if let Ok(archive_reader) = Reader::new()
        .unwrap()
        .support_filter_all()
        .support_format_all()
        .open_filename(&service_entry, chunk_size)
      {
        while let Ok(e) = archive_reader.next_header() {
          // Which file are we looking at?
          let pathname = e.pathname();
          if pathname != "cortex.log" {
            continue;
          }
          let mut raw_entry_data = Vec::new();
          while let Ok(chunk) = archive_reader.read_data(chunk_size) {
            raw_entry_data.extend(chunk.into_iter());
          }
          if let Ok(log_string) = str::from_utf8(&raw_entry_data) {
            for line in log_string.lines() {
              if line.is_empty() {
                continue;
              }
              // Special cases are:
              // - "Loading..." info messages
              // - "Processing definitions..." info messages
              if let Some(cap) = LOADING_LINE_REGEX.captures(line) {
                let mut filepath = cap.get(1).map_or("", |m| m.as_str()).to_string();
                let mut filename = cap.get(2).map_or("", |m| m.as_str()).to_string();
                cortex::helpers::utf_truncate(&mut filename, 50);
                filepath += &filename;
                cortex::helpers::utf_truncate(&mut filepath, 50);
                messages.push(NewTaskMessage::new(
                  task.id,
                  "info",
                  "loaded_file".to_string(),
                  filename,
                  filepath,
                ));
              }
            }
          }
          break; // only one log file per archive
        }
      }

      // Check if messages overflow buffer, in which case persist
      if messages.len() > MESSAGE_BUFFER_SIZE {
        backend.connection.transaction::<(), Error, _>(|t_conn| {
          for message in &messages {
            message.create(t_conn)?;
          }
          Ok(())
        })?;
        messages = Vec::new();
      }
    }
  }

  // Flush any remaining messages to DB.
  if !messages.is_empty() {
    backend.connection.transaction::<(), Error, _>(|t_conn| {
      for message in &messages {
        message.create(t_conn)?;
      }
      Ok(())
    })?;
  }

  let end_traverse = time::get_time();

  let traverse_duration = (end_traverse - start_traverse).num_milliseconds();
  println!(
    "-- Message traversal for corpus {:?} and service {:?} took {:?}ms",
    corpus.name, service.name, traverse_duration
  );
  println!("-- traversed {total_entries:?} entries.");
  Ok(())
}
