//! Cache-enabled task reports, delegating to `Backend` for the core reporting logic
use crate::backend::Backend;
use crate::backend::TaskReportOptions;
use crate::frontend::params::ReportParams;
use crate::models::{Corpus, Service};
use redis::Commands;
use rocket::request::Form;
use std::collections::HashMap;

/// Cached proxy over `Backend::task_report`
pub fn task_report(
  global: &mut HashMap<String, String>,
  corpus: &Corpus,
  service: &Service,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  params: &Option<Form<ReportParams>>,
) -> Vec<HashMap<String, String>>
{
  let all_messages = match params {
    None => false,
    Some(ref params) => *params.all.as_ref().unwrap_or(&false),
  };
  let offset = match params {
    None => 0,
    Some(ref params) => *params.offset.as_ref().unwrap_or(&0),
  };
  let page_size = match params {
    None => 100,
    Some(ref params) => *params.page_size.as_ref().unwrap_or(&100),
  };
  let fetched_report;
  let mut time_val: String = time::now().rfc822().to_string();

  let mut redis_connection = match redis::Client::open("redis://127.0.0.1/") {
    Ok(redis_client) => match redis_client.get_connection() {
      Ok(rc) => Some(rc),
      _ => None,
    },
    _ => None,
  };

  let mut cache_key = String::new();
  let mut cache_key_time = String::new();
  let cached_report: Vec<HashMap<String, String>> =
    if what.is_some() || severity == Some("no_problem".to_string()) {
      vec![]
    } else {
      // Levels 1-3 get cached, except no_problem pages
      let key_tail = match severity.clone() {
        Some(severity) => {
          let cat_tail = match category.clone() {
            Some(category) => {
              let what_tail = match what.clone() {
                Some(what) => "_".to_string() + &what,
                None => String::new(),
              };
              "_".to_string() + &category + &what_tail
            },
            None => String::new(),
          };
          "_".to_string() + &severity + &cat_tail
        },
        None => String::new(),
      } + if all_messages { "_all_messages" } else { "" };
      cache_key = corpus.id.to_string() + "_" + &service.id.to_string() + &key_tail;
      cache_key_time = cache_key.clone() + "_time";
      let cache_val: String = if let Some(ref mut rc) = redis_connection {
        rc.get(cache_key.clone()).unwrap_or_default()
      } else {
        String::new()
      };
      if cache_val.is_empty() {
        vec![]
      } else {
        serde_json::from_str(&cache_val).unwrap_or_default()
      }
    };

  if cached_report.is_empty() {
    let backend = Backend::default();
    fetched_report = backend.task_report(TaskReportOptions {
      corpus,
      service,
      severity_opt: severity.clone(),
      category_opt: category,
      what_opt: what.clone(),
      all_messages,
      offset,
      page_size,
    });
    if what.is_none() && severity != Some("no_problem".to_string()) {
      let report_json: String = serde_json::to_string(&fetched_report).unwrap();
      // don't cache the task list pages

      if let Some(ref mut rc) = redis_connection {
        let _: () = rc.set(cache_key, report_json).unwrap();
      }

      if let Some(ref mut rc) = redis_connection {
        let _: () = rc.set(cache_key_time, time_val.clone()).unwrap();
      }
    }
  } else {
    // Get the report time, so that the user knows where the data is coming from
    time_val = if let Some(ref mut rc) = redis_connection {
      match rc.get(cache_key_time) {
        Ok(tval) => tval,
        Err(_) => time::now().rfc822().to_string(),
      }
    } else {
      time::now().rfc822().to_string()
    };
    fetched_report = cached_report;
  }

  // Setup the return

  let from_offset = offset;
  let to_offset = offset + page_size;
  global.insert("from_offset".to_string(), from_offset.to_string());
  if from_offset >= page_size {
    // TODO: properly do tera ifs?
    global.insert("offset_min_false".to_string(), "true".to_string());
    global.insert(
      "prev_offset".to_string(),
      (from_offset - page_size).to_string(),
    );
  }

  if fetched_report.len() >= page_size as usize {
    global.insert("offset_max_false".to_string(), "true".to_string());
  }
  global.insert(
    "next_offset".to_string(),
    (from_offset + page_size).to_string(),
  );

  global.insert("offset".to_string(), offset.to_string());
  global.insert("page_size".to_string(), page_size.to_string());
  global.insert("to_offset".to_string(), to_offset.to_string());
  global.insert("report_time".to_string(), time_val);

  fetched_report
}
