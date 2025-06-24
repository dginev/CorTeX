#![allow(clippy::extra_unused_lifetimes)]
use chrono::prelude::*;
use diesel::result::Error;
use diesel::*;
use rocket::serde::Serialize;
// use super::messages::*;
// use super::tasks::Task;
// use crate::helpers::TaskStatus;
use crate::concerns::CortexInsertable;
use crate::models::{Corpus, Service, Task};
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

impl CortexInsertable for NewHistoricalTask {
  fn create(&self, connection: &mut PgConnection) -> Result<usize, Error> {
    insert_into(historical_tasks::table)
      .values(self)
      .execute(connection)
  }
}

impl HistoricalTask {
  /// Obtain all historical records for a given task id
  pub fn find_by(
    needle_id: i64,
    connection: &mut PgConnection,
  ) -> Result<Vec<HistoricalTask>, Error> {
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
  pub fn report_for(
    corpus: &Corpus,
    service: &Service,
    offset: Option<i64>,
    connection: &mut PgConnection,
  ) -> Result<Vec<(String, HistoricalTask, HistoricalTask)>, Error> {
    use crate::schema::historical_tasks::dsl::{saved_at, task_id};
    use crate::schema::tasks::dsl::{corpus_id, service_id};
    // 1. First we obtain upto 100 tasks, for the given (corpus, service) pair, starting from the
    //    given offset.
    let tasks_batch: Vec<Task> = tasks::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .order(tasks::id.asc())
      .limit(100)
      .offset(offset.unwrap_or(0))
      .get_results(connection)?;
    // 2. Next, we extract all historical tasks for those 100 ids, using a single query,
    //    descendingly sorting by saved_at.

    let historical_tasks_batch: Vec<HistoricalTask> = historical_tasks::table
      .filter(task_id.eq_any(tasks_batch.iter().map(|task| task.id)))
      .order(saved_at.desc())
      .get_results(connection)?;
    // 3. Lastly, we further constrain the reported tasks by only picking the two most recent of
    //    each task.
    let mut reported_tasks = Vec::new();
    for task in tasks_batch {
      let mut latest_two = historical_tasks_batch
        .iter()
        .filter(|h| h.task_id == task.id)
        .take(2)
        .cloned()
        .collect::<Vec<_>>();
      // Note: we pad the report to always have 2 entries, even if one or none are available, to
      // simplify Also, skip empty cases.
      if latest_two.len() == 1 {
        reported_tasks.push((task.entry, latest_two[0].clone(), latest_two.remove(0)));
      } else if latest_two.len() == 2 {
        reported_tasks.push((task.entry, latest_two.remove(0), latest_two.remove(0)));
      }
    }
    Ok(reported_tasks)
  }
}
