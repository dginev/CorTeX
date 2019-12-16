//! Various parameter data structures for the Rocket frontend routes

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
