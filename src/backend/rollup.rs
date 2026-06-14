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
//! kept fresh by [`refresh_report_summary`] on the run-completion path (and at a daily cadence
//! while long runs are in flight), which lets us drop the hard Redis dependency. A full
//! `ROLLUP(category, what)` gives three grains keyed by two discriminators:
//!
//! | grain              | `category_is_total` | `what_is_total` | reader              |
//! |--------------------|---------------------|-----------------|---------------------|
//! | per `what`         | 0                   | 0               | [`what_rollup`]     |
//! | per category       | 0                   | 1               | [`category_rollup`] / [`category_total`] |
//! | per severity total | 1                   | 1               | [`severity_total`]  |
//!
//! See `migrations/2026-06-13-150000_report_summary_grand_total/up.sql`.

use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Integer, Text};

/// One precomputed report row from `report_summary`: a `what` (the drill-down report), a
/// category-grain rollup (`what = None`, the "category" report), or the per-severity grand total
/// (`category = ""`, `what = None`).
#[derive(QueryableByName, Debug, Clone)]
pub struct ReportSummaryRow {
  /// Log-message category (the empty string for uncategorized messages and for the grand total).
  #[diesel(sql_type = Text)]
  pub category: String,
  /// The `what` class; `None` for the category-grain and grand-total rows.
  #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
  pub what: Option<String>,
  /// Distinct tasks contributing to this grain — computed by Postgres (distinct counts are not
  /// summable from a finer grain).
  #[diesel(sql_type = diesel::sql_types::BigInt)]
  pub task_count: i64,
  /// Total log messages for this grain.
  #[diesel(sql_type = diesel::sql_types::BigInt)]
  pub message_count: i64,
}

/// Recomputes the `report_summary` materialized view.
///
/// Uses `REFRESH ... CONCURRENTLY` so the rebuild does **not** take an ACCESS EXCLUSIVE lock —
/// report reads keep seeing the previous rollup until the new one is ready (the rebuild is ~2 min
/// at production scale; blocking every report read for that long is unacceptable). CONCURRENTLY
/// needs a populated matview + the unique index from migration
/// `2026-06-14-040000_report_summary_concurrent_refresh`; both hold after migrations, but if
/// CONCURRENTLY is somehow unavailable (e.g. the view was left un-populated) we fall back to a
/// plain refresh so the rollup still updates instead of getting stuck.
///
/// **Must not be called inside a transaction** — `REFRESH ... CONCURRENTLY` forbids it. Every
/// caller (the finalize thread, `mark_new_run`) runs it outside a transaction; keep it that way.
pub(crate) fn refresh_report_summary(connection: &mut PgConnection) -> QueryResult<()> {
  match sql_query("REFRESH MATERIALIZED VIEW CONCURRENTLY report_summary").execute(connection) {
    Ok(_) => Ok(()),
    Err(e) => {
      eprintln!(
        "-- report_summary CONCURRENTLY refresh failed ({e:?}); falling back to a plain refresh"
      );
      sql_query("REFRESH MATERIALIZED VIEW report_summary").execute(connection)?;
      Ok(())
    },
  }
}

/// Category-grain report for a `(corpus, service, severity)`: one row per category with its
/// distinct-task and message counts, ordered by descending task count (ties broken by category name
/// for a stable paging order), windowed to `[offset, offset + limit)`.
pub(crate) fn category_rollup(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
  limit: i64,
  offset: i64,
) -> QueryResult<Vec<ReportSummaryRow>> {
  sql_query(
    "SELECT category, what, task_count, message_count FROM report_summary \
     WHERE corpus_id = $1 AND service_id = $2 AND severity = $3 \
       AND category_is_total = 0 AND what_is_total = 1 \
     ORDER BY task_count DESC, category ASC LIMIT $4 OFFSET $5",
  )
  .bind::<Integer, _>(corpus_id)
  .bind::<Integer, _>(service_id)
  .bind::<Text, _>(severity)
  .bind::<BigInt, _>(limit)
  .bind::<BigInt, _>(offset)
  .get_results(connection)
}

/// The single category-grain row for one `(corpus, service, severity, category)`, i.e. its distinct
/// tasks and total messages — the denominators the `what` drill-down report needs.
pub(crate) fn category_total(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
  category: &str,
) -> QueryResult<Option<ReportSummaryRow>> {
  sql_query(
    "SELECT category, what, task_count, message_count FROM report_summary \
     WHERE corpus_id = $1 AND service_id = $2 AND severity = $3 \
       AND category_is_total = 0 AND what_is_total = 1 AND category = $4",
  )
  .bind::<Integer, _>(corpus_id)
  .bind::<Integer, _>(service_id)
  .bind::<Text, _>(severity)
  .bind::<Text, _>(category)
  .get_result(connection)
  .optional()
}

/// `what`-grain drill-down for a `(corpus, service, severity, category)`: one row per `what`,
/// ordered by descending task count (ties broken by `what` for a stable paging order), windowed to
/// `[offset, offset + limit)`.
pub(crate) fn what_rollup(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
  category: &str,
  limit: i64,
  offset: i64,
) -> QueryResult<Vec<ReportSummaryRow>> {
  sql_query(
    "SELECT category, what, task_count, message_count FROM report_summary \
     WHERE corpus_id = $1 AND service_id = $2 AND severity = $3 \
       AND category_is_total = 0 AND what_is_total = 0 AND category = $4 \
     ORDER BY task_count DESC, what ASC LIMIT $5 OFFSET $6",
  )
  .bind::<Integer, _>(corpus_id)
  .bind::<Integer, _>(service_id)
  .bind::<Text, _>(severity)
  .bind::<Text, _>(category)
  .bind::<BigInt, _>(limit)
  .bind::<BigInt, _>(offset)
  .get_results(connection)
}

/// The per-severity grand total for a `(corpus, service, severity)`: distinct tasks that carry at
/// least one message of this severity, and the total message count. `None` when the severity has no
/// logged messages. (`category` comes back as the empty string.)
pub(crate) fn severity_total(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
) -> QueryResult<Option<ReportSummaryRow>> {
  sql_query(
    "SELECT COALESCE(category, '') AS category, what, task_count, message_count FROM report_summary \
     WHERE corpus_id = $1 AND service_id = $2 AND severity = $3 AND category_is_total = 1",
  )
  .bind::<Integer, _>(corpus_id)
  .bind::<Integer, _>(service_id)
  .bind::<Text, _>(severity)
  .get_result(connection)
  .optional()
}
