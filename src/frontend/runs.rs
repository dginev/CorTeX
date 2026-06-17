// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Historical-runs capability: inspect the run history of a `(corpus, service)` as an agent API
//! (the JSON twin of the human history screen).
//!
//! Follows the symmetry contract — one shared [`RunDto`] is the read model for both the agent API
//! (`GET /api/runs/...`) and the server-rendered human screen ([`runs_page`], `GET /runs/...`).
//! Handlers live here; the app is assembled in [`crate::frontend::server`]. The binary's legacy
//! `history` page (Vega charts) still renders today and migrates onto this surface in a later
//! increment.

use chrono::NaiveDateTime;
use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::{Template, context};
use serde::Serialize;

use std::collections::HashMap;

use crate::backend::{DbPool, list_task_diffs, summary_task_diffs};
use crate::frontend::actor::{AdminReject, AdminSession, ReturnTo, require_admin_to};
use crate::frontend::helpers::decorate_uri_encodings;
use crate::frontend::params::{MAX_REPORT_OFFSET, MAX_REPORT_PAGE_SIZE, TemplateContext};
use crate::helpers::TaskStatus;
use crate::models::{
  Corpus, DiffStatusFilter, HistoricalRun, RunMetadata, RunMetadataStack, Service, TaskRunMetadata,
};

/// A historical `(corpus, service)` run as exposed over the API: a stable `id` handle,
/// who/why/when, whether it has completed, and the per-severity task tallies. For a **completed**
/// run these are the snapshot frozen at completion; for an **open** run they are overlaid with
/// **live** progress (`HistoricalRun::with_live_tallies`), since stored tallies are only frozen at
/// completion — so an in-progress run reports its real state, not zeros.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RunDto {
  /// Stable external handle (UUIDv7) for this run — the durable token for referencing it.
  pub public_id: String,
  /// Internal serial run id.
  pub id: i32,
  /// Who initiated the run.
  pub owner: String,
  /// Why the run was initiated (free-text description / rerun filter summary).
  pub description: String,
  /// Run start, ISO-8601 (`YYYY-MM-DDThh:mm:ss`, naive/local).
  pub start_time: String,
  /// Run end, ISO-8601; `None` while the run is still open (it closes when the next run starts).
  pub end_time: Option<String>,
  /// Whether the run has completed (`end_time` is set).
  pub completed: bool,
  /// Total tasks in the run (excludes invalids from the denominator elsewhere).
  pub total: i32,
  /// Tasks that completed with no notable problems.
  pub no_problem: i32,
  /// Tasks that completed with warnings.
  pub warning: i32,
  /// Tasks that completed with errors.
  pub error: i32,
  /// Tasks that failed fatally.
  pub fatal: i32,
  /// Invalid tasks (excluded from totals).
  pub invalid: i32,
  /// Tasks still in progress when the run closed.
  pub in_progress: i32,
}

impl From<HistoricalRun> for RunDto {
  fn from(run: HistoricalRun) -> RunDto {
    RunDto {
      public_id: run.public_id.to_string(),
      id: run.id,
      owner: run.owner,
      description: run.description,
      start_time: crate::frontend::helpers::iso_utc(run.start_time),
      end_time: run.end_time.map(crate::frontend::helpers::iso_utc),
      completed: run.end_time.is_some(),
      total: run.total,
      no_problem: run.no_problem,
      warning: run.warning,
      error: run.error,
      fatal: run.fatal,
      invalid: run.invalid,
      in_progress: run.in_progress,
    }
  }
}

/// One run-over-run tally change for the human history table: the signed delta and the CSS class
/// that colours it (good/bad/neutral). Human-render only — the agent `/api/runs` feed stays raw
/// tallies (agents compute their own deltas).
#[derive(Debug, Serialize)]
struct DeltaCell {
  /// Signed change vs the next-older run (`this.tally - older.tally`).
  v: i32,
  /// CSS class encoding the *direction's meaning*: `delta-good` (improvement), `delta-bad`
  /// (regression), or `delta-zero` (no change). Severity-aware — see [`delta_class`].
  cls: &'static str,
}

/// Per-severity change of a run vs the next-older run, so the table answers "how did this run move
/// the conversion tallies" directly instead of making the reader subtract consecutive rows.
#[derive(Debug, Serialize)]
struct RunDelta {
  /// Change in clean conversions (more is better — the headline conversion-rate movement).
  no_problem: DeltaCell,
  /// Change in warnings (fewer is better).
  warning: DeltaCell,
  /// Change in errors (fewer is better).
  error: DeltaCell,
  /// Change in fatal failures (fewer is better).
  fatal: DeltaCell,
}

/// A run-history table row: the shared [`RunDto`] tallies plus the human-only run-over-run delta.
/// `delta` is `None` for the oldest run (nothing older to compare against).
#[derive(Debug, Serialize)]
struct RunRow {
  /// The run's absolute tallies (flattened, so the template reads `run.no_problem` etc.).
  #[serde(flatten)]
  run: RunDto,
  /// Change vs the next-older run; `None` for the oldest row.
  delta: Option<RunDelta>,
}

/// Maps a signed tally delta to its colour class given the severity's polarity. For `no_problem`
/// more is better (`higher_is_better = true`); for warning/error/fatal fewer is better. A zero
/// delta is `delta-zero` (muted) so an unchanged column reads as "no change", not "no data".
fn delta_class(delta: i32, higher_is_better: bool) -> &'static str {
  if delta == 0 {
    "delta-zero"
  } else if (delta > 0) == higher_is_better {
    "delta-good"
  } else {
    "delta-bad"
  }
}

impl RunDelta {
  /// Builds the per-severity deltas of `run` against the next-older `older` run.
  fn between(run: &RunDto, older: &RunDto) -> RunDelta {
    let cell = |delta: i32, higher_is_better: bool| DeltaCell {
      v: delta,
      cls: delta_class(delta, higher_is_better),
    };
    RunDelta {
      no_problem: cell(run.no_problem - older.no_problem, true),
      warning: cell(run.warning - older.warning, false),
      error: cell(run.error - older.error, false),
      fatal: cell(run.fatal - older.fatal, false),
    }
  }
}

/// A historical run as exposed in the **system-wide** overview: the per-`(corpus, service)`
/// [`RunDto`] fields plus the corpus + service names (the overview spans every pair, so the names
/// are part of the row). The read model for `GET /api/runs` and the `/admin/runs` management
/// screen.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RunOverviewDto {
  /// Stable external handle (UUIDv7) for this run — the durable token for referencing it.
  pub public_id: String,
  /// The corpus the run targeted.
  pub corpus: String,
  /// The service the run targeted.
  pub service: String,
  /// Who initiated the run.
  pub owner: String,
  /// Why the run was initiated.
  pub description: String,
  /// Run start, ISO-8601.
  pub start_time: String,
  /// Run end, ISO-8601; `None` while still open.
  pub end_time: Option<String>,
  /// Whether the run has completed.
  pub completed: bool,
  /// Total tasks in the run.
  pub total: i32,
  /// Tasks with no notable problems.
  pub no_problem: i32,
  /// Tasks with warnings.
  pub warning: i32,
  /// Tasks with errors.
  pub error: i32,
  /// Fatally-failed tasks.
  pub fatal: i32,
  /// Invalid tasks.
  pub invalid: i32,
  /// Tasks still in progress when the run closed.
  pub in_progress: i32,
}

impl RunOverviewDto {
  /// Builds the overview row from a run + the corpus/service name lookups (unknown ids render as
  /// their numeric id rather than failing the whole listing).
  fn build(
    run: HistoricalRun,
    corpora: &HashMap<i32, String>,
    services: &HashMap<i32, String>,
  ) -> RunOverviewDto {
    RunOverviewDto {
      public_id: run.public_id.to_string(),
      corpus: corpora
        .get(&run.corpus_id)
        .cloned()
        .unwrap_or_else(|| format!("#{}", run.corpus_id)),
      service: services
        .get(&run.service_id)
        .cloned()
        .unwrap_or_else(|| format!("#{}", run.service_id)),
      owner: run.owner,
      description: run.description,
      start_time: crate::frontend::helpers::iso_utc(run.start_time),
      end_time: run.end_time.map(crate::frontend::helpers::iso_utc),
      completed: run.end_time.is_some(),
      total: run.total,
      no_problem: run.no_problem,
      warning: run.warning,
      error: run.error,
      fatal: run.fatal,
      invalid: run.invalid,
      in_progress: run.in_progress,
    }
  }
}

/// Loads the most-recent historical runs as overview rows, optionally narrowed by `corpus` and/or
/// `service` name and/or exact `owner` (the **filter-driven** run-management surface the owner
/// asked for) — the shared core of `GET /api/runs` and the `/admin/runs` screen. An unknown
/// corpus/service name matches nothing (empty result, not an error). `limit` is clamped by the
/// caller.
fn load_recent_runs(
  pool: &DbPool,
  corpus: Option<&str>,
  service: Option<&str>,
  owner: Option<&str>,
  limit: i64,
) -> Result<Vec<RunOverviewDto>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  // Resolve the corpus/service name filters to ids; an unknown name narrows to nothing.
  let corpus_id = match corpus.filter(|name| !name.is_empty()) {
    Some(name) => match Corpus::find_by_name(name, &mut connection) {
      Ok(corpus) => Some(corpus.id),
      Err(_) => return Ok(Vec::new()),
    },
    None => None,
  };
  let service_id = match service.filter(|name| !name.is_empty()) {
    Some(name) => match Service::find_by_name(name, &mut connection) {
      Ok(service) => Some(service.id),
      Err(_) => return Ok(Vec::new()),
    },
    None => None,
  };
  let owner = owner.filter(|owner| !owner.is_empty());
  let runs = HistoricalRun::recent_filtered(&mut connection, corpus_id, service_id, owner, limit)
    .map_err(|_| Status::InternalServerError)?;
  // Open runs freeze their tallies only at completion, so their stored counts are zero; overlay the
  // live progress so an in-progress run shows real numbers (a no-op for completed runs). Batched
  // into one query across all open runs — this is a system-wide list, so a per-run overlay would be
  // an N+1 over the open-run count (KNOWN_ISSUES P-1).
  let runs = HistoricalRun::overlay_live_tallies(runs, &mut connection);
  // The corpora/services tables are small; one batched read each beats N+1 per-run lookups.
  let corpora: HashMap<i32, String> = Corpus::all(&mut connection)
    .unwrap_or_default()
    .into_iter()
    .map(|corpus| (corpus.id, corpus.name))
    .collect();
  let services: HashMap<i32, String> = Service::all(&mut connection)
    .unwrap_or_default()
    .into_iter()
    .map(|service| (service.id, service.name))
    .collect();
  Ok(
    runs
      .into_iter()
      .map(|run| RunOverviewDto::build(run, &corpora, &services))
      .collect(),
  )
}

/// The system-wide run history (agent twin of the `/admin/runs` screen): the most recent runs,
/// newest first, optionally filtered by `corpus`/`service`/`owner`, capped at `limit` (default 100,
/// max 500). `503` if the pool is exhausted.
#[rocket_okapi::openapi(tag = "Runs")]
#[get("/api/runs?<corpus>&<service>&<owner>&<limit>")]
pub fn api_all_runs(
  corpus: Option<String>,
  service: Option<String>,
  owner: Option<String>,
  limit: Option<i64>,
  pool: &State<DbPool>,
) -> Result<Json<Vec<RunOverviewDto>>, Status> {
  let limit = limit.unwrap_or(100).clamp(1, 500);
  Ok(Json(load_recent_runs(
    pool,
    corpus.as_deref(),
    service.as_deref(),
    owner.as_deref(),
    limit,
  )?))
}

/// The system-wide run-management overview (`GET /admin/runs`): the most recent runs, filterable by
/// corpus/service/owner, each linking into its per-service history + diff drill-downs. Signed-in
/// admins only (unauthenticated → sign-in, returning here).
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/admin/runs?<corpus>&<service>&<owner>&<limit>")]
pub fn all_runs_page(
  corpus: Option<String>,
  service: Option<String>,
  owner: Option<String>,
  limit: Option<i64>,
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  let admin = require_admin_to(session, &return_to)?;
  let limit = limit.unwrap_or(100).clamp(1, 500);
  // Best-effort, like the other admin screens: a db hiccup renders an empty table, never a 500.
  let runs = load_recent_runs(
    pool,
    corpus.as_deref(),
    service.as_deref(),
    owner.as_deref(),
    limit,
  )
  .unwrap_or_default();
  // The known corpus + service names seed the filter dropdowns (small tables).
  let mut corpus_names: Vec<String> = Vec::new();
  let mut service_names: Vec<String> = Vec::new();
  if let Ok(mut connection) = pool.get() {
    corpus_names = Corpus::all(&mut connection)
      .unwrap_or_default()
      .into_iter()
      .map(|corpus| corpus.name)
      .collect();
    service_names = Service::all(&mut connection)
      .unwrap_or_default()
      .into_iter()
      .map(|service| service.name)
      .collect();
  }
  let global = serde_json::json!({
    "title": "Historical runs",
    "description": "Recent conversion runs across every corpus and service",
  });
  Ok(Template::render(
    "admin-runs",
    context! {
      global, owner: admin.owner, runs, corpus_names, service_names,
      filter_corpus: corpus, filter_service: service, filter_owner: owner,
    },
  ))
}

/// Resolves a `(corpus, service)` name pair to its records, mapping each miss to `404`.
fn resolve(
  corpus: &str,
  service: &str,
  connection: &mut diesel::PgConnection,
) -> Result<(Corpus, Service), Status> {
  let corpus = Corpus::find_by_name(corpus, connection).map_err(|_| Status::NotFound)?;
  let service = Service::find_by_name(service, connection).map_err(|_| Status::NotFound)?;
  Ok((corpus, service))
}

/// Lists the run history of a `(corpus, service)`, most-recent first (the agent twin of the history
/// screen). `404` if the corpus or service is unknown.
#[rocket_okapi::openapi(tag = "Runs")]
#[get("/api/runs/<corpus>/<service>")]
pub fn api_runs(
  corpus: &str,
  service: &str,
  pool: &State<DbPool>,
) -> Result<Json<Vec<RunDto>>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let runs = HistoricalRun::find_by(&corpus, &service, &mut connection).unwrap_or_default();
  // An open run's tallies are frozen only at completion; overlay live progress for any open run.
  Ok(Json(
    runs
      .into_iter()
      .map(|run| RunDto::from(run.with_live_tallies(&mut connection)))
      .collect(),
  ))
}

/// Returns the currently open run of a `(corpus, service)`, or `null` if none is in progress. `404`
/// if the corpus or service is unknown.
#[rocket_okapi::openapi(tag = "Runs")]
#[get("/api/runs/<corpus>/<service>/current")]
pub fn api_run_current(
  corpus: &str,
  service: &str,
  pool: &State<DbPool>,
) -> Result<Json<Option<RunDto>>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let current = HistoricalRun::find_current(&corpus, &service, &mut connection)
    .map_err(|_| Status::InternalServerError)?;
  // The current run is by definition open, so its stored tallies are zero — overlay live progress.
  Ok(Json(current.map(|run| {
    RunDto::from(run.with_live_tallies(&mut connection))
  })))
}

/// One cell of the run-comparison matrix: how many tasks moved from `previous_status` to
/// `current_status` between the two snapshots.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RunDiffTransitionDto {
  /// Severity key in the earlier snapshot (`no_problem`, `warning`, `error`, `fatal`).
  pub previous_status: String,
  /// Severity key in the later snapshot.
  pub current_status: String,
  /// Number of tasks that made this transition.
  pub task_count: usize,
}

/// A comparison of two saved task-status snapshots of a `(corpus, service)`: the status-transition
/// matrix (what improved / regressed between runs) plus the snapshot dates available to compare.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RunDiffDto {
  /// Snapshot dates available for comparison.
  pub available_dates: Vec<String>,
  /// The full previous→current status-transition matrix, with task counts.
  pub transitions: Vec<RunDiffTransitionDto>,
}

/// Parses an optional `YYYY-MM-DD hh:mm:ss[.fff]` snapshot timestamp, mapping a malformed value to
/// `400`. (The legacy HTML diff route `.unwrap()`s here and panics — a dispatch-path panic this
/// twin fixes; see `docs/KNOWN_ISSUES.md`.)
fn parse_snapshot_date(raw: Option<&str>) -> Result<Option<NaiveDateTime>, Status> {
  match raw.map(str::trim).filter(|value| !value.is_empty()) {
    None => Ok(None),
    Some(value) => NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f")
      .map(Some)
      .map_err(|_| Status::BadRequest),
  }
}

/// Parses an optional severity key (`no_problem`/`warning`/`error`/`fatal`/…) into a status filter,
/// mapping a present-but-unknown value to `400`. Absent or empty means "no filter on this side".
fn parse_status(raw: Option<&str>) -> Result<Option<TaskStatus>, Status> {
  match raw.map(str::trim).filter(|value| !value.is_empty()) {
    None => Ok(None),
    Some(value) => TaskStatus::from_key(value)
      .map(Some)
      .ok_or(Status::BadRequest),
  }
}

/// Compares two task-status snapshots of a `(corpus, service)` (the agent twin of the diff-summary
/// screen). `previous`/`current` are snapshot timestamps from `available_dates`; omit them to use
/// the most recent saved pair. `400` on a malformed date, `404` if the corpus/service is unknown.
#[rocket_okapi::openapi(tag = "Runs")]
#[get("/api/runs/<corpus>/<service>/diff?<previous>&<current>")]
pub fn api_run_diff(
  corpus: &str,
  service: &str,
  previous: Option<&str>,
  current: Option<&str>,
  pool: &State<DbPool>,
) -> Result<Json<RunDiffDto>, Status> {
  let previous_date = parse_snapshot_date(previous)?;
  let current_date = parse_snapshot_date(current)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let (available_dates, rows) = summary_task_diffs(
    &mut connection,
    &corpus,
    &service,
    previous_date,
    current_date,
  );
  let transitions = rows
    .into_iter()
    .map(|row| RunDiffTransitionDto {
      previous_status: row.previous_status,
      current_status: row.current_status,
      task_count: row.task_count,
    })
    .collect();
  Ok(Json(RunDiffDto {
    available_dates,
    transitions,
  }))
}

/// A single task's status transition between two snapshots — which document regressed or improved,
/// and when each snapshot was taken.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskDiffDto {
  /// Task identifier.
  pub task_id: String,
  /// Document entry name (trimmed).
  pub entry: String,
  /// Severity key in the earlier snapshot.
  pub previous_status: String,
  /// Severity key in the later snapshot.
  pub current_status: String,
  /// When the earlier snapshot was saved (`YYYY-MM-DD`).
  pub previous_saved_at: String,
  /// When the later snapshot was saved (`YYYY-MM-DD`).
  pub current_saved_at: String,
}

impl From<TaskRunMetadata> for TaskDiffDto {
  fn from(task: TaskRunMetadata) -> TaskDiffDto {
    TaskDiffDto {
      task_id: task.task_id,
      entry: task.entry,
      previous_status: task.previous_status,
      current_status: task.current_status,
      previous_saved_at: task.previous_saved_at,
      current_saved_at: task.current_saved_at,
    }
  }
}

/// Lists the individual tasks whose status changed between two snapshots of a `(corpus, service)`
/// — the drill-down behind the comparison matrix (which documents regressed/improved). Optionally
/// filtered to a `previous_status`/`current_status` transition and paginated (`offset`/`page_size`,
/// default 100). `400` on a malformed date or status, `404` if the corpus/service is unknown.
#[allow(clippy::too_many_arguments)]
#[rocket_okapi::openapi(tag = "Runs")]
#[get(
  "/api/runs/<corpus>/<service>/tasks?<previous>&<current>&<previous_status>&<current_status>&<offset>&<page_size>"
)]
pub fn api_run_task_diffs(
  corpus: &str,
  service: &str,
  previous: Option<&str>,
  current: Option<&str>,
  previous_status: Option<&str>,
  current_status: Option<&str>,
  offset: Option<usize>,
  page_size: Option<usize>,
  pool: &State<DbPool>,
) -> Result<Json<Vec<TaskDiffDto>>, Status> {
  let filters = DiffStatusFilter {
    previous_status: parse_status(previous_status)?,
    current_status: parse_status(current_status)?,
    previous_date: parse_snapshot_date(previous)?,
    current_date: parse_snapshot_date(current)?,
    // Bound both: `?page_size=0` would request an unpaginated diff (R-8) and a huge value would
    // `LIMIT` a whole task-diff set into one response; a deep `offset` is a scan-and-discard (P-4).
    // Same bounds as the report paths.
    offset: offset.unwrap_or(0).min(MAX_REPORT_OFFSET as usize),
    page_size: page_size
      .unwrap_or(100)
      .clamp(1, MAX_REPORT_PAGE_SIZE as usize),
  };
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus, service) = resolve(corpus, service, &mut connection)?;
  let tasks = list_task_diffs(&mut connection, &corpus, &service, filters);
  Ok(Json(tasks.into_iter().map(TaskDiffDto::from).collect()))
}

/// Severity keys a human can filter a task-diff on (the transition endpoints we record snapshots
/// for). Offered as the dropdown options on the task-diff screen.
const DIFF_SEVERITY_KEYS: [&str; 6] =
  ["no_problem", "warning", "error", "fatal", "invalid", "todo"];

/// The human task-diff screen: a server-rendered, filterable table of the individual tasks whose
/// status changed between two snapshots — the 1:1 HTML twin of [`api_run_task_diffs`], sharing
/// [`TaskDiffDto`]. This is the *filter-driven* heart of run management: pick a
/// `previous_status → current_status` transition (and optionally a snapshot pair) and see exactly
/// which documents regressed or improved. This parses gracefully: `400` on a malformed
/// date/status, `404` on an unknown corpus/service, and an empty filter just lists every change.
/// It replaced the legacy `diff-history` binary route, which `.expect()`ed the status params and
/// `.unwrap()`ed the dates — **panicking on the dispatch path** (the F-1 gap, now closed).
#[allow(clippy::too_many_arguments)]
#[get(
  "/runs/<corpus>/<service>/tasks?<previous>&<current>&<previous_status>&<current_status>&<offset>&<page_size>"
)]
pub fn runs_tasks_page(
  corpus: &str,
  service: &str,
  previous: Option<&str>,
  current: Option<&str>,
  previous_status: Option<&str>,
  current_status: Option<&str>,
  offset: Option<usize>,
  page_size: Option<usize>,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  // Parse before touching the DB so bad input fails fast and cheaply (mirrors the agent twin).
  let previous_status_filter = parse_status(previous_status)?;
  let current_status_filter = parse_status(current_status)?;
  let previous_date = parse_snapshot_date(previous)?;
  let current_date = parse_snapshot_date(current)?;
  // Bound the paginate params (R-8: no `page_size=0`; P-4: no huge page / deep `OFFSET` scan).
  let offset = offset.unwrap_or(0).min(MAX_REPORT_OFFSET as usize);
  let page_size = page_size
    .unwrap_or(100)
    .clamp(1, MAX_REPORT_PAGE_SIZE as usize);

  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus_record, service_record) = resolve(corpus, service, &mut connection)?;
  let tasks: Vec<TaskDiffDto> = list_task_diffs(
    &mut connection,
    &corpus_record,
    &service_record,
    DiffStatusFilter {
      previous_status: previous_status_filter,
      current_status: current_status_filter,
      previous_date,
      current_date,
      offset,
      page_size,
    },
  )
  .into_iter()
  .map(TaskDiffDto::from)
  .collect();

  // A full page implies there may be more; any non-zero offset implies a previous page exists.
  let page_len = tasks.len();
  let has_next = page_len == page_size;
  let has_prev = offset > 0;
  // Normalize the selected filter back to canonical keys for the form's `selected` state, so a
  // round-trip preserves the choice (and an unknown-but-accepted alias collapses to its key).
  let selected_previous = previous_status_filter
    .map(|s| s.to_key())
    .unwrap_or_default();
  let selected_current = current_status_filter
    .map(|s| s.to_key())
    .unwrap_or_default();
  let global = serde_json::json!({
    "title": format!("Task changes · {service} / {corpus}"),
    "description": format!("Per-task severity changes of service {service} over corpus {corpus}"),
  });
  Ok(Template::render(
    "runs-tasks",
    context! {
      global,
      corpus,
      service,
      tasks,
      severities: DIFF_SEVERITY_KEYS,
      selected_previous,
      selected_current,
      previous_date: previous.unwrap_or_default(),
      current_date: current.unwrap_or_default(),
      offset: offset as i64,
      page_size: page_size as i64,
      from_offset: offset as i64 + 1,
      to_offset: offset as i64 + page_len as i64,
      prev_offset: offset.saturating_sub(page_size) as i64,
      next_offset: (offset + page_size) as i64,
      has_prev,
      has_next,
    },
  ))
}

/// The human diff-summary screen: the server-rendered status-transition **matrix** between two
/// snapshots — the 1:1 HTML twin of [`api_run_diff`], sharing [`RunDiffTransitionDto`]. A snapshot
/// pair is chosen with two date dropdowns (a JS-free `<form method=get>`); each transition cell
/// links into the [`runs_tasks_page`] drill-down pre-filtered to that `previous → current`
/// transition. Reuses `parse_snapshot_date`, so a malformed date is a `400` and `404` on an unknown
/// corpus/service. It replaced the legacy `diff-summary` binary route, which `.unwrap()`ed the date
/// and **panicked on the dispatch path** (the F-1 gap, now closed).
#[get("/runs/<corpus>/<service>/diff?<previous>&<current>")]
pub fn runs_diff_page(
  corpus: &str,
  service: &str,
  previous: Option<&str>,
  current: Option<&str>,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let previous_date = parse_snapshot_date(previous)?;
  let current_date = parse_snapshot_date(current)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus_record, service_record) = resolve(corpus, service, &mut connection)?;
  let (available_dates, rows) = summary_task_diffs(
    &mut connection,
    &corpus_record,
    &service_record,
    previous_date,
    current_date,
  );
  let transitions: Vec<RunDiffTransitionDto> = rows
    .into_iter()
    .map(|row| RunDiffTransitionDto {
      previous_status: row.previous_status,
      current_status: row.current_status,
      task_count: row.task_count,
    })
    .collect();
  let global = serde_json::json!({
    "title": format!("Run diff · {service} / {corpus}"),
    "description": format!("Status-transition matrix of service {service} over corpus {corpus}"),
  });
  Ok(Template::render(
    "runs-diff",
    context! {
      global,
      corpus,
      service,
      available_dates,
      transitions,
      previous_date: previous.unwrap_or_default(),
      current_date: current.unwrap_or_default(),
    },
  ))
}

/// The human run-history screen: a server-rendered table of the same runs `GET /api/runs/...`
/// returns (the 1:1 HTML twin, sharing [`RunDto`]). `404` if the corpus/service is unknown.
#[get("/runs/<corpus>/<service>")]
pub fn runs_page(corpus: &str, service: &str, pool: &State<DbPool>) -> Result<Template, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus_record, service_record) = resolve(corpus, service, &mut connection)?;
  let runs: Vec<RunDto> = HistoricalRun::find_by(&corpus_record, &service_record, &mut connection)
    .unwrap_or_default()
    .into_iter()
    // Overlay live progress on any still-open run so the table doesn't show it as all-zeros.
    .map(|run| RunDto::from(run.with_live_tallies(&mut connection)))
    .collect();
  // Run-over-run deltas (newest-first, so row `i` compares against the next-older row `i+1`) so the
  // human table answers "how did this run move the conversion tallies" without the reader
  // subtracting consecutive rows. The agent `/api/runs` feed stays raw (agents diff themselves).
  let deltas: Vec<Option<RunDelta>> = (0..runs.len())
    .map(|i| {
      runs
        .get(i + 1)
        .map(|older| RunDelta::between(&runs[i], older))
    })
    .collect();
  let runs: Vec<RunRow> = runs
    .into_iter()
    .zip(deltas)
    .map(|(run, delta)| RunRow { run, delta })
    .collect();
  // `global` carries the title/description the shared `layout` template expects.
  let global = serde_json::json!({
    "title": format!("Run history · {service} / {corpus}"),
    "description": format!("Historical runs of service {service} over corpus {corpus}"),
  });
  Ok(Template::render(
    "runs",
    context! { global, corpus, service, runs },
  ))
}

/// The human run-history **visualization**: the Vega bar-chart of per-run success rates over time
/// plus the tabular breakdown — the chart view that complements the [`runs_page`] table and the
/// structured [`api_runs`] feed. Relocated from the legacy binary route onto the library surface
/// (pooled connection; the legacy serialization `.unwrap()` — a request-path panic — softened to a
/// graceful empty series). `404` if the corpus/service is unknown.
#[get("/history/<corpus>/<service>")]
pub fn history_page(corpus: &str, service: &str, pool: &State<DbPool>) -> Result<Template, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let (corpus_record, service_record) = resolve(corpus, service, &mut connection)?;
  let mut context = TemplateContext::default();
  let mut global = HashMap::new();
  if let Ok(runs) = HistoricalRun::find_by(&corpus_record, &service_record, &mut connection) {
    // Overlay live progress on any open run so the latest chart point reflects real progress.
    let runs_meta: Vec<RunMetadata> = runs
      .into_iter()
      .map(|run| RunMetadata::from(run.with_live_tallies(&mut connection)))
      .collect();
    let runs_meta_stack = RunMetadataStack::transform(&runs_meta);
    // Soften the legacy `.unwrap()` (a request-path panic on a serialization error) to an empty
    // series: the chart renders nothing rather than crashing the request.
    // The result is embedded **inside a `<script>` block** (`{{ history_serialized | safe }}`), and
    // serde_json escapes `"`/control chars but NOT `<` — so a user-set run `description` containing
    // `</script>` would break out of the script tag (stored XSS, and `/history` is public). Escape
    // `<`/`>`/`&` to their JSON `\uXXXX` forms — `JSON.parse` decodes them back, so the chart data
    // is byte-identical while the markup can no longer be escaped.
    let history_json = serde_json::to_string(&runs_meta_stack)
      .unwrap_or_default()
      .replace('<', "\\u003c")
      .replace('>', "\\u003e")
      .replace('&', "\\u0026");
    context.history_serialized = Some(history_json);
    global.insert(
      "history_length".to_string(),
      runs_meta
        .iter()
        .filter(|run| !run.end_time.is_empty())
        .count()
        .to_string(),
    );
    context.history = Some(runs_meta);
  }
  global.insert(
    "title".to_string(),
    format!("Run history · {service} / {corpus}"),
  );
  global.insert(
    "description".to_string(),
    format!("Historical runs of service {service} over corpus {corpus}"),
  );
  global.insert("service_name".to_string(), service.to_string());
  global.insert("corpus_name".to_string(), corpus.to_string());
  context.global = global;
  decorate_uri_encodings(&mut context);
  Ok(Template::render("history", context))
}

/// The route set for the historical-runs capability.
pub fn routes() -> Vec<Route> {
  // NB: the `api_run*` routes (incl. `api_all_runs`) are mounted via `frontend::apidoc`
  // (rocket_okapi).
  routes![
    all_runs_page,
    runs_tasks_page,
    runs_diff_page,
    runs_page,
    history_page
  ]
}

#[cfg(test)]
mod tests {
  use super::delta_class;

  #[test]
  fn delta_class_polarity() {
    // no_problem: more clean conversions is an improvement.
    assert_eq!(delta_class(26, true), "delta-good");
    assert_eq!(delta_class(-5, true), "delta-bad");
    // error/fatal/warning: fewer problems is an improvement.
    assert_eq!(delta_class(3, false), "delta-bad");
    assert_eq!(delta_class(-3, false), "delta-good");
    // unchanged columns are muted, regardless of polarity.
    assert_eq!(delta_class(0, true), "delta-zero");
    assert_eq!(delta_class(0, false), "delta-zero");
  }
}
