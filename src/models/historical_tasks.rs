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

/// One aggregated cell of the status-transition matrix (see
/// [`HistoricalTask::status_change_matrix`]): how many tasks moved from `previous_status` to
/// `current_status` between two snapshots.
#[derive(QueryableByName)]
struct StatusChangeCell {
  /// raw signed status at the earlier (previous) snapshot
  #[diesel(sql_type = Integer)]
  previous_status: i32,
  /// raw signed status at the later (current) snapshot
  #[diesel(sql_type = Integer)]
  current_status: i32,
  /// number of tasks making this exact transition
  #[diesel(sql_type = BigInt)]
  task_count: i64,
}

/// The result of [`HistoricalTask::status_change_matrix`]: the available snapshot-date labels
/// (newest first) and the `(previous_status, current_status, task_count)` transition cells.
pub type StatusChangeMatrix = (Vec<String>, Vec<(i32, i32, i64)>);

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
  /// Retention stats for the per-task snapshot store: total rows and the oldest `saved_at`. This is
  /// the unbounded-growth table (one row per task per save-snapshot), so the admin "manage
  /// historical data" screen surfaces these to decide a retention cutoff.
  pub fn retention_stats(
    connection: &mut PgConnection,
  ) -> Result<(i64, Option<NaiveDateTime>), Error> {
    use crate::schema::historical_tasks::dsl;
    let total: i64 = dsl::historical_tasks.count().get_result(connection)?;
    let oldest: Option<NaiveDateTime> = dsl::historical_tasks
      .select(diesel::dsl::min(dsl::saved_at))
      .first(connection)?;
    Ok((total, oldest))
  }

  /// How many snapshot rows are strictly older than `cutoff` — the **dry-run count** shown before a
  /// prune, so the admin sees exactly what a prune would remove.
  pub fn count_before(connection: &mut PgConnection, cutoff: NaiveDateTime) -> Result<i64, Error> {
    use crate::schema::historical_tasks::dsl;
    dsl::historical_tasks
      .filter(dsl::saved_at.lt(cutoff))
      .count()
      .get_result(connection)
  }

  /// Deletes snapshot rows strictly older than `cutoff` (retention prune), returning the number
  /// removed. The run *summaries* (`historical_runs`) are untouched — only the bulky per-task
  /// snapshots are pruned, so the run history/charts survive while old per-task diffs age out.
  pub fn prune_before(
    connection: &mut PgConnection,
    cutoff: NaiveDateTime,
  ) -> Result<usize, Error> {
    use crate::schema::historical_tasks::dsl;
    diesel::delete(dsl::historical_tasks.filter(dsl::saved_at.lt(cutoff))).execute(connection)
  }

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
    // Always paginated: `page_size` bounds the rows pulled into the application. A `page_size` of 0
    // yields an empty page (`LIMIT 0`) rather than the former unbounded "load every snapshot row"
    // branch, which pulled millions of rows for a large corpus (the unbounded-load class fixed in
    // KNOWN_ISSUES R-8). The diff *summary* counts in SQL via `status_change_matrix` instead.
    {
      // A specific transition (both statuses given) uses a tight query that matches the two
      // (snapshot, status) pairs directly. With a partial or empty status filter we instead list
      // *every* task whose status changed between the two snapshots — the "an empty filter lists
      // every change" contract of the run-tasks screen. Either way the work is paginated in SQL.
      // The unfiltered path previously `.expect()`ed the statuses and **panicked on the request
      // thread** (a 500 that killed the Rocket worker) whenever the tasks screen was opened without
      // picking a transition — see docs/KNOWN_ISSUES.md (F-2).
      let recent_historical_tasks: Vec<HistoricalTaskReport> = if let (Some(prev), Some(cur)) =
        (previous_status, current_status)
      {
        // Optimized query to fetch historical tasks with matching status and saved_at.
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
          AND h.saved_at IN ($3, $5)
        ORDER BY t.entry ASC, h.task_id ASC, h.saved_at ASC
        OFFSET $7
        LIMIT $8;
        "###;
        sql_query(matching_tasks_query)
          .bind::<Integer, _>(corpus.id)
          .bind::<Integer, _>(service.id)
          .bind::<Timestamp, _>(dates[0])
          .bind::<Integer, _>(cur.raw())
          .bind::<Timestamp, _>(dates[1])
          .bind::<Integer, _>(prev.raw())
          .bind::<Integer, _>(2 * offset as i32)
          .bind::<Integer, _>(2 * page_size as i32)
          .get_results::<HistoricalTaskReport>(connection)?
      } else {
        // No (or partial) transition selected: every task present in *both* snapshots whose status
        // differs between them, paginated — two rows per task, ordered so each pair is adjacent.
        let changed_tasks_query = r###"
        WITH matching_tasks AS (
          SELECT h.task_id
          FROM historical_tasks h
          JOIN tasks t ON h.task_id = t.id
          WHERE t.corpus_id = $1 AND t.service_id = $2
            AND h.saved_at IN ($3, $4)
          GROUP BY h.task_id
          HAVING COUNT(DISTINCT h.saved_at) = 2 AND COUNT(DISTINCT h.status) = 2
        )
        SELECT h.task_id, h.status, h.saved_at, t.entry
        FROM historical_tasks h
        JOIN tasks t ON h.task_id = t.id
        WHERE h.saved_at IN ($3, $4)
          AND h.task_id IN (SELECT task_id FROM matching_tasks)
        ORDER BY t.entry ASC, h.task_id ASC, h.saved_at ASC
        OFFSET $5
        LIMIT $6;
        "###;
        sql_query(changed_tasks_query)
          .bind::<Integer, _>(corpus.id)
          .bind::<Integer, _>(service.id)
          .bind::<Timestamp, _>(dates[0])
          .bind::<Timestamp, _>(dates[1])
          .bind::<Integer, _>(2 * offset as i32)
          .bind::<Integer, _>(2 * page_size as i32)
          .get_results::<HistoricalTaskReport>(connection)?
      };
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
    }
    Ok((dates_labels, reported_tasks))
  }

  /// The **status-transition matrix** between two snapshots of a `(corpus, service)`: how many
  /// tasks moved from each previous status to each current status. Unlike [`report_for`], it
  /// aggregates **in SQL** (`GROUP BY previous, current`), so the result is bounded to one row per
  /// status pair (≤ a few dozen) regardless of corpus size — a corpus with two 1.5M-task snapshots
  /// is summarized without loading millions of `historical_tasks` rows into the application (the
  /// unbounded-load class of KNOWN_ISSUES R-7/R-8). Returns the available snapshot-date labels
  /// (newest first) and the matrix cells `(previous_status, current_status, task_count)`. Fewer
  /// than two snapshots (or an out-of-range date pair) yields an empty matrix — a normal "nothing
  /// to diff" result, not an error.
  pub fn status_change_matrix(
    corpus: &Corpus,
    service: &Service,
    previous_date: Option<NaiveDateTime>,
    current_date: Option<NaiveDateTime>,
    connection: &mut PgConnection,
  ) -> Result<StatusChangeMatrix, Error> {
    use crate::schema::historical_tasks::dsl::{saved_at, task_id};
    use crate::schema::tasks::dsl::{corpus_id, service_id};

    // All snapshot dates for this (corpus, service): bounded by the number of saved runs, not by
    // task count (DISTINCT over saved_at). Drives the date picker + the default two-most-recent
    // diff.
    let tasks_subquery = tasks::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .select(tasks::id);
    let all_dates: Vec<NaiveDateTime> = historical_tasks::table
      .filter(task_id.eq_any(tasks_subquery))
      .order(saved_at.desc())
      .select(saved_at)
      .distinct()
      .get_results(connection)?;
    let dates_labels = all_dates
      .iter()
      .map(|date| date.format("%Y-%m-%d %H:%M:%S%.f").to_string())
      .collect();

    // Choose the newer/older snapshots to diff: the caller's pair, else the two most recent.
    let (older, newer) = match (previous_date, current_date) {
      (Some(previous), Some(current)) => (previous, current),
      _ if all_dates.len() > 1 => (all_dates[1], all_dates[0]),
      _ => return Ok((dates_labels, Vec::new())),
    };

    // Count the transitions in the database — one row per (previous, current) status pair, so the
    // application never materializes more than the matrix itself.
    let matrix = sql_query(
      "SELECT prev.status AS previous_status, cur.status AS current_status, \
       COUNT(*) AS task_count \
       FROM historical_tasks prev \
       JOIN historical_tasks cur ON prev.task_id = cur.task_id \
       JOIN tasks t ON t.id = prev.task_id \
       WHERE t.corpus_id = $1 AND t.service_id = $2 \
       AND prev.saved_at = $3 AND cur.saved_at = $4 \
       GROUP BY prev.status, cur.status",
    )
    .bind::<Integer, _>(corpus.id)
    .bind::<Integer, _>(service.id)
    .bind::<Timestamp, _>(older)
    .bind::<Timestamp, _>(newer)
    .get_results::<StatusChangeCell>(connection)?
    .into_iter()
    .map(|cell| (cell.previous_status, cell.current_status, cell.task_count))
    .collect();
    Ok((dates_labels, matrix))
  }
}
