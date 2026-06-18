//! Various parameter data structures for the Rocket frontend routes
use crate::models::RunMetadata;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Largest report **page size** any report path (human screen or agent endpoint) will honour. The
/// deepest report rung is a per-task entry list, so an unbounded `page_size` would `LIMIT` the
/// whole list into one response/render (the unbounded-load class, principle #6); clamp it
/// everywhere.
pub const MAX_REPORT_PAGE_SIZE: i64 = 1000;

/// Largest report **offset** any report path will honour. `OFFSET` is scan-and-discard, so a deep
/// offset is a multi-second query that pins a connection (KNOWN_ISSUES P-4); cap the paginate
/// depth. The reports are an inspection surface (look at affected papers) — bulk action uses rerun,
/// which filters server-side without enumerating.
pub const MAX_REPORT_OFFSET: i64 = 100_000;

#[derive(FromForm)]
/// Configuration parameters for a frontend reports page
pub struct ReportParams {
  /// show all tasks, or only those of the current severity
  pub all: Option<bool>,
  /// offset for paging in SQL
  pub offset: Option<i64>,
  /// page size for paging in SQL
  pub page_size: Option<i64>,
}

/// The JSON body of a human rerun request. Auth is the signed-in [`crate::frontend::actor::
/// AdminSession`] cookie now — no token in the body — so only the free-text purpose remains.
#[derive(Serialize, Deserialize)]
pub struct RerunRequestParams {
  /// a plain text description for the purpose of the rerun
  pub description: String,
}

/// A backend-retrieved report used for filling in Tera-templated pages
#[derive(Serialize, Default)]
pub struct TemplateContext {
  /// global data, as per Rocket examples
  pub global: HashMap<String, String>,
  /// tabular data for reporting on corpora
  pub corpora: Option<Vec<HashMap<String, String>>>,
  /// tabular data for reporting on services
  pub services: Option<Vec<HashMap<String, String>>>,
  /// all registered services (the corpus screen's "activate a service" picker)
  pub all_services: Option<Vec<HashMap<String, String>>>,
  /// tabular data for reporting on entries
  pub entries: Option<Vec<HashMap<String, String>>>,
  /// tabular data for reporting on message `categories`
  pub categories: Option<Vec<HashMap<String, String>>>,
  /// tabular data for reporting on message `whats`
  pub whats: Option<Vec<HashMap<String, String>>>,
  /// tabular data for reporting on workers
  pub workers: Option<Vec<HashMap<String, String>>>,
  /// tabular data for reporting on rerun history
  pub history: Option<Vec<RunMetadata>>,
  /// serialized data for easy plotting of rerun history
  pub history_serialized: Option<String>,
  /// Whether the current viewer is a signed-in admin. Gates admin-only affordances in the shared
  /// templates (e.g. the corpus screen's "Corpus actions"); defaults to `false` (anonymous), so a
  /// page that doesn't set it shows nothing privileged.
  pub is_admin: bool,
}
