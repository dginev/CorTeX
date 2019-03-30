use diesel::result::Error;
use diesel::*;
use std::time::SystemTime;

// use super::messages::*;
// use super::tasks::Task;
// use crate::helpers::TaskStatus;
use crate::concerns::CortexInsertable;
use crate::models::{Corpus, Service};
use crate::schema::historical_runs;
use crate::backend::progress_report;

#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug, PartialEq, Eq, QueryableByName)]
#[table_name = "historical_runs"]
/// Historical `(Corpus, Service)` run records
pub struct HistoricalRun {
  /// task primary key, auto-incremented by postgresql
  pub id: i32,
  /// id of the service owning this task
  pub service_id: i32,
  /// id of the corpus hosting this task
  pub corpus_id: i32,
  /// total tasks in run
  pub total: i32,
  /// invalid tasks in run
  pub invalid: i32,
  /// fatal results in run
  pub fatal: i32,
  /// error results in run
  pub error: i32,
  /// warning results in run
  pub warning: i32,
  /// results with no notable problems in run
  pub no_problem: i32,
  /// tasks still in progress at end of run
  pub in_progress: i32,
  /// start timestamp of run
  pub start_time: SystemTime,
  /// end timestamp of run, i.e. timestamp of next run initiation
  pub end_time: Option<SystemTime>,
  /// owner who initiated the run
  pub owner: String,
  /// description of the purpose of this run
  pub description: String,
}

#[derive(Insertable, Debug, Clone)]
#[table_name = "historical_runs"]
/// A new task, to be inserted into `CorTeX`
pub struct NewHistoricalRun {
  /// id of the service owning this task
  pub service_id: i32,
  /// id of the corpus hosting this task
  pub corpus_id: i32,
  /// description of the purpose of this run
  pub description: String,
  /// owner who initiated the run
  pub owner: String,
}

impl CortexInsertable for NewHistoricalRun {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(historical_runs::table)
      .values(self)
      .execute(connection)
  }
}

impl HistoricalRun {
  /// Obtain all historical runs for a given `(Corpus, Service)` pair
  pub fn find_by(
    corpus: &Corpus,
    service: &Service,
    connection: &PgConnection,
  ) -> Result<Vec<HistoricalRun>, Error>
  {
    use crate::schema::historical_runs::dsl::{corpus_id, service_id};
    let runs: Vec<HistoricalRun> = historical_runs::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .get_results(connection)?;
    Ok(runs)
  }

  /// Mark this historical run as completed, by setting `end_time` to the current time.
  pub fn mark_completed(&self, connection: &PgConnection) -> Result<(), Error> {
    use diesel::dsl::now;
    if self.end_time.is_none() {
      // gather the current statistics for this run, then update.
      let report = progress_report(connection, self.corpus_id, self.service_id);
      let total = *report.get("total").unwrap_or(&0.0) as i32;
      let no_problem = *report.get("no_problem").unwrap_or(&0.0) as i32;
      let warning = *report.get("warning").unwrap_or(&0.0) as i32;
      let error = *report.get("error").unwrap_or(&0.0) as i32;
      let fatal = *report.get("fatal").unwrap_or(&0.0) as i32;
      let invalid = *report.get("invalid").unwrap_or(&0.0) as i32;
      let queued_count_f64: f64 = report.get("queued").unwrap_or(&0.0) + report.get("todo").unwrap_or(&0.0);
      let in_progress = queued_count_f64 as i32;

      //
      update(self)
        .set((historical_runs::end_time.eq(now),
         historical_runs::total.eq(total),
         historical_runs::in_progress.eq(in_progress),
         historical_runs::invalid.eq(invalid),
         historical_runs::no_problem.eq(no_problem),
         historical_runs::warning.eq(warning),
         historical_runs::error.eq(error),
         historical_runs::fatal.eq(fatal)))
        .execute(connection)?;
    }
    Ok(())
  }
}
