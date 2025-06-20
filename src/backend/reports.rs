use diesel::dsl::sql;
use diesel::*;
use regex::Regex;
use std::collections::HashMap;

use crate::helpers::TaskStatus;
use crate::models::{Corpus, Service, Task};
use crate::reports::{AggregateReport, TaskDetailReport};
use crate::schema::tasks;

lazy_static! {
  static ref ENTRY_NAME_REGEX: Regex = Regex::new(r"^(.+)/[^/]+$").unwrap();
  static ref TASK_REPORT_NAME_REGEX: Regex = Regex::new(r"^.+/(.+)\..+$").unwrap();
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

pub(crate) fn task_report(
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
        .filter(status.eq(task_status.unwrap().raw()))
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
      let total_count: i64 = tasks::table
        .filter(service_id.eq(service.id))
        .filter(corpus_id.eq(corpus.id))
        .count()
        .get_result(connection)
        .unwrap();
      let invalid_count: i64 = tasks::table
        .filter(service_id.eq(service.id))
        .filter(corpus_id.eq(corpus.id))
        .filter(status.eq(TaskStatus::Invalid.raw()))
        .count()
        .get_result(connection)
        .unwrap();
      let total_valid_count = total_count - invalid_count;

      let log_table = match task_status {
        Some(ref ts) => ts.to_table(),
        None => {
          all_messages = true;
          "log_infos".to_string()
        },
      };

      let task_status_raw = task_status.unwrap_or(TaskStatus::NoProblem).raw();
      let status_clause = if !all_messages {
        String::from("status=$3")
      } else {
        String::from("status < $3 and status > ") + &TaskStatus::Invalid.raw().to_string()
      };
      let bind_status = if !all_messages {
        task_status_raw
      } else {
        task_status_raw + 1 // TODO: better would be a .prev() method or so, since this hardwires
                            // the assumption of using adjacent negative
                            // integers
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
          let status_report_rows_result = status_report_query.get_result(connection);
          let status_report_rows: AggregateReport = status_report_rows_result.unwrap();

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
                let this_category_report: AggregateReport =
                  this_category_report_query.get_result(connection).unwrap();

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
                  + "and category=$4 and what=$5 ORDER BY tasks.entry ASC offset $6 limit $7";

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
    None => {
      let total_entry = stats_hash.get_mut("total").unwrap();
      *total_entry
    },
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
      let trimmed_entry = task.entry.trim_end().to_string();
      if service.name == "import" {
        trimmed_entry
      } else {
        ENTRY_NAME_REGEX.replace(&trimmed_entry, "$1").to_string() + "/" + &service.name + ".zip"
      }
    })
    .collect()
}
