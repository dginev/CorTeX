#![allow(clippy::extra_unused_lifetimes)]
use chrono::prelude::*;
use diesel::result::Error;
use diesel::*;

// use super::messages::*;
// use super::tasks::Task;
// use crate::helpers::TaskStatus;
use crate::concerns::CortexInsertable;
use crate::schema::historical_tasks;

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
}
