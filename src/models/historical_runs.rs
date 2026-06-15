#![allow(clippy::extra_unused_lifetimes)]
use chrono::prelude::*;
use diesel::result::Error;
use diesel::*;
use serde::Serialize;
use std::collections::HashSet;

// use super::messages::*;
// use super::tasks::Task;
// use crate::helpers::TaskStatus;
use crate::backend::progress_report;
use crate::concerns::CortexInsertable;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, Service};
use crate::schema::historical_runs;

#[derive(Identifiable, Queryable, Associations, Clone, Debug, PartialEq, Eq, QueryableByName)]
#[diesel(table_name = historical_runs)]
#[diesel(belongs_to(Corpus, foreign_key = corpus_id))]
#[diesel(belongs_to(Service, foreign_key = service_id))]
/// Historical `(Corpus, Service)` run records
pub struct HistoricalRun {
  /// id of the historical run
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
    let mut start_time_guard = HashSet::new();
    let mut runs_meta_vega = Vec::new();
    for run in runs_meta.iter() {
      // skip extension runs from Vega report, too noisy
      // Tip: rename the "description" field if you want the run included
      if run.description == "extending corpus with more entries" {
        continue;
      }
      // Avoid adding more than one run at a given start_time for the vega metadata stack,
      // as vega wrongly combines the data into a single entry.
      if !run.start_time.is_empty() && !run.end_time.is_empty() {
        if start_time_guard.contains(&run.start_time) {
          continue;
        } else {
          start_time_guard.insert(run.start_time.clone());
        }
      }
      let total = run.field_f32("total");
      for field in ["fatal", "error", "warning", "no_problem", "in_progress"].iter() {
        runs_meta_vega.push(RunMetadataStack {
          severity: (*field).to_string(),
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
#[diesel(table_name = historical_runs)]
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
  fn create(&self, connection: &mut PgConnection) -> Result<usize, Error> {
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
    connection: &mut PgConnection,
  ) -> Result<Vec<HistoricalRun>, Error> {
    use crate::schema::historical_runs::dsl::{corpus_id, service_id, start_time};
    let runs: Vec<HistoricalRun> = historical_runs::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .order(start_time.desc())
      .get_results(connection)?;
    Ok(runs)
  }

  /// The most recent historical runs across **all** corpora/services, newest first — the
  /// system-wide run-management overview. Capped at `limit`.
  pub fn recent_all(
    connection: &mut PgConnection,
    limit: i64,
  ) -> Result<Vec<HistoricalRun>, Error> {
    Self::recent_filtered(connection, None, None, None, limit)
  }

  /// The most recent historical runs, newest first, optionally narrowed to a `corpus_id`,
  /// `service_id`, and/or exact `owner` — the filter-driven run-management overview. Capped at
  /// `limit`. Any combination of filters may be `None` (no constraint on that field).
  pub fn recent_filtered(
    connection: &mut PgConnection,
    corpus: Option<i32>,
    service: Option<i32>,
    owner: Option<&str>,
    limit: i64,
  ) -> Result<Vec<HistoricalRun>, Error> {
    use crate::schema::historical_runs::dsl;
    let mut query = dsl::historical_runs.into_boxed();
    if let Some(corpus) = corpus {
      query = query.filter(dsl::corpus_id.eq(corpus));
    }
    if let Some(service) = service {
      query = query.filter(dsl::service_id.eq(service));
    }
    if let Some(owner) = owner {
      query = query.filter(dsl::owner.eq(owner.to_string()));
    }
    query
      .order(dsl::start_time.desc())
      .limit(limit)
      .get_results(connection)
  }

  /// Obtain a currently ongoing run entry for a  `(Corpus, Service)` pair, if any
  pub fn find_current(
    corpus: &Corpus,
    service: &Service,
    connection: &mut PgConnection,
  ) -> Result<Option<HistoricalRun>, Error> {
    use crate::schema::historical_runs::dsl::{corpus_id, end_time, service_id};
    historical_runs::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .filter(end_time.is_null())
      .first(connection)
      .optional()
  }

  /// Mark this historical run as completed, by setting `end_time` to the current time.
  pub fn mark_completed(&self, connection: &mut PgConnection) -> Result<(), Error> {
    use diesel::dsl::now;
    if self.end_time.is_none() {
      // Freeze the current statistics for this run, then close it.
      let t = live_tally_fields(connection, self.corpus_id, self.service_id);
      update(self)
        .set((
          historical_runs::end_time.eq(now),
          historical_runs::total.eq(t.total),
          historical_runs::in_progress.eq(t.in_progress),
          historical_runs::invalid.eq(t.invalid),
          historical_runs::no_problem.eq(t.no_problem),
          historical_runs::warning.eq(t.warning),
          historical_runs::error.eq(t.error),
          historical_runs::fatal.eq(t.fatal),
        ))
        .execute(connection)?;
    }
    Ok(())
  }

  /// Returns this run with its per-severity tallies reflecting **live** task state *when the run is
  /// still open* (`end_time` is `None`). A run only freezes its tallies at [`Self::mark_completed`]
  /// (i.e. when the next run supersedes it), so an open run's stored tallies are all zero;
  /// overlaying the current progress makes the in-progress run show its real state across the
  /// run-management surfaces — the "live + historical run state" north star — instead of a
  /// misleading row of zeros (most visible on the *current* run and the dashboard's "last run").
  /// A completed run is returned unchanged: its frozen snapshot is authoritative and this is a
  /// no-op (no extra query). Cost is one grouped `progress_report` per *open* run only.
  #[must_use]
  pub fn with_live_tallies(mut self, connection: &mut PgConnection) -> HistoricalRun {
    if self.end_time.is_none() {
      let t = live_tally_fields(connection, self.corpus_id, self.service_id);
      self.total = t.total;
      self.no_problem = t.no_problem;
      self.warning = t.warning;
      self.error = t.error;
      self.fatal = t.fatal;
      self.invalid = t.invalid;
      self.in_progress = t.in_progress;
    }
    self
  }
}

/// The per-severity tallies of a `(corpus, service)` at a point in time.
struct RunTallies {
  total: i32,
  no_problem: i32,
  warning: i32,
  error: i32,
  fatal: i32,
  invalid: i32,
  in_progress: i32,
}

/// Computes the live per-severity tallies for a `(corpus, service)` from the **current** task
/// status distribution — the single source of truth shared by [`HistoricalRun::mark_completed`]
/// (which freezes it) and [`HistoricalRun::with_live_tallies`] (which overlays it on an open run),
/// so the in-progress numbers are identical to what gets frozen at completion. `in_progress` folds
/// the not-yet-finished `todo` + `queued` tasks; `total` excludes invalids (as `progress_report`
/// does).
fn live_tally_fields(connection: &mut PgConnection, corpus_id: i32, service_id: i32) -> RunTallies {
  let report = progress_report(connection, corpus_id, service_id);
  let get = |key: &str| *report.get(key).unwrap_or(&0.0);
  RunTallies {
    total: get("total") as i32,
    no_problem: get("no_problem") as i32,
    warning: get("warning") as i32,
    error: get("error") as i32,
    fatal: get("fatal") as i32,
    invalid: get("invalid") as i32,
    in_progress: (get("queued") + get("todo")) as i32,
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
