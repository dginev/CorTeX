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
// use Archive::*;

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

    let batch_size = 100;
  // Fetch a batch of tasks for the given corpus and service
  let mut offset = 0;
  loop {
    let batch : Vec<Task> = tasks::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .filter(status.eq(TaskStatus::TODO.raw()))
      .offset(offset)
      .limit(batch_size)
      .get_results(&mut backend.connection).expect("DB connection failed");
    let batch_len = batch.len();
    let mut batch_reports = Vec::new();
    eprintln!("-- Processing {} tasks from offset {}", batch_len, offset);
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
    if batch_len < batch_size as usize {
      break; // No more tasks to process
    }
    offset += batch_size;
  }
}

