//! During the August 2025, upgrade to Postgres 17 on our KWARC server that runs CorTeX builds for ar5iv,
//! there was a keyboard mistake that caused the DB to drop the session tables for the full run.
//! 
//! As a means of recovery, observe that all files remain on disk in case of DB mishaps, where each
//! converted task has a "cortex.log" file that persists all messages, including the final conversion
//! status.
//! 
//! This led to the strategy of:
//! 1. Marking all tasks TODO
//! 2. Walking the TODO tasks of a corpus in batches, reading the cortex.log file for each and 
//!    preparing log_* messages in the "usual way" (reusing the code from the dispatcher/server/sink)
//! 3. The remaining TODO tasks are then either "Fatals" that came back without a log file, or 
//!    have other issues. Ideally we actually re-convert those entries for certainty.
//! 

use std::env;
use std::thread;
use std::time::Duration;
use std::path::Path;

use regex::Regex;
use lazy_static::lazy_static;

use diesel::*;

use cortex::backend::Backend;
use cortex::models::{Corpus, Service, Task};
use cortex::helpers::{TaskStatus, generate_report};
use cortex::schema::tasks;
use cortex::schema::tasks::dsl::{corpus_id, service_id,status};

lazy_static! {
  static ref ENTRY_ZIP_NAME_REGEX: Regex =
    Regex::new(r"[^/]+\.zip$").unwrap();
}

// Walks all TODO "status" entries of a corpus, unpacks+scans their cortex.log file, then
// 1. uses conversion status to update "status" value in tasks
// 2. uses log scanner to insert all log messages in log tables, same logic as in dispatcher
//
// Note: we are only recovering the "tex_to_html" service here
fn main() {
  // Read input arguments
  let mut input_args = env::args();
  let _ = input_args.next(); // skip process name
  let corpus_path = match input_args.next() {
    Some(path) => path,
    None => {
      eprintln!("-- Usage: recover_log_reports <corpus_path>");
      std::process::exit(1);
    },
  };
  let mut backend = Backend::default();
  let corpus = Corpus::find_by_path(&corpus_path, &mut backend.connection)
    .expect("Please provide a path to a registered corpus");
  let service = Service::find_by_name("tex_to_html", &mut backend.connection)
    .expect("DB connection failed: could not find tex_to_hml service");
  // Load all tasks at once, since we will be marking them as completed as we go
  // and batching will get polluted.
  let batch_size = 100;
  let tasks : Vec<Task> = tasks::table
  .filter(corpus_id.eq(corpus.id))
  .filter(service_id.eq(service.id))
  .filter(status.eq(TaskStatus::TODO.raw()))
  .get_results(&mut backend.connection).expect("DB connection failed");
  eprintln!("-- will scan and update {} tasks a batch of {} at a time", tasks.len(), batch_size);
  let mut tasks_iter = tasks.into_iter();
  loop {
    let batch: Vec<Task> = tasks_iter.by_ref().take(batch_size).collect();
    if batch.is_empty() { break; }
    let batch_len = batch.len();
    let mut batch_reports = Vec::new();
    for task in batch {
      let html_entry = ENTRY_ZIP_NAME_REGEX.replace(&task.entry,"tex_to_html.zip").to_string();
      let html_entry_path = Path::new(&html_entry);
      if html_entry_path.exists() {
        let report = generate_report(task, html_entry_path);
        batch_reports.push(report);
      } else {
        eprintln!("-- Missing result file: {:?}", html_entry);
      }
    }
    let mut success = false;
    let reports_len : usize = batch_reports.iter().map(|r| r.messages.len()).sum();
    if let Err(e) = backend.mark_done(&batch_reports) {
      eprintln!("-- mark_done attempt failed: {e:?}");
      // DB persist failed, retry
      let mut retries = 0;
      while retries < 3 {
        thread::sleep(Duration::new(2, 0)); // wait 2 seconds before retrying, in case this is latency related
        retries += 1;
        match backend.mark_done(&batch_reports) {
          Ok(_) => {
            success = true;
            break;
          },
          Err(e) => eprintln!("-- mark_done retry failed: {e:?}"),
        };
      }
    } else {
      success = true;
    }
    if success {
      eprintln!("-- Successfully saved {} reports for {} tasks", reports_len, batch_len);
    }
  }
}
