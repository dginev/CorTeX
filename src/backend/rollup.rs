// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Per-`(corpus, service, severity)`-scoped report cache for the category/what drill-down reports.
//!
//! These functions used to read a global `report_summary` MATERIALIZED VIEW whose `REFRESH` scanned
//! all five `log_*` tables across **every** corpus (~99 GB / 345M rows, ~16 min under fleet load)
//! and starved the conversion finalize path. That global cube was retired. Reports are now backed
//! by `report_grain_cache`, a regular table populated **one `(corpus, service, severity)` slice at
//! a time** — there is no code path that aggregates more than one corpus at once, so reporting can
//! never again stall conversions. A slice is (re)computed:
//!
//! * **cold miss** — a reader for an un-cached slice runs the scoped aggregation once
//!   ([`populate_scope`]) and stores it, then reads. This is the only place the heavy scan happens,
//!   and it is bounded to one log table for one corpus (ms for a typical corpus; a couple of
//!   seconds for the largest, paid once until invalidated).
//! * **rerun** — [`invalidate_scope`] clears the reran scope; the next view repopulates fresh.
//! * **run-completion** — the dispatcher finalize thread invalidates only the scopes it touched.
//! * **force refresh** — [`invalidate_all`] drops every cached slice (a cheap DELETE, not a scan);
//!   each repopulates lazily per scope on its next view.
//!
//! The cache rows mirror the retired matview's `ROLLUP(category, what)` grains, so the readers
//! `SELECT` them with the same per-grain filters:
//!
//! | grain              | rows (`category_is_total`, `what_is_total`) | reader                                   |
//! |--------------------|---------------------------------------------|------------------------------------------|
//! | per `what`         | `(0, 0)`                                    | [`what_rollup`]                          |
//! | per category       | `(0, 1)`                                    | [`category_rollup`] / [`category_total`] |
//! | per severity total | `(1, 1)`                                    | [`severity_total`]                       |

use diesel::prelude::*;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Integer, Text};

/// One report row at a given grain: a `what` (the drill-down report), a category-grain rollup
/// (`what = None`, the "category" report), or the per-severity grand total (`category = ""`,
/// `what = None`). Distinct-task counts are computed by Postgres at each grain (they are not
/// summable from a finer grain). Stored in `report_grain_cache`, populated per scope on demand.
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

/// Maps a drill-down severity key to its `(log table, task-status predicate)` — the scoped-query
/// equivalent of the retired matview's per-severity UNION branch. `warning|error|fatal|invalid`
/// key off the task's worst-message status; `info` is the all-messages dimension over `log_infos`
/// across every completed, non-invalid task (`-5 < status < 0`). Returns `None` for any
/// non-drill-down key, so the report yields no rows — exactly what the matview lookup did for an
/// absent severity. Both returned strings are compile-time constants (never user input), so they
/// are safe to interpolate into the query text below.
fn severity_scope(severity: &str) -> Option<(&'static str, &'static str)> {
  match severity {
    "warning" => Some(("log_warnings", "t.status = -2")),
    "error" => Some(("log_errors", "t.status = -3")),
    "fatal" => Some(("log_fatals", "t.status = -4")),
    "invalid" => Some(("log_invalids", "t.status = -5")),
    // The all-messages dimension: every completed, non-invalid task (NoProblem..Fatal).
    "info" => Some(("log_infos", "t.status < 0 AND t.status > -5")),
    _ => None,
  }
}

/// Retired no-op. The `report_summary` matview it used to rebuild is gone, and the
/// `report_grain_cache` that replaced it is maintained per scope (see [`populate_scope`] /
/// [`invalidate_scope`] / [`invalidate_all`]), never globally. Kept so any lingering call site
/// stays valid and does no work — a report refresh can no longer take a lock or starve conversions.
pub(crate) fn refresh_report_summary(_connection: &mut PgConnection) -> QueryResult<()> { Ok(()) }

/// Always `None`: report freshness is now per-scope (the cache row's `computed_at`), not a single
/// global stamp. Callers branch on `report_uses_rollup` (always false) and render a live timestamp
/// instead of consulting this, so it is no longer reached on the report path.
pub(crate) fn report_summary_refreshed_at(_connection: &mut PgConnection) -> Option<(i64, String)> {
  None
}

/// Whether `report_grain_cache` already holds the given `(corpus, service, severity)` slice. A
/// slice with zero messages produces no `ROLLUP` rows, so this stays `false` for empty severities —
/// their (cheap, no-row) aggregation simply re-runs on each read, which is harmless.
fn scope_cached(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
) -> bool {
  #[derive(QueryableByName)]
  struct Flag {
    #[diesel(sql_type = diesel::sql_types::Bool)]
    hit: bool,
  }
  sql_query(
    "SELECT EXISTS(SELECT 1 FROM report_grain_cache \
       WHERE corpus_id = $1 AND service_id = $2 AND severity = $3) AS hit",
  )
  .bind::<Integer, _>(corpus_id)
  .bind::<Integer, _>(service_id)
  .bind::<Text, _>(severity)
  .get_result::<Flag>(connection)
  .map(|f| f.hit)
  .unwrap_or(false)
}

/// (Re)compute and store the full `ROLLUP(category, what)` grain set for one
/// `(corpus, service, severity)` slice — the matview's per-severity branch, scoped to one corpus.
/// Runs `DELETE` + `INSERT` in one transaction so a concurrent reader sees the old or new slice
/// atomically, never a half-written one. The `ON CONFLICT DO UPDATE` makes a rare cold-miss race
/// (two requests populating the same slice at once) last-writer-wins instead of erroring. No-op for
/// a non-drill-down severity (e.g. `no_problem`/`todo`), matching the matview's absence of those
/// rows.
pub(crate) fn populate_scope(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
) -> QueryResult<()> {
  let Some((table, status_pred)) = severity_scope(severity) else {
    return Ok(());
  };
  connection.transaction::<(), diesel::result::Error, _>(|conn| {
    sql_query(
      "DELETE FROM report_grain_cache \
         WHERE corpus_id = $1 AND service_id = $2 AND severity = $3",
    )
    .bind::<Integer, _>(corpus_id)
    .bind::<Integer, _>(service_id)
    .bind::<Text, _>(severity)
    .execute(conn)?;
    sql_query(format!(
      "INSERT INTO report_grain_cache \
         (corpus_id, service_id, severity, category, what, category_is_total, what_is_total, \
          task_count, message_count) \
       SELECT $1, $2, $3, \
         CASE WHEN GROUPING(COALESCE(l.category, '')) = 1 THEN NULL \
              ELSE COALESCE(l.category, '') END, \
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL \
              ELSE COALESCE(l.what, '') END, \
         GROUPING(COALESCE(l.category, ''))::int, GROUPING(COALESCE(l.what, ''))::int, \
         COUNT(DISTINCT l.task_id)::bigint, COUNT(*)::bigint \
       FROM tasks t JOIN {table} l ON l.task_id = t.id \
       WHERE t.corpus_id = $1 AND t.service_id = $2 AND {status_pred} \
       GROUP BY ROLLUP(COALESCE(l.category, ''), COALESCE(l.what, '')) \
       ON CONFLICT (corpus_id, service_id, severity, category_is_total, what_is_total, category, what) \
         DO UPDATE SET task_count = EXCLUDED.task_count, \
                       message_count = EXCLUDED.message_count, \
                       computed_at = now()"
    ))
    .bind::<Integer, _>(corpus_id)
    .bind::<Integer, _>(service_id)
    .bind::<Text, _>(severity)
    .execute(conn)?;
    Ok(())
  })
}

/// Ensure the slice is cached, populating it on a cold miss. Shared by every reader.
fn ensure_scope(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
) -> QueryResult<()> {
  if !scope_cached(connection, corpus_id, service_id, severity) {
    populate_scope(connection, corpus_id, service_id, severity)?;
  }
  Ok(())
}

/// Drop every cached grain for one `(corpus, service)` scope (all severities). Cheap — a keyed
/// `DELETE`, no scan; the next report view repopulates the slice it needs. Called on the rerun path
/// and (per touched scope) on run-completion.
pub(crate) fn invalidate_scope(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
) -> QueryResult<()> {
  sql_query("DELETE FROM report_grain_cache WHERE corpus_id = $1 AND service_id = $2")
    .bind::<Integer, _>(corpus_id)
    .bind::<Integer, _>(service_id)
    .execute(connection)
    .map(|_| ())
}

/// Drop the entire cache (a cheap `DELETE`, never a scan). Each `(corpus, service, severity)` slice
/// then repopulates lazily, per scope, on its next view. Backs the global "Force refresh" action.
pub(crate) fn invalidate_all(connection: &mut PgConnection) -> QueryResult<()> {
  sql_query("DELETE FROM report_grain_cache")
    .execute(connection)
    .map(|_| ())
}

/// Category-grain report for a `(corpus, service, severity)`: one row per category with its
/// distinct-task and message counts, ordered by descending task count (ties broken by category name
/// for a stable paging order), windowed to `[offset, offset + limit)`. Served from the cache,
/// populated on a cold miss.
pub(crate) fn category_rollup(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
  limit: i64,
  offset: i64,
) -> QueryResult<Vec<ReportSummaryRow>> {
  if severity_scope(severity).is_none() {
    return Ok(Vec::new());
  }
  ensure_scope(connection, corpus_id, service_id, severity)?;
  sql_query(
    "SELECT COALESCE(category, '') AS category, NULL::text AS what, task_count, message_count \
     FROM report_grain_cache \
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
/// tasks and total messages — the denominators the `what` drill-down report needs. `None` when the
/// category has no messages of this severity.
pub(crate) fn category_total(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
  category: &str,
) -> QueryResult<Option<ReportSummaryRow>> {
  if severity_scope(severity).is_none() {
    return Ok(None);
  }
  ensure_scope(connection, corpus_id, service_id, severity)?;
  sql_query(
    "SELECT COALESCE(category, '') AS category, NULL::text AS what, task_count, message_count \
     FROM report_grain_cache \
     WHERE corpus_id = $1 AND service_id = $2 AND severity = $3 \
       AND category_is_total = 0 AND what_is_total = 1 AND COALESCE(category, '') = $4",
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
/// `[offset, offset + limit)`. Served from the cache, populated on a cold miss.
pub(crate) fn what_rollup(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
  category: &str,
  limit: i64,
  offset: i64,
) -> QueryResult<Vec<ReportSummaryRow>> {
  if severity_scope(severity).is_none() {
    return Ok(Vec::new());
  }
  ensure_scope(connection, corpus_id, service_id, severity)?;
  sql_query(
    "SELECT COALESCE(category, '') AS category, COALESCE(what, '') AS what, task_count, message_count \
     FROM report_grain_cache \
     WHERE corpus_id = $1 AND service_id = $2 AND severity = $3 \
       AND category_is_total = 0 AND what_is_total = 0 AND COALESCE(category, '') = $4 \
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
/// logged messages (the `ROLLUP` produces no rows for an empty slice). (`category` comes back as
/// the empty string.)
pub(crate) fn severity_total(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
  severity: &str,
) -> QueryResult<Option<ReportSummaryRow>> {
  if severity_scope(severity).is_none() {
    return Ok(None);
  }
  ensure_scope(connection, corpus_id, service_id, severity)?;
  sql_query(
    "SELECT ''::text AS category, NULL::text AS what, task_count, message_count \
     FROM report_grain_cache \
     WHERE corpus_id = $1 AND service_id = $2 AND severity = $3 AND category_is_total = 1 LIMIT 1",
  )
  .bind::<Integer, _>(corpus_id)
  .bind::<Integer, _>(service_id)
  .bind::<Text, _>(severity)
  .get_result(connection)
  .optional()
}
