#![allow(clippy::extra_unused_lifetimes)]
use chrono::prelude::*;
// use diesel::pg::Pg;
use diesel::result::Error;
use diesel::sql_types::{BigInt, Integer, Text, Timestamp};
use diesel::*;
use rocket::serde::Serialize;
// use super::messages::*;
// use super::tasks::Task;
// use crate::helpers::TaskStatus;
use crate::concerns::CortexInsertable;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, Service};
use crate::schema::{historical_tasks, tasks};

#[derive(Identifiable, Queryable, Clone, Debug, PartialEq, Eq, QueryableByName)]
#[diesel(table_name = historical_tasks)]
/// Historical `(Corpus, Service)` run records
pub struct HistoricalTask {
  /// id of the historical record (task granularity)
  pub id: i64,
  /// foreign key in Tasks(id)
  pub task_id: i64,
  /// The historical status of the task
  pub status: i32,
  /// When was the save request for this historical record made
  pub saved_at: NaiveDateTime,
}

#[derive(Insertable, Debug, Clone)]
#[diesel(table_name = historical_tasks)]
/// A new task, to be inserted into `CorTeX`
pub struct NewHistoricalTask {
  /// id of the task we are tracking
  pub task_id: i64,
  /// the historical status of the task
  pub status: i32,
}

#[derive(Debug, Clone, Serialize)]
/// A single report row for the diff-historical feature
pub struct TaskRunMetadata {
  /// id of the task we are tracking
  pub task_id: String,
  /// The underling entry of the task
  pub entry: String,
  /// The previous result for processing this task
  pub previous_status: String,
  /// The current result for processing this task
  pub current_status: String,
  /// The previous highlight for displaying this task
  pub previous_highlight: String,
  /// The current highlight for displaying this task
  pub current_highlight: String,
  /// Previous timestamp of a manual save
  pub previous_saved_at: String,
  /// Current/latest timestamp of a manual save
  pub current_saved_at: String,
}

#[derive(Debug, Clone, Serialize)]
/// A single report row for the diff-summary feature
pub struct DiffStatusRow {
  /// The previous result for processing this task
  pub previous_status: String,
  /// The current result for processing this task
  pub current_status: String,
  /// The previous highlight for displaying this task
  pub previous_highlight: String,
  /// The current highlight for displaying this task
  pub current_highlight: String,
  /// Task count
  pub task_count: usize,
}

#[derive(Debug, Clone, Default)]
/// A collection of filters for task status history reports
pub struct DiffStatusFilter {
  /// The previous result for processing this task
  pub previous_status: Option<TaskStatus>,
  /// The current result for processing this task
  pub current_status: Option<TaskStatus>,
  /// The requested previous date for this manual save
  pub previous_date: Option<NaiveDateTime>,
  /// The requested current date for this manual save
  pub current_date: Option<NaiveDateTime>,
  /// Starting offset
  pub offset: usize,
  /// Page size
  pub page_size: usize,
}

/// A historical overview contains the list of labels for all dates where a snapshot was taken,
/// followed by pairs of reports for tasks at the previous date and current date chosen.
pub type HistoricalReportOverview = (
  Vec<String>,
  Vec<(HistoricalTaskReport, HistoricalTaskReport)>,
);

impl CortexInsertable for NewHistoricalTask {
  fn create(&self, connection: &mut PgConnection) -> Result<usize, Error> {
    insert_into(historical_tasks::table)
      .values(self)
      .execute(connection)
  }
}

#[derive(Debug, Clone, PartialEq, Eq, QueryableByName)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[diesel(table_name = historical_tasks)]
/// A reportable historical task
pub struct HistoricalTaskReport {
  #[diesel(sql_type = BigInt)]
  /// the id from the task table
  pub task_id: i64,
  #[diesel(sql_type = Integer)]
  /// the recorded status at this timestamp
  pub status: i32,
  #[diesel(sql_type = Timestamp)]
  /// the saved request was at this time
  pub saved_at: NaiveDateTime,
  #[diesel(sql_type = Text)]
  /// the filename on disk for this task
  pub entry: String,
}

impl HistoricalTask {
  /// Obtain all historical records for a given task id
  pub fn find_by(needle_id: i64, connection: &mut PgConnection) -> Result<Vec<Self>, Error> {
    use crate::schema::historical_tasks::dsl::{saved_at, task_id};
    let runs: Vec<HistoricalTask> = historical_tasks::table
      .filter(task_id.eq(needle_id))
      .order(saved_at.desc())
      .get_results(connection)?;
    Ok(runs)
  }

  /// Obtain the most recent historical record for a given taskid
  pub fn find_most_recent(
    needle_id: i64,
    connection: &mut PgConnection,
  ) -> Result<Option<HistoricalTask>, Error> {
    use crate::schema::historical_tasks::dsl::{saved_at, task_id};
    historical_tasks::table
      .filter(task_id.eq(needle_id))
      .order(saved_at.desc())
      .first(connection)
      .optional()
  }

  /// Prepare a report for diffing the two most recent historical records of all tasks belonging to
  /// a `(corpus,service)` pair. We do this 100 tasks at a time, starting from the given offset.
  /// The return contract is (id, previous, current)
  pub fn report_for(
    corpus: &Corpus,
    service: &Service,
    filters: Option<DiffStatusFilter>,
    connection: &mut PgConnection,
  ) -> Result<HistoricalReportOverview, Error> {
    use crate::schema::historical_tasks::dsl::{saved_at, task_id};
    use crate::schema::tasks::dsl::{corpus_id, service_id};
    // 1. We need to know the cutoff date of the previous status, to only select the relevant
    //    entries.
    let tasks_subquery = tasks::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .order(tasks::id.asc())
      .select(tasks::id);
    let mut dates: Vec<NaiveDateTime> = Vec::new();
    let mut previous_status = None;
    let mut current_status = None;
    let mut offset = 0;
    let mut page_size = 0;
    if let Some(opts) = filters {
      previous_status = opts.previous_status;
      current_status = opts.current_status;
      offset = opts.offset;
      page_size = opts.page_size;
      if let (Some(previous_date_param), Some(current_date_param)) =
        (opts.previous_date, opts.current_date)
      {
        dates.push(current_date_param);
        dates.push(previous_date_param);
      }
    }
    let all_dates = historical_tasks::table
      .filter(task_id.eq_any(tasks_subquery))
      .order(saved_at.desc())
      .select(saved_at)
      .distinct()
      .get_results(connection)?;
    if dates.is_empty() && all_dates.len() > 1 {
      dates = vec![all_dates[0], all_dates[1]];
    }
    // 2. Next, we extract all historical tasks for those 100 ids, using a single query,
    //    descendingly sorting by saved_at.
    if dates.len() < 2 {
      return Ok((Vec::new(), Vec::new()));
    }
    let dates_labels = all_dates
      .into_iter()
      .map(|date| date.format("%Y-%m-%d %H:%M:%S%.f").to_string())
      .collect();
    let mut reported_tasks = Vec::new();
    if page_size > 0 {
      // Optimized query to fetch historical tasks with matching status and saved_at
      let matching_tasks_query = r###"
        WITH matching_tasks AS (
          SELECT h.task_id
          FROM historical_tasks h
          JOIN tasks t ON h.task_id = t.id
          WHERE t.corpus_id = $1 AND t.service_id = $2
            AND (
              (h.saved_at = $3 AND h.status = $4) OR
              (h.saved_at = $5 AND h.status = $6)
            )
          GROUP BY h.task_id
          HAVING COUNT(DISTINCT h.status) = 2
        )
        SELECT h.task_id, h.status, h.saved_at, t.entry
        FROM historical_tasks h
        JOIN tasks t ON h.task_id = t.id
        WHERE h.task_id IN (SELECT task_id FROM matching_tasks)
        ORDER BY t.entry ASC, h.task_id ASC, h.saved_at ASC
        OFFSET $7
        LIMIT $8;
        "###;

      let main_query = sql_query(matching_tasks_query)
        .bind::<Integer, _>(corpus.id)
        .bind::<Integer, _>(service.id)
        .bind::<Timestamp, _>(dates[0])
        .bind::<Integer, _>(current_status.expect("Current status is required").raw())
        .bind::<Timestamp, _>(dates[1])
        .bind::<Integer, _>(previous_status.expect("Previous status is required").raw())
        .bind::<Integer, _>(2 * offset as i32)
        .bind::<Integer, _>(2 * page_size as i32);
      // let debug = debug_query::<Pg, _>(&main_query);
      let recent_historical_tasks: Vec<HistoricalTaskReport> =
        main_query.get_results::<HistoricalTaskReport>(connection)?;
      // Iterate in pairs, as applicable
      let mut iter = recent_historical_tasks.into_iter();
      while let Some(t1) = iter.next() {
        if let Some(t2) = iter.next() {
          if t1.task_id == t2.task_id {
            reported_tasks.push((t1, t2));
          }
        } else {
          break;
        }
      }
    } else {
      // Top-level report without filters
      let all_historical_tasks: Vec<HistoricalTask> = historical_tasks::table
        .filter(saved_at.eq(dates[0]))
        .or_filter(saved_at.eq(dates[1]))
        .filter(task_id.eq_any(tasks_subquery))
        .order((task_id.asc(), saved_at.asc()))
        .select(historical_tasks::all_columns)
        .get_results(connection)?;
      let mut peek_tasks = all_historical_tasks.into_iter().peekable();
      while let Some(task) = peek_tasks.next() {
        if let Some(next_task) = peek_tasks.peek() {
          if task.task_id == next_task.task_id {
            let t1_report = HistoricalTaskReport {
              task_id: task.task_id,
              status: task.status,
              saved_at: task.saved_at,
              entry: String::new(),
            };
            let next_task = peek_tasks.next().unwrap();
            let t2_report = HistoricalTaskReport {
              task_id: next_task.task_id,
              status: next_task.status,
              saved_at: next_task.saved_at,
              entry: String::new(),
            };
            reported_tasks.push((t1_report, t2_report));
          }
        }
      }
    }
    Ok((dates_labels, reported_tasks))
  }
}
