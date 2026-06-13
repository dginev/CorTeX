// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Reads over the `report_summary` materialized view (Arm 14 #6).
//!
//! The view precomputes the category/what aggregate counts that the live `task_report` otherwise
//! derives with an O(millions of log rows) join+group+sort. Reading it is an indexed lookup; it is
//! kept fresh by [`refresh_report_summary`] on the run-completion path, which lets us drop the hard
//! Redis dependency. See `migrations/2026-06-13-140000_create_report_summary/up.sql`.

use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{Integer, Text};

/// One precomputed report row from `report_summary`: either a category-grain rollup
/// (`what = None`, the "category" report) or a single `what` (the drill-down report).
#[derive(QueryableByName, Debug, Clone)]
pub struct ReportSummaryRow {
  /// Log-message category (the empty string for uncategorized messages).
  #[diesel(sql_type = Text)]
  pub category: String,
  /// The `what` class; `None` for the category-grain rollup row.
  #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
  pub what: Option<String>,
  /// Distinct tasks contributing to this (category[, what]) — computed by Postgres, not summable.
  #[diesel(sql_type = diesel::sql_types::BigInt)]
  pub task_count: i64,
  /// Total log messages for this (category[, what]).
  #[diesel(sql_type = diesel::sql_types::BigInt)]
  pub message_count: i64,
}

/// Recomputes the `report_summary` materialized view. Call after a run completes (cheap relative to
/// per-read recomputation; brief lock — see the migration note on `REFRESH ... CONCURRENTLY`).
pub(crate) fn refresh_report_summary(connection: &mut PgConnection) -> QueryResult<()> {
  sql_query("REFRESH MATERIALIZED VIEW report_summary").execute(connection)?;
  Ok(())
}

/// Category-grain report for a `(corpus, service, severity)`: one row per category with its
/// distinct-task and message counts (the rollup rows, `what IS NULL`).
pub(crate) fn category_rollup(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
) -> QueryResult<Vec<ReportSummaryRow>> {
  sql_query(
    "SELECT category, what, task_count, message_count FROM report_summary \
     WHERE corpus_id = $1 AND service_id = $2 AND severity = $3 AND what_is_total = 1 \
     ORDER BY task_count DESC, category ASC",
  )
  .bind::<Integer, _>(corpus_id)
  .bind::<Integer, _>(service_id)
  .bind::<Text, _>(severity)
  .get_results(connection)
}

/// `what`-grain drill-down for a `(corpus, service, severity, category)`: one row per `what`.
pub(crate) fn what_rollup(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
  category: &str,
) -> QueryResult<Vec<ReportSummaryRow>> {
  sql_query(
    "SELECT category, what, task_count, message_count FROM report_summary \
     WHERE corpus_id = $1 AND service_id = $2 AND severity = $3 AND what_is_total = 0 \
       AND category = $4 ORDER BY task_count DESC, what ASC",
  )
  .bind::<Integer, _>(corpus_id)
  .bind::<Integer, _>(service_id)
  .bind::<Text, _>(severity)
  .bind::<Text, _>(category)
  .get_results(connection)
}
