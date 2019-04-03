use chrono::prelude::*;
use diesel::result::Error;
use diesel::*;
use serde::Serialize;

// use super::messages::*;
// use super::tasks::Task;
// use crate::helpers::TaskStatus;
use crate::backend::progress_report;
use crate::concerns::CortexInsertable;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, Service};
use crate::schema::historical_runs;

#[derive(Identifiable, Queryable, Clone, Debug, PartialEq, Eq, QueryableByName)]
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
  pub start_time: NaiveDateTime,
  /// end timestamp of run, i.e. timestamp of next run initiation
  pub end_time: Option<NaiveDateTime>,
  /// owner who initiated the run
  pub owner: String,
  /// description of the purpose of this run
  pub description: String,
}

#[derive(Debug, Serialize, Clone)]
/// A JSON-friendly data structure, used for the frontend reports
pub struct RunMetadata {
  /// total tasks in run
  pub total: i32,
  /// invalid tasks in run
  pub invalid: i32,
  /// fatak tasks in run
  pub fatal: i32,
  /// error tasks in run
  pub error: i32,
  /// warning tasks in run
  pub warning: i32,
  /// no_problem tasks in run
  pub no_problem: i32,
  /// in_progress tasks in run
  pub in_progress: i32,
  /// start time of run, formatted for a report
  pub start_time: String,
  /// end time of run, formatted for a report
  pub end_time: String,
  /// initiator of the run
  pub owner: String,
  /// description of the run
  pub description: String,
}
impl RunMetadata {
  /// f32 type cast for the run frequency fields
  pub fn field_f32(&self, field: &str) -> f32 {
    let field_i32 = match field {
      "invalid" => self.invalid,
      "total" => self.total,
      "fatal" => self.fatal,
      "error" => self.error,
      "warning" => self.warning,
      "no_problem" => self.no_problem,
      "in_progress" => self.in_progress,
      _ => unimplemented!(),
    };
    field_i32 as f32
  }
}

#[derive(Debug, Serialize, Clone)]
/// A JSON-friendly data structure, used for vega-lite Stack figures
/// https://vega.github.io/vega-lite/docs/stack.html
pub struct RunMetadataStack {
  /// type of messages
  pub severity: String,
  /// raw severity index
  pub severity_numeric: i32,
  /// percent to total
  pub percent: f32,
  /// total number of jobs
  pub total: i32,
  /// start time of run, formatted for a report
  pub start_time: String,
  /// end time of run, formatted for a report
  pub end_time: String,
  /// initiator of the run
  pub owner: String,
  /// description of the run
  pub description: String,
}
impl RunMetadataStack {
  /// Transforms to a vega-lite Stack -near representation
  pub fn transform(runs_meta: &[RunMetadata]) -> Vec<RunMetadataStack> {
    let mut runs_meta_vega = Vec::new();
    for run in runs_meta.iter() {
      let total = run.field_f32("total");
      for field in &["fatal", "error", "warning", "no_problem", "in_progress"] {
        runs_meta_vega.push(RunMetadataStack {
          severity: field.to_string(),
          severity_numeric: TaskStatus::from_key(field).unwrap().raw(),
          percent: (100.0 * run.field_f32(field)) / total,
          total: run.total,
          start_time: run.start_time.clone(),
          end_time: run.end_time.clone(),
          owner: run.owner.clone(),
          description: run.description.clone(),
        })
      }
    }
    runs_meta_vega
  }
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
    use crate::schema::historical_runs::dsl::{corpus_id, service_id, start_time};
    let runs: Vec<HistoricalRun> = historical_runs::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .order(start_time.desc())
      .get_results(connection)?;
    Ok(runs)
  }

  /// Obtain a currently ongoing run entry for a  `(Corpus, Service)` pair, if any
  pub fn find_current(
    corpus: &Corpus,
    service: &Service,
    connection: &PgConnection,
  ) -> Result<Option<HistoricalRun>, Error>
  {
    use crate::schema::historical_runs::dsl::{corpus_id, end_time, service_id};
    historical_runs::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .filter(end_time.is_null())
      .first(connection)
      .optional()
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
      let queued_count_f64: f64 =
        report.get("queued").unwrap_or(&0.0) + report.get("todo").unwrap_or(&0.0);
      let in_progress = queued_count_f64 as i32;

      //
      update(self)
        .set((
          historical_runs::end_time.eq(now),
          historical_runs::total.eq(total),
          historical_runs::in_progress.eq(in_progress),
          historical_runs::invalid.eq(invalid),
          historical_runs::no_problem.eq(no_problem),
          historical_runs::warning.eq(warning),
          historical_runs::error.eq(error),
          historical_runs::fatal.eq(fatal),
        ))
        .execute(connection)?;
    }
    Ok(())
  }
}

impl From<HistoricalRun> for RunMetadata {
  fn from(run: HistoricalRun) -> RunMetadata {
    let HistoricalRun {
      total,
      warning,
      error,
      no_problem,
      invalid,
      fatal,
      start_time,
      end_time,
      description,
      in_progress,
      owner,
      ..
    } = run;
    RunMetadata {
      total,
      invalid,
      fatal,
      warning,
      error,
      no_problem,
      in_progress,
      start_time: start_time.format("%Y-%m-%d").to_string(),
      end_time: match end_time {
        Some(etime) => etime.format("%Y-%m-%d").to_string(),
        None => String::new(),
      },
      owner,
      description,
    }
  }
}
