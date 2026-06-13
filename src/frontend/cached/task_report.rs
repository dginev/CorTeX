//! Thin presentation proxy over [`crate::backend::Backend::task_report`].
//!
//! The category/`what` aggregate grains are served by the `report_summary` rollup (an indexed
//! lookup, kept fresh on the run-completion path), so the former Redis cache that shielded the
//! expensive live aggregation is gone — Redis is no longer a hard dependency of the frontend. This
//! function now only translates request params into a [`TaskReportOptions`], delegates, and records
//! the pagination/`report_time` globals the report templates expect.
use crate::backend::Backend;
use crate::backend::TaskReportOptions;
use crate::frontend::params::ReportParams;
use crate::models::{Corpus, Service};
use std::collections::HashMap;

/// Renders a task report, filling in the pagination + provenance globals the templates consume.
pub fn task_report(
  global: &mut HashMap<String, String>,
  corpus: &Corpus,
  service: &Service,
  severity: Option<String>,
  category: Option<String>,
  what: Option<String>,
  params: &Option<ReportParams>,
) -> Vec<HashMap<String, String>> {
  let all_messages = params.as_ref().and_then(|p| p.all).unwrap_or(false);
  let offset = params.as_ref().and_then(|p| p.offset).unwrap_or(0);
  let page_size = params.as_ref().and_then(|p| p.page_size).unwrap_or(100);

  let mut backend = Backend::default();
  let fetched_report = backend.task_report(TaskReportOptions {
    corpus,
    service,
    severity_opt: severity,
    category_opt: category,
    what_opt: what,
    all_messages,
    offset,
    page_size,
  });

  // Pagination + provenance globals for the templates.
  let from_offset = offset;
  let to_offset = offset + page_size;
  global.insert("from_offset".to_string(), from_offset.to_string());
  if from_offset >= page_size {
    global.insert("offset_min_false".to_string(), "true".to_string());
    global.insert(
      "prev_offset".to_string(),
      (from_offset - page_size).to_string(),
    );
  }
  // A full page of *data* rows implies there may be another page. Aggregate reports append summary
  // rows (`total`, `no_messages`) that are not part of the paged set, so exclude them from the
  // count — otherwise the "next" control would over-signal.
  let data_rows = fetched_report
    .iter()
    .filter(|row| {
      let name = row.get("name").map(String::as_str).unwrap_or("");
      name != "total" && name != "no_messages"
    })
    .count();
  if data_rows >= page_size as usize {
    global.insert("offset_max_false".to_string(), "true".to_string());
  }
  global.insert(
    "next_offset".to_string(),
    (from_offset + page_size).to_string(),
  );
  global.insert("offset".to_string(), offset.to_string());
  global.insert("page_size".to_string(), page_size.to_string());
  global.insert("to_offset".to_string(), to_offset.to_string());
  global.insert("report_time".to_string(), time::now().rfc822().to_string());

  fetched_report
}
