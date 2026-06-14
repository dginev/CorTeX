//! Various parameter data structures for the Rocket frontend routes
use crate::models::RunMetadata;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
