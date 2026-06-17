use chrono::NaiveDateTime;
use diesel::dsl::sql;
use diesel::*;
use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

use super::rollup;
use crate::frontend::helpers::severity_highlight;
use crate::helpers::TaskStatus;
use crate::models::{
  Corpus, DiffStatusFilter, DiffStatusRow, HistoricalTask, LogError, LogFatal, LogInfo, LogInvalid,
  LogRecord, LogWarning, Service, Task, TaskRunMetadata,
};
use crate::reports::{AggregateReport, TaskDetailReport};
use crate::schema::tasks;

static TASK_REPORT_NAME_REGEX: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"^.+/(.+)\..+$").unwrap());

/// Maximum messages loaded **per severity** for a single document's forensic view. A hostile or
/// pathological document can carry millions of messages of one severity (observed in production: a
/// single arXiv task with 1.6M warnings); loading them all would allocate gigabytes and hang the
/// request — an unbounded per-request resource acquisition the design principles forbid. The view
/// is a *sample* bounded by this cap; the true per-severity totals are reported separately
/// ([`MessageCounts`]) so the cap is transparent, never silent.
pub const DOCUMENT_MESSAGE_CAP: i64 = 1000;

/// True per-severity message totals for a task (the real counts, **before** the
/// [`DOCUMENT_MESSAGE_CAP`] sampling cap), so a forensic view can show "showing N of M".
#[derive(Debug, Default, Clone, Copy)]
pub struct MessageCounts {
  /// info-level messages
  pub info: i64,
  /// warning-level messages
  pub warning: i64,
  /// error-level messages
  pub error: i64,
  /// fatal-level messages
  pub fatal: i64,
  /// invalid-level messages
  pub invalid: i64,
}
impl MessageCounts {
  /// Grand total across all severities.
  pub fn total(&self) -> i64 { self.info + self.warning + self.error + self.fatal + self.invalid }
}

/// Worker-log messages attached to a `task` — the forensic evidence behind that document's
/// conversion status — loaded through the Diesel-generated row structs (`LogInfo` … `LogInvalid`)
/// via their `belongs_to(Task)` association, so the column mapping is compiler-checked. Each is
/// returned as a [`LogRecord`] trait object (which carries its own
/// `severity()`/`category()`/`what()` /`details()`), in info → invalid order.
///
/// **Bounded:** at most [`DOCUMENT_MESSAGE_CAP`] rows are loaded **per severity** (so the worst
/// case is a few thousand records, never the full millions a pathological document can hold). The
/// second return value is the [`MessageCounts`] of *true* per-severity totals, so a caller can
/// render the real magnitude and flag truncation rather than silently dropping evidence. Every
/// query is keyed by the indexed `task_id` and this serves a single document, never a hot path; a
/// failed sub-query contributes no rows rather than erroring the whole report.
pub fn task_messages(
  connection: &mut PgConnection,
  task: &Task,
) -> (Vec<Box<dyn LogRecord>>, MessageCounts) {
  let mut messages: Vec<Box<dyn LogRecord>> = Vec::new();
  let mut counts = MessageCounts::default();
  macro_rules! collect {
    ($row:ty, $field:ident) => {
      counts.$field = <$row>::belonging_to(task)
        .count()
        .get_result(connection)
        .unwrap_or(0);
      if let Ok(rows) = <$row>::belonging_to(task)
        .limit(DOCUMENT_MESSAGE_CAP)
        .load::<$row>(connection)
      {
        messages.extend(
          rows
            .into_iter()
            .map(|row| Box::new(row) as Box<dyn LogRecord>),
        );
      }
    };
  }
  collect!(LogInfo, info);
  collect!(LogWarning, warning);
  collect!(LogError, error);
  collect!(LogFatal, fatal);
  collect!(LogInvalid, invalid);
  (messages, counts)
}

/// An options object describing a CorTeX report request
#[derive(Debug, Clone)]
pub struct TaskReportOptions<'a> {
  /// Corpus object to report over
  pub corpus: &'a Corpus,
  /// Service object to report over
  pub service: &'a Service,
  /// Optional: severity level for report
  pub severity_opt: Option<String>,
  /// Optional: category name for report
  pub category_opt: Option<String>,
  /// Optional: `what` name for report
  pub what_opt: Option<String>,
  /// Optional: show messages from all severities?
  pub all_messages: bool,
  /// Offset fixed number of messages
  pub offset: i64,
  /// Size limit for report
  pub page_size: i64,
}

pub(crate) fn progress_report(
  connection: &mut PgConnection,
  corpus: i32,
  service: i32,
) -> HashMap<String, f64> {
  use crate::schema::tasks::{corpus_id, service_id, status};
  use diesel::sql_types::BigInt;

  let mut stats_hash: HashMap<String, f64> = HashMap::new();
  for status_key in TaskStatus::keys() {
    stats_hash.insert(status_key, 0.0);
  }
  stats_hash.insert("total".to_string(), 0.0);
  let rows: Vec<(i32, i64)> = tasks::table
    .select((status, sql::<BigInt>("count(*) AS status_count")))
    .filter(service_id.eq(service))
    .filter(corpus_id.eq(corpus))
    .group_by(tasks::status)
    .order(sql::<BigInt>("status_count").desc())
    .load(connection)
    .unwrap_or_default();
  for &(raw_status, count) in &rows {
    let task_status = TaskStatus::from_raw(raw_status);
    let status_key = task_status.to_key();
    {
      let status_frequency = stats_hash.entry(status_key).or_insert(0.0);
      *status_frequency += count as f64;
    }
    if task_status != TaskStatus::Invalid {
      // DIScount invalids from the total numbers
      let total_frequency = stats_hash.entry("total".to_string()).or_insert(0.0);
      *total_frequency += count as f64;
    }
  }
  aux_stats_compute_percentages(&mut stats_hash, None);
  stats_hash
}

/// Computes a `(corpus, service)` progress report at the granularity implied by the optional
/// `severity`/`category`/`what` selectors.
///
/// The aggregate grains — the category report and its `what` drill-down — are served from the
/// `report_summary` rollup (an indexed lookup, refreshed on the run-completion path) rather than
/// the expensive live aggregation in [`task_report_live`]. The per-task drill-downs (`no_problem`
/// and `no_messages` entry lists, the `what`-detail list) and the all-severities (`all_messages`)
/// view are not materialized, so they fall through to the live path. Both paths share
/// [`aux_task_rows_stats`], so the rollup-backed numbers are identical to the live ones (pinned by
/// `tests/report_rollup_test.rs`).
pub(crate) fn task_report(
  connection: &mut PgConnection,
  options: TaskReportOptions,
) -> Vec<HashMap<String, String>> {
  // The gate and the freshness stamp share ONE oracle ([`report_uses_rollup`]) so the footer can
  // never claim matview-freshness for a live-computed report (and vice-versa).
  if report_uses_rollup(
    options.severity_opt.as_deref(),
    options.category_opt.as_deref(),
    options.what_opt.as_deref(),
    options.all_messages,
  ) {
    // The oracle guarantees a rollup severity; the URL severity string IS the matview's `severity`
    // key (`warning`/`error`/`fatal`/`invalid`/`info`). The `if let` + fall-through keeps this
    // panic-free on the request path regardless.
    if let Some(severity) = options.severity_opt.clone() {
      match (options.category_opt.as_deref(), options.what_opt.as_deref()) {
        // Category report: one row per category, plus the severity totals.
        (None, None) => {
          let severity_tasks =
            severity_task_count(connection, options.corpus, options.service, &severity);
          return category_grain_from_rollup(
            connection,
            options.corpus,
            options.service,
            &severity,
            severity_tasks,
            options.page_size,
            options.offset,
          );
        },
        // `what` drill-down within a category.
        (Some(category), None) => {
          return what_grain_from_rollup(
            connection,
            options.corpus,
            options.service,
            &severity,
            category,
            options.page_size,
            options.offset,
          );
        },
        _ => {},
      }
    }
  }
  task_report_live(connection, options)
}

/// Whether a report request is served from the **`report_summary` rollup** (a fast, matview-backed
/// indexed lookup) rather than the live `log_*` aggregation in [`task_report_live`]. The matview
/// covers only the category and `what`-drill-down **aggregate grains** of the five rollup
/// severities (`warning`/`error`/`fatal`/`invalid`/`info`); the top-level overview
/// (`progress_report`, a live `tasks` count), the all-severities `all=true` view, the `no_messages`
/// row, and every per-task entry list are computed live.
///
/// This is the single source of truth for both the serving branch ([`task_report`]) **and** the
/// report-freshness footer: the "data refreshed …" matview timestamp must be shown **iff** this is
/// `true` — otherwise the data is live ("just now"), and stamping it with the matview's age lies
/// about freshness (the bug fixed alongside this oracle).
pub fn report_uses_rollup(
  severity_opt: Option<&str>,
  category_opt: Option<&str>,
  what_opt: Option<&str>,
  all_messages: bool,
) -> bool {
  // Reports are now computed LIVE and per-(corpus,service)-scoped from the indexed task/log tables
  // (the `task_report_live` path). The global `report_summary` matview was retired: it recomputed
  // all corpora × five log tables for any single-corpus change (a ~99 GB / 345M-row scan to reflect
  // one run's worth of logs), and that refresh starved the conversion finalize path under load,
  // stalling the worker fleet. A scoped live drill-down touches one log table for one
  // (corpus, service) via the existing per-status indexes, so it is cheap to regenerate on demand.
  // Always take the live path.
  let _ = (severity_opt, category_opt, what_opt, all_messages);
  false
}

/// Total tasks counted toward percentage denominators: all tasks for the pair minus `Invalid` ones
/// (which were never processed, so they would dilute the service percentages).
fn total_valid_task_count(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
) -> i64 {
  use crate::schema::tasks::dsl::{corpus_id, service_id, status};
  let total: i64 = tasks::table
    .filter(service_id.eq(service.id))
    .filter(corpus_id.eq(corpus.id))
    .count()
    .get_result(connection)
    .unwrap_or(0);
  let invalid: i64 = tasks::table
    .filter(service_id.eq(service.id))
    .filter(corpus_id.eq(corpus.id))
    .filter(status.eq(TaskStatus::Invalid.raw()))
    .count()
    .get_result(connection)
    .unwrap_or(0);
  total - invalid
}

/// Counts the tasks of a `(corpus, service)` pair currently in a given raw status.
fn count_in_status(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  raw_status: i32,
) -> i64 {
  use crate::schema::tasks::dsl::{corpus_id, service_id, status};
  tasks::table
    .filter(service_id.eq(service.id))
    .filter(corpus_id.eq(corpus.id))
    .filter(status.eq(raw_status))
    .count()
    .get_result(connection)
    .unwrap_or(0)
}

/// The denominator a rollup severity's report is computed against, matching the live path: the four
/// message severities count tasks **in that status**; `info` is the all-messages dimension, so its
/// denominator is the **full task count** (the `all_messages` branch of [`task_report_live`] counts
/// every task of the pair, invalids included).
fn severity_task_count(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  severity: &str,
) -> i64 {
  let raw_status = match severity {
    "warning" => TaskStatus::Warning.raw(),
    "error" => TaskStatus::Error.raw(),
    "fatal" => TaskStatus::Fatal.raw(),
    "invalid" => TaskStatus::Invalid.raw(),
    // `info` aggregates across all tasks, not one status.
    _ => return total_task_count(connection, corpus, service),
  };
  count_in_status(connection, corpus, service, raw_status)
}

/// Total tasks of a `(corpus, service)` pair (all statuses, invalids included) — the `info`
/// report's denominator.
fn total_task_count(connection: &mut PgConnection, corpus: &Corpus, service: &Service) -> i64 {
  use crate::schema::tasks::dsl::{corpus_id, service_id};
  tasks::table
    .filter(service_id.eq(service.id))
    .filter(corpus_id.eq(corpus.id))
    .count()
    .get_result(connection)
    .unwrap_or(0)
}

/// Category report for a severity, assembled from the rollup: one row per category (distinct tasks
/// + messages), a `no_messages` row for tasks that completed silently, and the severity total.
fn category_grain_from_rollup(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  severity: &str,
  severity_tasks: i64,
  limit: i64,
  offset: i64,
) -> Vec<HashMap<String, String>> {
  let category_rows =
    rollup::category_rollup(connection, corpus.id, service.id, severity, limit, offset)
      .unwrap_or_default();
  let grand_total =
    rollup::severity_total(connection, corpus.id, service.id, severity).unwrap_or_default();
  let total_valid_count = total_valid_task_count(connection, corpus, service);
  // Tasks that carry at least one message of this severity, and the total message count.
  let logged_task_count = grand_total.as_ref().map_or(0, |g| g.task_count);
  let logged_message_count = grand_total.as_ref().map_or(0, |g| g.message_count);
  let silent_task_count = if logged_task_count >= severity_tasks {
    None
  } else {
    Some(severity_tasks - logged_task_count)
  };
  let report_rows = rows_to_aggregates(category_rows, |row| row.category);
  aux_task_rows_stats(
    &report_rows,
    total_valid_count,
    severity_tasks,
    logged_message_count,
    silent_task_count,
  )
}

/// `what` drill-down within a category, assembled from the rollup: one row per `what`, with the
/// owning category's totals as the denominators.
fn what_grain_from_rollup(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  severity: &str,
  category: &str,
  limit: i64,
  offset: i64,
) -> Vec<HashMap<String, String>> {
  let what_rows = rollup::what_rollup(
    connection, corpus.id, service.id, severity, category, limit, offset,
  )
  .unwrap_or_default();
  let category_total =
    rollup::category_total(connection, corpus.id, service.id, severity, category)
      .unwrap_or_default();
  let total_valid_count = total_valid_task_count(connection, corpus, service);
  let (category_tasks, category_messages) = category_total
    .as_ref()
    .map_or((0, 0), |c| (c.task_count, c.message_count));
  let report_rows = rows_to_aggregates(what_rows, |row| row.what.unwrap_or_default());
  aux_task_rows_stats(
    &report_rows,
    total_valid_count,
    category_tasks,
    category_messages,
    None,
  )
}

/// Adapts rollup rows into the `AggregateReport` shape `aux_task_rows_stats` consumes, naming each
/// row via `name_of` (the category, or the `what`).
fn rows_to_aggregates(
  rows: Vec<rollup::ReportSummaryRow>,
  name_of: impl Fn(rollup::ReportSummaryRow) -> String,
) -> Vec<AggregateReport> {
  rows
    .into_iter()
    .map(|row| {
      let task_count = row.task_count;
      let message_count = row.message_count;
      AggregateReport {
        report_name: Some(name_of(row)),
        task_count,
        message_count,
      }
    })
    .collect()
}

/// Live (non-materialized) computation of a task report, used for the per-task drill-down grains
/// and the all-severities view, and as the equivalence reference for the rollup-backed aggregate
/// grains.
pub(crate) fn task_report_live(
  connection: &mut PgConnection,
  options: TaskReportOptions,
) -> Vec<HashMap<String, String>> {
  use crate::schema::tasks::dsl::{corpus_id, service_id, status};
  use diesel::sql_types::{BigInt, Text};
  // destructure options
  let TaskReportOptions {
    corpus,
    service,
    severity_opt,
    category_opt,
    what_opt,
    mut all_messages,
    offset,
    page_size,
  } = options;
  // The final report, populated based on the specific selectors
  let mut report = Vec::new();

  if let Some(severity_name) = severity_opt {
    let task_status = TaskStatus::from_key(&severity_name);
    // NoProblem report is a bit special, as it provides a simple list of entries - we assume no
    // logs of notability for this severity.
    if task_status == Some(TaskStatus::NoProblem) {
      let entry_rows: Vec<(String, i64)> = tasks::table
        .select((tasks::entry, tasks::id))
        .filter(service_id.eq(service.id))
        .filter(corpus_id.eq(corpus.id))
        // In this branch `task_status == Some(NoProblem)`; use the constant directly so the report
        // path carries no `.unwrap()` (structurally panic-free, not merely provably-so today).
        .filter(status.eq(TaskStatus::NoProblem.raw()))
        .order(tasks::entry.asc())
        .offset(offset)
        .limit(page_size)
        .load(connection)
        .unwrap_or_default();
      for &(ref entry_fixedwidth, entry_taskid) in &entry_rows {
        let mut entry_map = HashMap::new();
        let entry_trimmed = entry_fixedwidth.trim_end().to_string();
        let entry_name = TASK_REPORT_NAME_REGEX
          .replace(&entry_trimmed, "$1")
          .to_string();

        entry_map.insert("entry".to_string(), entry_trimmed);
        entry_map.insert("entry_name".to_string(), entry_name);
        entry_map.insert("entry_taskid".to_string(), entry_taskid.to_string());
        entry_map.insert("details".to_string(), "OK".to_string());
        report.push(entry_map);
      }
    } else {
      // The "total tasks" used in the divison denominators for computing the percentage
      // distributions are all valid tasks (total - invalid), as we don't want to dilute
      // the service percentage with jobs that were never processed. For now the fastest
      // way to obtain that number is using 2 queries for each and subtracting the numbers in Rust
      // Degrade to 0 on a query error rather than panicking the report request (principle #2,
      // matching the `unwrap_or_default` siblings on this path); the percentage math clamps the
      // denominator to ≥1, so a degraded total yields 0% instead of a div-by-zero.
      let total_count: i64 = tasks::table
        .filter(service_id.eq(service.id))
        .filter(corpus_id.eq(corpus.id))
        .count()
        .get_result(connection)
        .unwrap_or(0);
      let invalid_count: i64 = tasks::table
        .filter(service_id.eq(service.id))
        .filter(corpus_id.eq(corpus.id))
        .filter(status.eq(TaskStatus::Invalid.raw()))
        .count()
        .get_result(connection)
        .unwrap_or(0);
      let total_valid_count = (total_count - invalid_count).max(0);

      let log_table = match task_status {
        Some(ref ts) => ts.to_table(),
        None => {
          all_messages = true;
          "log_infos".to_string()
        },
      };

      let task_status_raw = task_status.unwrap_or(TaskStatus::NoProblem).raw();
      let (status_clause, bind_status) = if !all_messages {
        (String::from("status=$3 "), task_status_raw)
      } else {
        (
          String::from("status < $3 and status > ") + &TaskStatus::Invalid.raw().to_string(),
          0,
        ) // all completed tasks are negative integers, so 0 is a safe upper bound
      };
      match category_opt {
        None => {
          // Bad news, query is close to line noise
          // Good news, we avoid the boilerplate of dispatching to 4 distinct log tables for now
          let category_report_string =
            "SELECT category as report_name, count(*) as task_count, COALESCE(SUM(total_counts::integer),0) as message_count FROM (".to_string()+
              "SELECT "+&log_table+".category, "+&log_table+".task_id, count(*) as total_counts FROM "+
                "tasks LEFT OUTER JOIN "+&log_table+" ON (tasks.id="+&log_table+".task_id) WHERE service_id=$1 and corpus_id=$2 and "+ &status_clause +
                  " GROUP BY "+&log_table+".category, "+&log_table+".task_id) as tmp "+
            "GROUP BY category ORDER BY task_count desc";
          let category_report_query = sql_query(category_report_string);
          let category_report_rows: Vec<AggregateReport> = category_report_query
            .bind::<BigInt, i64>(i64::from(service.id))
            .bind::<BigInt, i64>(i64::from(corpus.id))
            .bind::<BigInt, i64>(i64::from(bind_status))
            .load(connection)
            .unwrap_or_default();

          // How many tasks total in this severity-status?
          let severity_tasks: i64 = if !all_messages {
            tasks::table
              .filter(service_id.eq(service.id))
              .filter(corpus_id.eq(corpus.id))
              .filter(status.eq(task_status_raw))
              .count()
              .get_result(connection)
              .unwrap_or(-1)
          } else {
            tasks::table
              .filter(service_id.eq(service.id))
              .filter(corpus_id.eq(corpus.id))
              .count()
              .get_result(connection)
              .unwrap_or(-1)
          };
          let status_report_query_string =
          "SELECT NULL as report_name, count(*) as task_count, COALESCE(SUM(inner_message_count::integer),0) as message_count FROM ( ".to_string()+
            "SELECT tasks.id, count(*) as inner_message_count FROM "+
            "tasks, "+&log_table+" where tasks.id="+&log_table+".task_id and "+
            "service_id=$1 and corpus_id=$2 and "+&status_clause+" group by tasks.id) as tmp";
          let status_report_query = sql_query(status_report_query_string)
            .bind::<BigInt, i64>(i64::from(service.id))
            .bind::<BigInt, i64>(i64::from(corpus.id))
            .bind::<BigInt, i64>(i64::from(bind_status));
          let status_report_rows: AggregateReport = status_report_query
            .get_result(connection)
            .unwrap_or_default();

          let logged_task_count: i64 = status_report_rows.task_count;
          let logged_message_count: i64 = status_report_rows.message_count;
          let silent_task_count = if logged_task_count >= severity_tasks {
            None
          } else {
            Some(severity_tasks - logged_task_count)
          };
          report = aux_task_rows_stats(
            &category_report_rows,
            total_valid_count,
            severity_tasks,
            logged_message_count,
            silent_task_count,
          )
        },
        Some(category_name) => {
          if category_name == "no_messages" {
            let no_messages_query_string = "SELECT * FROM tasks t WHERE ".to_string()
              + "service_id=$1 and corpus_id=$2 and "
              + &status_clause
              + " and "
              + "NOT EXISTS (SELECT null FROM "
              + &log_table
              + " where "
              + &log_table
              + ".task_id=t.id) limit 100";
            let no_messages_query = sql_query(no_messages_query_string)
              .bind::<BigInt, i64>(i64::from(service.id))
              .bind::<BigInt, i64>(i64::from(corpus.id))
              .bind::<BigInt, i64>(i64::from(bind_status))
              .bind::<BigInt, i64>(i64::from(task_status_raw));
            let no_message_tasks: Vec<Task> = no_messages_query
              .get_results(connection)
              .unwrap_or_default();

            for task in &no_message_tasks {
              let mut entry_map = HashMap::new();
              let entry = task.entry.trim_end().to_string();
              let entry_name = TASK_REPORT_NAME_REGEX.replace(&entry, "$1").to_string();

              entry_map.insert("entry".to_string(), entry);
              entry_map.insert("entry_name".to_string(), entry_name);
              entry_map.insert("entry_taskid".to_string(), task.id.to_string());
              entry_map.insert("details".to_string(), "OK".to_string());
              report.push(entry_map);
            }
          } else {
            match what_opt {
              None => {
                let what_report_query_string =
            "SELECT what as report_name, count(*) as task_count, COALESCE(SUM(total_counts::integer),0) as message_count FROM ( ".to_string() +
              "SELECT "+&log_table+".what, "+&log_table+".task_id, count(*) as total_counts FROM "+
                "tasks LEFT OUTER JOIN "+&log_table+" ON (tasks.id="+&log_table+".task_id) "+
                "WHERE service_id=$1 and corpus_id=$2 and "+&status_clause+" and category=$4 "+
                "GROUP BY "+&log_table+".what, "+&log_table+".task_id) as tmp GROUP BY what ORDER BY task_count desc";
                let what_report_query = sql_query(what_report_query_string)
                  .bind::<BigInt, i64>(i64::from(service.id))
                  .bind::<BigInt, i64>(i64::from(corpus.id))
                  .bind::<BigInt, i64>(i64::from(bind_status))
                  .bind::<Text, _>(category_name.clone());
                let what_report: Vec<AggregateReport> = what_report_query
                  .get_results(connection)
                  .unwrap_or_default();
                // How many tasks and messages total in this category?
                let this_category_report_query_string = "SELECT NULL as report_name, count(*) as task_count, COALESCE(SUM(inner_message_count::integer),0) as message_count FROM".to_string() +
              " (SELECT tasks.id, count(*) as inner_message_count "+
              "FROM tasks, "+&log_table+" WHERE tasks.id="+&log_table+".task_id and "+
                "service_id=$1 and corpus_id=$2 and "+&status_clause+" and category=$4 group by tasks.id) as tmp";
                let this_category_report_query = sql_query(this_category_report_query_string)
                  .bind::<BigInt, i64>(i64::from(service.id))
                  .bind::<BigInt, i64>(i64::from(corpus.id))
                  .bind::<BigInt, i64>(i64::from(bind_status))
                  .bind::<Text, _>(category_name);
                let this_category_report: AggregateReport = this_category_report_query
                  .get_result(connection)
                  .unwrap_or_default();

                report = aux_task_rows_stats(
                  &what_report,
                  total_valid_count,
                  this_category_report.task_count,
                  this_category_report.message_count,
                  None,
                )
              },
              Some(what_name) => {
                let details_report_query_string = "SELECT tasks.id, tasks.entry, ".to_string()
                  + &log_table
                  + ".details from tasks, "
                  + &log_table
                  + " WHERE tasks.id="
                  + &log_table
                  + ".task_id and service_id=$1 and corpus_id=$2 and "
                  + &status_clause
                  + " and category=$4 and what=$5 ORDER BY tasks.entry ASC offset $6 limit $7";

                let details_report_query = sql_query(details_report_query_string)
                  .bind::<BigInt, i64>(i64::from(service.id))
                  .bind::<BigInt, i64>(i64::from(corpus.id))
                  .bind::<BigInt, i64>(i64::from(bind_status))
                  .bind::<Text, _>(category_name)
                  .bind::<Text, _>(what_name)
                  .bind::<BigInt, i64>(offset)
                  .bind::<BigInt, i64>(page_size);
                let details_report: Vec<TaskDetailReport> = details_report_query
                  .get_results(connection)
                  .unwrap_or_default();
                for details_row in details_report {
                  let mut entry_map = HashMap::new();
                  let entry = details_row.entry.trim_end().to_string();
                  let entry_name = TASK_REPORT_NAME_REGEX.replace(&entry, "$1").to_string();
                  // TODO: Also use url-escape
                  entry_map.insert("entry".to_string(), entry);
                  entry_map.insert("entry_name".to_string(), entry_name);
                  entry_map.insert("entry_taskid".to_string(), details_row.id.to_string());
                  entry_map.insert("details".to_string(), details_row.details);
                  report.push(entry_map);
                }
              },
            }
          }
        },
      }
    }
  }
  report
}

fn aux_stats_compute_percentages(stats_hash: &mut HashMap<String, f64>, total_given: Option<f64>) {
  // Compute percentages, now that we have a total
  let total: f64 = 1.0_f64.max(match total_given {
    // Defensive: fall back to 0.0 (then clamped to 1.0 below) if a caller omitted the "total" key,
    // rather than `.unwrap()`-panicking on this report path.
    None => stats_hash.get("total").copied().unwrap_or(0.0),
    Some(total_num) => total_num,
  });
  let stats_keys = stats_hash.keys().cloned().collect::<Vec<_>>();
  for stats_key in stats_keys {
    {
      let key_percent_value: f64 = 100.0 * (*stats_hash.get_mut(&stats_key).unwrap() / total);
      let key_percent_rounded: f64 = (key_percent_value * 100.0).round() / 100.0;
      let key_percent_name = stats_key + "_percent";
      stats_hash.insert(key_percent_name, key_percent_rounded);
    }
  }
}

fn aux_task_rows_stats(
  report_rows: &[AggregateReport],
  mut total_valid_tasks: i64,
  these_tasks: i64,
  mut these_messages: i64,
  these_silent: Option<i64>,
) -> Vec<HashMap<String, String>> {
  let mut report = Vec::new();
  // Guard against dividing by 0
  if total_valid_tasks <= 0 {
    total_valid_tasks = 1;
  }
  if these_messages <= 0 {
    these_messages = 1;
  }

  for row in report_rows {
    let stat_type: String = match row.report_name {
      Some(ref name) => name.trim_end().to_string(),
      None => String::new(),
    };
    if stat_type.is_empty() {
      continue;
    } // skip, empty
    let stat_tasks: i64 = row.task_count;
    let stat_messages: i64 = row.message_count;
    let mut stats_hash: HashMap<String, String> = HashMap::new();
    stats_hash.insert("name".to_string(), stat_type);
    stats_hash.insert("tasks".to_string(), stat_tasks.to_string());
    stats_hash.insert("messages".to_string(), stat_messages.to_string());

    let tasks_percent_value: f64 = 100.0 * (stat_tasks as f64 / total_valid_tasks as f64);
    let tasks_percent_rounded: f64 = (tasks_percent_value * 100.0).round() / 100.0;
    stats_hash.insert(
      "tasks_percent".to_string(),
      tasks_percent_rounded.to_string(),
    );
    let messages_percent_value: f64 = 100.0 * (stat_messages as f64 / these_messages as f64);
    let messages_percent_rounded: f64 = (messages_percent_value * 100.0).round() / 100.0;
    stats_hash.insert(
      "messages_percent".to_string(),
      messages_percent_rounded.to_string(),
    );

    report.push(stats_hash);
  }

  let these_tasks_percent_value: f64 = 100.0 * (these_tasks as f64 / total_valid_tasks as f64);
  let these_tasks_percent_rounded: f64 = (these_tasks_percent_value * 100.0).round() / 100.0;
  // Append the total to the end of the report:
  let mut total_hash = HashMap::new();
  total_hash.insert("name".to_string(), "total".to_string());
  match these_silent {
    None => {},
    Some(silent_count) => {
      let mut no_messages_hash: HashMap<String, String> = HashMap::new();
      no_messages_hash.insert("name".to_string(), "no_messages".to_string());
      no_messages_hash.insert("tasks".to_string(), silent_count.to_string());
      let silent_tasks_percent_value: f64 =
        100.0 * (silent_count as f64 / total_valid_tasks as f64);
      let silent_tasks_percent_rounded: f64 = (silent_tasks_percent_value * 100.0).round() / 100.0;
      no_messages_hash.insert(
        "tasks_percent".to_string(),
        silent_tasks_percent_rounded.to_string(),
      );
      no_messages_hash.insert("messages".to_string(), "0".to_string());
      no_messages_hash.insert("messages_percent".to_string(), "0".to_string());
      report.push(no_messages_hash);
    },
  };
  total_hash.insert("tasks".to_string(), these_tasks.to_string());
  total_hash.insert(
    "tasks_percent".to_string(),
    these_tasks_percent_rounded.to_string(),
  );
  total_hash.insert("messages".to_string(), these_messages.to_string());
  total_hash.insert("messages_percent".to_string(), "100".to_string());
  report.push(total_hash);
  report
}

pub(crate) fn list_tasks(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  task_status: &TaskStatus,
) -> Vec<Task> {
  use crate::schema::tasks::dsl::{corpus_id, service_id, status};
  tasks::table
    .filter(service_id.eq(service.id))
    .filter(corpus_id.eq(corpus.id))
    .filter(status.eq(task_status.raw()))
    .load(connection)
    .unwrap_or_default()
}

pub(crate) fn list_entries(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  task_status: &TaskStatus,
) -> Vec<String> {
  list_tasks(connection, corpus, service, task_status)
    .into_iter()
    .map(|task| {
      if service.name == "import" {
        task.entry.trim_end().to_string()
      } else {
        crate::helpers::result_archive_path(&task.entry, &service.name, corpus.sandbox_id())
          .map(|p| p.to_string_lossy().into_owned())
          .unwrap_or_default()
      }
    })
    .collect()
}

/// Prepares a template-friendly list of task differences
pub fn list_task_diffs(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  filters: DiffStatusFilter,
) -> Vec<TaskRunMetadata> {
  match HistoricalTask::report_for(corpus, service, Some(filters), connection) {
    Ok((_dates, report)) => report
      .into_iter()
      .map(|row| {
        let previous_status = TaskStatus::from_raw(row.0.status).to_key();
        let current_status = TaskStatus::from_raw(row.1.status).to_key();
        let previous_highlight = severity_highlight(&previous_status).to_owned();
        let current_highlight = severity_highlight(&current_status).to_owned();
        TaskRunMetadata {
          task_id: row.0.task_id.to_string(),
          entry: TASK_REPORT_NAME_REGEX
            .replace(&row.0.entry, "$1")
            .to_string(),
          previous_status,
          current_status,
          previous_highlight,
          current_highlight,
          previous_saved_at: row.0.saved_at.format("%Y-%m-%d").to_string(),
          current_saved_at: row.1.saved_at.format("%Y-%m-%d").to_string(),
        }
      })
      .collect(),
    _ => Vec::new(),
  }
}

/// Prepares a template-friendly summary of task differences
pub fn summary_task_diffs(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  previous_date: Option<NaiveDateTime>,
  current_date: Option<NaiveDateTime>,
) -> (Vec<String>, Vec<DiffStatusRow>) {
  // Aggregate the (previous → current) status transitions **in SQL** (one row per status pair),
  // never loading the per-task snapshots into the application — a corpus with two
  // multi-million-task snapshots is summarized in constant application memory (KNOWN_ISSUES R-8).
  // Degrades to an empty matrix (→ all-zero table) on any DB error, like the rest of this report
  // path.
  let (dates, matrix) =
    HistoricalTask::status_change_matrix(corpus, service, previous_date, current_date, connection)
      .unwrap_or_default();
  let mut summary: HashMap<i32, HashMap<i32, i64>> = HashMap::new();
  for (prev_status, current_status, count) in matrix {
    summary
      .entry(prev_status)
      .or_default()
      .insert(current_status, count);
  }
  // Here we are only interested to report on the 4 "completed" severities; we could add invalid,
  // but not yet a focus.
  use TaskStatus::*;
  let mut tabular = Vec::new();
  for prev in [NoProblem, Warning, Error, Fatal].iter() {
    for current in [NoProblem, Warning, Error, Fatal].iter() {
      let previous_status = prev.to_key();
      let current_status = current.to_key();
      let previous_highlight = severity_highlight(&previous_status).to_owned();
      let current_highlight = severity_highlight(&current_status).to_owned();
      let task_count = summary
        .get(&prev.raw())
        .and_then(|row| row.get(&current.raw()))
        .copied()
        .unwrap_or(0) as usize;
      tabular.push(DiffStatusRow {
        previous_status,
        current_status,
        previous_highlight,
        current_highlight,
        task_count,
      });
    }
  }
  (dates, tabular)
}

#[cfg(test)]
mod rollup_equivalence_tests {
  //! Pins behavioral equivalence: the rollup-backed [`task_report`] must return exactly what the
  //! live aggregation ([`task_report_live`]) returns for the category and `what` grains it now
  //! serves — so wiring reports to the materialized view changed performance, not numbers.
  use super::{TaskReportOptions, rollup, task_report, task_report_live};
  use crate::backend;
  use crate::helpers::TaskStatus;
  use crate::models::{Corpus, NewCorpus, NewService, Service};
  use crate::schema::{corpora, log_errors, log_infos, log_warnings, services, tasks};
  use diesel::prelude::*;
  use std::collections::HashMap;

  const CORPUS_NAME: &str = "rollup-equivalence corpus";
  const SERVICE_NAME: &str = "rollup_equiv_svc";

  fn add_task(conn: &mut PgConnection, entry: &str, service: i32, corpus: i32, status: i32) -> i64 {
    diesel::insert_into(tasks::table)
      .values((
        tasks::entry.eq(entry),
        tasks::service_id.eq(service),
        tasks::corpus_id.eq(corpus),
        tasks::status.eq(status),
      ))
      .returning(tasks::id)
      .get_result(conn)
      .expect("insert task")
  }

  fn add_warning(conn: &mut PgConnection, task_id: i64, category: &str, what: &str) {
    diesel::insert_into(log_warnings::table)
      .values((
        log_warnings::task_id.eq(task_id),
        log_warnings::category.eq(category),
        log_warnings::what.eq(what),
        log_warnings::details.eq(""),
      ))
      .execute(conn)
      .expect("insert log_warning");
  }

  fn add_error(conn: &mut PgConnection, task_id: i64, category: &str, what: &str) {
    diesel::insert_into(log_errors::table)
      .values((
        log_errors::task_id.eq(task_id),
        log_errors::category.eq(category),
        log_errors::what.eq(what),
        log_errors::details.eq(""),
      ))
      .execute(conn)
      .expect("insert log_error");
  }

  fn add_info(conn: &mut PgConnection, task_id: i64, category: &str, what: &str) {
    diesel::insert_into(log_infos::table)
      .values((
        log_infos::task_id.eq(task_id),
        log_infos::category.eq(category),
        log_infos::what.eq(what),
        log_infos::details.eq(""),
      ))
      .execute(conn)
      .expect("insert log_info");
  }

  /// Index report rows by their `name` so comparisons are order-independent.
  fn by_name(rows: Vec<HashMap<String, String>>) -> HashMap<String, HashMap<String, String>> {
    rows
      .into_iter()
      .map(|row| (row.get("name").cloned().unwrap_or_default(), row))
      .collect()
  }

  fn options_paged<'a>(
    corpus: &'a Corpus,
    service: &'a Service,
    severity: &str,
    category: Option<&str>,
    page_size: i64,
    offset: i64,
  ) -> TaskReportOptions<'a> {
    TaskReportOptions {
      corpus,
      service,
      severity_opt: Some(severity.to_string()),
      category_opt: category.map(str::to_string),
      what_opt: None,
      all_messages: false,
      offset,
      page_size,
    }
  }

  fn options<'a>(
    corpus: &'a Corpus,
    service: &'a Service,
    severity: &str,
    category: Option<&str>,
  ) -> TaskReportOptions<'a> {
    options_paged(corpus, service, severity, category, 100, 0)
  }

  #[test]
  fn live_report_grains_are_correct() {
    let mut backend = backend::testdb();

    // --- Clean slate -----------------------------------------------------------------------------
    if let Ok(existing) = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection) {
      let ids: Vec<i64> = tasks::table
        .filter(tasks::corpus_id.eq(existing.id))
        .select(tasks::id)
        .load(&mut backend.connection)
        .unwrap_or_default();
      diesel::delete(log_warnings::table.filter(log_warnings::task_id.eq_any(&ids)))
        .execute(&mut backend.connection)
        .ok();
      diesel::delete(log_errors::table.filter(log_errors::task_id.eq_any(&ids)))
        .execute(&mut backend.connection)
        .ok();
      diesel::delete(tasks::table.filter(tasks::corpus_id.eq(existing.id)))
        .execute(&mut backend.connection)
        .ok();
      diesel::delete(corpora::table.filter(corpora::id.eq(existing.id)))
        .execute(&mut backend.connection)
        .ok();
    }
    diesel::delete(services::table.filter(services::name.eq(SERVICE_NAME)))
      .execute(&mut backend.connection)
      .ok();

    // --- Seed corpus + service -------------------------------------------------------------------
    backend
      .add(&NewCorpus {
        name: CORPUS_NAME.to_string(),
        path: "/tmp/rollup-equivalence".to_string(),
        complex: true,
        description: String::new(),
      })
      .expect("add corpus");
    let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("corpus");
    backend
      .add(&NewService {
        name: SERVICE_NAME.to_string(),
        version: 0.1,
        inputformat: "tex".to_string(),
        outputformat: "html".to_string(),
        inputconverter: Some("import".to_string()),
        complex: true,
        description: String::from("rollup equivalence service"),
      })
      .expect("add service");
    let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");

    let warning = TaskStatus::Warning.raw();
    let error = TaskStatus::Error.raw();
    let conn = &mut backend.connection;

    // Warnings: math{ux,uy} + math{ux} + font{missing}, plus one silent warning task (no logs),
    // which must surface as a `no_messages` row of 1.
    let a = add_task(conn, "/eq/a", service.id, corpus.id, warning);
    let b = add_task(conn, "/eq/b", service.id, corpus.id, warning);
    let c = add_task(conn, "/eq/c", service.id, corpus.id, warning);
    let _silent = add_task(conn, "/eq/d", service.id, corpus.id, warning);
    add_warning(conn, a, "math", "undefined_x");
    add_warning(conn, a, "math", "undefined_y");
    add_warning(conn, b, "math", "undefined_x");
    add_warning(conn, c, "font", "missing");

    // Errors: tex{err1} + tex{err1,err2}.
    let e = add_task(conn, "/eq/e", service.id, corpus.id, error);
    let f = add_task(conn, "/eq/f", service.id, corpus.id, error);
    add_error(conn, e, "tex", "err1");
    add_error(conn, f, "tex", "err1");
    add_error(conn, f, "tex", "err2");

    // Info: the all-messages dimension — `log_infos` over ALL completed tasks regardless of status,
    // so attach info messages to a warning task (a), an error task (e) and a no-problem task (g).
    // /info was the report that scanned live instead of using the matview's `info` branch.
    let g = add_task(
      conn,
      "/eq/g",
      service.id,
      corpus.id,
      TaskStatus::NoProblem.raw(),
    );
    add_info(conn, a, "load", "package");
    add_info(conn, e, "load", "package");
    add_info(conn, g, "load", "class");

    // The category/what/severity-total readers now compute live + per-(corpus,service)-scoped (the
    // global report_summary matview was retired). Pin them directly against the seeded data.
    let warning_cats = rollup::category_rollup(conn, corpus.id, service.id, "warning", 100, 0)
      .expect("category_rollup");
    assert_eq!(warning_cats.len(), 2, "warning categories: math, font");
    assert_eq!(warning_cats[0].category, "math", "busiest category first");
    assert_eq!(warning_cats[0].task_count, 2, "math distinct tasks A,B");
    assert_eq!(
      warning_cats[0].message_count, 3,
      "math messages: 2 (A) + 1 (B)"
    );
    assert_eq!(warning_cats[1].category, "font");
    assert_eq!(warning_cats[1].task_count, 1);
    let math_whats = rollup::what_rollup(conn, corpus.id, service.id, "warning", "math", 100, 0)
      .expect("what_rollup");
    assert_eq!(math_whats.len(), 2, "math whats: undefined_x, undefined_y");
    let warning_total = rollup::severity_total(conn, corpus.id, service.id, "warning")
      .expect("severity_total query")
      .expect("warning has messages");
    assert_eq!(
      warning_total.task_count, 3,
      "distinct warning-logged tasks A,B,C (silent D has no logs)"
    );
    assert_eq!(warning_total.message_count, 4, "total warning messages");
    assert!(
      rollup::severity_total(conn, corpus.id, service.id, "fatal")
        .expect("severity_total query")
        .is_none(),
      "a severity with no messages yields None"
    );

    // --- The dispatcher routes every grain to the live path; cross-check task_report == live -----
    let cases = [
      ("warning", None),
      ("warning", Some("math")),
      ("warning", Some("font")),
      ("error", None),
      ("error", Some("tex")),
      // `info` — the all-messages dimension; must come from the matview's info branch, not a live
      // log_infos scan.
      ("info", None),
      ("info", Some("load")),
    ];
    for (severity, category) in cases {
      let fast = by_name(task_report(
        conn,
        options(&corpus, &service, severity, category),
      ));
      let live = by_name(task_report_live(
        conn,
        options(&corpus, &service, severity, category),
      ));
      assert_eq!(
        fast, live,
        "task_report (live) vs task_report_live mismatch for severity={severity} category={category:?}"
      );
      assert!(
        !fast.is_empty(),
        "live path produced an empty report for severity={severity} category={category:?}"
      );
    }

    // --- Spot-check absolute values, so equivalence can't pass by both paths being wrong
    // ----------
    let warning_cat = by_name(task_report(
      conn,
      options(&corpus, &service, "warning", None),
    ));
    assert_eq!(
      warning_cat["math"]["tasks"], "2",
      "math: distinct tasks A,B"
    );
    assert_eq!(warning_cat["math"]["messages"], "3", "math: 2 (A) + 1 (B)");
    assert_eq!(warning_cat["font"]["tasks"], "1");
    assert_eq!(
      warning_cat["no_messages"]["tasks"], "1",
      "one silent task D"
    );
    assert_eq!(warning_cat["total"]["tasks"], "4", "A,B,C,D");

    // --- Guard: a severity with no rollup rows degrades gracefully (no panic; a zeroed total row),
    //     and the rollup path still matches the live path on that empty case ----------------------
    let fatal_fast = by_name(task_report(conn, options(&corpus, &service, "fatal", None)));
    assert!(
      fatal_fast.contains_key("total"),
      "an empty-severity report must still carry a total row"
    );
    assert_eq!(
      fatal_fast["total"]["tasks"], "0",
      "no fatal tasks -> 0 total"
    );
    let fatal_live = by_name(task_report_live(
      conn,
      options(&corpus, &service, "fatal", None),
    ));
    assert_eq!(
      fatal_fast, fatal_live,
      "empty severity: rollup vs live mismatch"
    );

    // --- Pagination: page_size 1 over the two warning categories (math=2 tasks, font=1) ----------
    // Each page carries its single category plus the always-present whole-severity total row.
    let page0 = by_name(task_report(
      conn,
      options_paged(&corpus, &service, "warning", None, 1, 0),
    ));
    let page1 = by_name(task_report(
      conn,
      options_paged(&corpus, &service, "warning", None, 1, 1),
    ));
    assert!(
      page0.contains_key("math") && !page0.contains_key("font"),
      "page 0 (busiest first) = math only"
    );
    assert!(
      page1.contains_key("font") && !page1.contains_key("math"),
      "page 1 = font only"
    );
    // Totals are whole-severity on every page, not per-page.
    assert_eq!(
      page0["total"]["tasks"], "4",
      "total is whole-severity on page 0"
    );
    assert_eq!(
      page1["total"]["tasks"], "4",
      "total is whole-severity on page 1"
    );
  }

  #[test]
  fn report_uses_rollup_is_always_live() {
    use super::report_uses_rollup;
    // The global `report_summary` matview was retired: every report grain — including the category
    // and `what` aggregate grains that used to be rollup-served — is now computed LIVE and
    // per-(corpus,service)-scoped (see `report_uses_rollup`). So the gate returns false for every
    // input; nothing routes to the matview.
    for sev in [
      "warning",
      "error",
      "fatal",
      "invalid",
      "info",
      "no_problem",
      "todo",
    ] {
      assert!(!report_uses_rollup(Some(sev), None, None, false));
      assert!(!report_uses_rollup(Some(sev), Some("cat"), None, false));
      assert!(!report_uses_rollup(
        Some(sev),
        Some("cat"),
        Some("w"),
        false
      ));
    }
    assert!(!report_uses_rollup(Some("warning"), None, None, true));
    assert!(!report_uses_rollup(None, None, None, false));
  }
}
