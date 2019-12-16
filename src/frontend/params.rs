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

/// Configuration in URL query parameter for rerun requests
#[derive(Serialize, Deserialize)]
pub struct RerunRequestParams {
  /// a password-like rerun token
  pub token: String,
  /// a plain text description for the purpose of the rerun
  pub description: String,
}

/// Global configuration for the frontend executable, read in at boot
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct FrontendConfig {
  /// a captcha secret registered with google
  pub captcha_secret: String,
  /// a list of known password-like tokens that allow users to trigger reruns
  pub rerun_tokens: HashMap<String, String>,
}

/// A backend-retrieved report used for filling in Tera-templated pages
#[derive(Serialize)]
pub struct TemplateContext {
  /// global data, as per Rocket examples
  pub global: HashMap<String, String>,
  /// tabular data for reporting on corpora
  pub corpora: Option<Vec<HashMap<String, String>>>,
  /// tabular data for reporting on services
  pub services: Option<Vec<HashMap<String, String>>>,
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
}
impl Default for TemplateContext {
  fn default() -> Self {
    TemplateContext {
      global: HashMap::new(),
      corpora: None,
      services: None,
      entries: None,
      categories: None,
      whats: None,
      workers: None,
      history: None,
      history_serialized: None,
    }
  }
}
