use std::collections::HashMap;

use crate::schema::{
  historical_tasks, log_errors, log_fatals, log_infos, log_invalids, log_warnings, tasks,
};
use diesel::result::Error;
use diesel::*;

use super::RerunOptions;
use crate::concerns::{CortexInsertable, MarkRerun};
use crate::helpers::{NewTaskMessage, TaskReport, TaskStatus, random_mark};
use crate::models::{
  Corpus, HistoricalRun, LogError, LogFatal, LogInfo, LogInvalid, LogRecord, LogWarning,
  NewHistoricalRun, NewLogError, NewLogFatal, NewLogInfo, NewLogInvalid, NewLogWarning, NewTask,
  Service,
};

pub(crate) fn mark_imported(
  connection: &mut PgConnection,
  imported_tasks: &[NewTask],
) -> Result<usize, Error> {
  // Insert, but only if the task is new (allow for extension calls with the same method)
  insert_into(tasks::table)
    .values(imported_tasks)
    .on_conflict_do_nothing()
    .execute(connection)
}

/// **Pause** a `(corpus, service)` run: transition every **in-progress** task (`status >= 0` — i.e.
/// TODO plus any leased/Queued mark) to **Blocked**, so the ventilator stops handing them out (it
/// only fetches `status = TODO`). Completed tasks (`status < 0`) are left alone. Returns the number
/// paused. The exact inverse of [`resume_blocked`] (Arm 7 run-lifecycle control).
pub(crate) fn mark_blocked(
  connection: &mut PgConnection,
  corpus_id_val: i32,
  service_id_val: i32,
) -> Result<usize, Error> {
  update(tasks::table)
    .filter(tasks::corpus_id.eq(corpus_id_val))
    .filter(tasks::service_id.eq(service_id_val))
    .filter(tasks::status.ge(0))
    .set(tasks::status.eq(TaskStatus::Blocked(-6).raw()))
    .execute(connection)
}

/// **Resume** a paused `(corpus, service)` run: transition every **Blocked** task (`status < -5`)
/// back to **TODO** (`0`), so the ventilator re-leases it on the next fetch. Returns the number
/// resumed. The exact inverse of [`mark_blocked`]; completed and already-in-progress tasks are left
/// alone (Invalid is `-5`, so the `< -5` filter never touches it).
pub(crate) fn resume_blocked(
  connection: &mut PgConnection,
  corpus_id_val: i32,
  service_id_val: i32,
) -> Result<usize, Error> {
  update(tasks::table)
    .filter(tasks::corpus_id.eq(corpus_id_val))
    .filter(tasks::service_id.eq(service_id_val))
    .filter(tasks::status.lt(-5))
    .set(tasks::status.eq(TaskStatus::TODO.raw()))
    .execute(connection)
}

/// **Pause ALL conversions** (the dashboard's global control): block every in-progress task
/// (`status >= 0`) across **every** `(corpus, service)`, fleet-wide, so the ventilator stops
/// leasing new work everywhere. The global twin of [`mark_blocked`]; returns the number paused.
/// Reversible with [`resume_all_blocked`]. (In-flight tasks already dispatched still land their
/// results — pause only stops *new* leasing, exactly like the per-run pause.)
pub(crate) fn mark_all_blocked(connection: &mut PgConnection) -> Result<usize, Error> {
  update(tasks::table)
    .filter(tasks::status.ge(0))
    .set(tasks::status.eq(TaskStatus::Blocked(-6).raw()))
    .execute(connection)
}

/// **Resume ALL conversions**: return every Blocked task (`status < -5`) across every pair to TODO
/// — the exact inverse of [`mark_all_blocked`]. Returns the number resumed.
pub(crate) fn resume_all_blocked(connection: &mut PgConnection) -> Result<usize, Error> {
  update(tasks::table)
    .filter(tasks::status.lt(-5))
    .set(tasks::status.eq(TaskStatus::TODO.raw()))
    .execute(connection)
}

pub(crate) fn mark_done(
  connection: &mut PgConnection,
  reports: &[TaskReport],
) -> Result<(), Error> {
  use crate::schema::tasks::{id, status};

  // Collect the finalized task ids once, to clear their prior logs in a single batched statement
  // per table instead of five deletes *per task*. The done-queue yields distinct task ids per
  // drain, so a batched `task_id = ANY(...)` deletes exactly the same rows as the old per-task
  // loop — far fewer round-trips on the hot finalize path (KNOWN_ISSUES D-8 write-amplification).
  let task_ids: Vec<i64> = reports.iter().map(|report| report.task.id).collect();
  // PostgreSQL caps a single statement at 65535 bind parameters. The finalize path
  // batches across an entire drained burst of reports, so at fleet scale (many papers
  // × many log messages) the per-severity INSERTs and the `eq_any` id lists overflow
  // that cap — which made the whole statement fail (`number of parameters must be
  // between 0 and 65535`), the finalize thread panic, and the dispatcher wedge
  // (observed on a 64-worker run, 2026-06-17). Chunk every batched statement to stay
  // under the cap: `eq_any` binds 1 param per id; a log row binds 4 columns
  // (task_id, category, what, details), so 16k rows ≈ 64k params.
  const ID_CHUNK: usize = 50_000;
  const LOG_INSERT_CHUNK: usize = 16_000;
  connection.transaction::<(), Error, _>(|t_connection| {
    // Clear the prior log messages for every finalized task (one statement per severity
    // table), chunked over the task-id list to respect the bind-parameter cap.
    for ids in task_ids.chunks(ID_CHUNK) {
      delete(log_infos::table.filter(log_infos::task_id.eq_any(ids))).execute(t_connection)?;
      delete(log_warnings::table.filter(log_warnings::task_id.eq_any(ids)))
        .execute(t_connection)?;
      delete(log_errors::table.filter(log_errors::task_id.eq_any(ids))).execute(t_connection)?;
      delete(log_fatals::table.filter(log_fatals::task_id.eq_any(ids))).execute(t_connection)?;
      delete(log_invalids::table.filter(log_invalids::task_id.eq_any(ids)))
        .execute(t_connection)?;
    }
    // Group the finalized task ids by their target status, and partition the new messages by
    // severity table, in one pass — so both the status UPDATEs and the message INSERTs become a
    // handful of batched statements below instead of two-per-task in a loop (the finalize hot
    // path).
    let mut ids_by_status: HashMap<i32, Vec<i64>> = HashMap::new();
    let mut new_infos: Vec<NewLogInfo> = Vec::new();
    let mut new_warnings: Vec<NewLogWarning> = Vec::new();
    let mut new_errors: Vec<NewLogError> = Vec::new();
    let mut new_fatals: Vec<NewLogFatal> = Vec::new();
    let mut new_invalids: Vec<NewLogInvalid> = Vec::new();
    for report in reports.iter() {
      ids_by_status
        .entry(report.status.raw())
        .or_default()
        .push(report.task.id);
      for message in &report.messages {
        // The synthetic conversion-status message is not a real log entry (kept out of the tables).
        if message.severity() == "status" {
          continue;
        }
        match message {
          NewTaskMessage::Info(record) => new_infos.push(record.clone()),
          NewTaskMessage::Warning(record) => new_warnings.push(record.clone()),
          NewTaskMessage::Error(record) => new_errors.push(record.clone()),
          NewTaskMessage::Fatal(record) => new_fatals.push(record.clone()),
          NewTaskMessage::Invalid(record) => new_invalids.push(record.clone()),
        }
      }
      // TODO: Update dependenct services, when integrated in DB
    }
    // Apply the status updates: one batched UPDATE per *distinct* terminal status (a small fixed
    // set — NoProblem/Warning/Error/Fatal/Invalid), each over the disjoint id set that resolved
    // to it.
    for (status_value, status_ids) in &ids_by_status {
      for ids in status_ids.chunks(ID_CHUNK) {
        update(tasks::table)
          .filter(id.eq_any(ids))
          .set(status.eq(*status_value))
          .execute(t_connection)?;
      }
    }
    // Batched INSERT per severity table, chunked to respect the bind-parameter cap.
    // (`chunks` over an empty Vec yields nothing, so no `is_empty` guard is needed.)
    for chunk in new_infos.chunks(LOG_INSERT_CHUNK) {
      insert_into(log_infos::table)
        .values(chunk)
        .execute(t_connection)?;
    }
    for chunk in new_warnings.chunks(LOG_INSERT_CHUNK) {
      insert_into(log_warnings::table)
        .values(chunk)
        .execute(t_connection)?;
    }
    for chunk in new_errors.chunks(LOG_INSERT_CHUNK) {
      insert_into(log_errors::table)
        .values(chunk)
        .execute(t_connection)?;
    }
    for chunk in new_fatals.chunks(LOG_INSERT_CHUNK) {
      insert_into(log_fatals::table)
        .values(chunk)
        .execute(t_connection)?;
    }
    for chunk in new_invalids.chunks(LOG_INSERT_CHUNK) {
      insert_into(log_invalids::table)
        .values(chunk)
        .execute(t_connection)?;
    }
    Ok(())
  })?;
  Ok(())
}

pub(crate) fn mark_rerun<'a>(
  connection: &'a mut PgConnection,
  options: RerunOptions<'a>,
) -> Result<(), Error> {
  let RerunOptions {
    corpus,
    service,
    severity_opt,
    category_opt,
    what_opt,
    owner_opt,
    description_opt,
  } = options;
  use crate::schema::tasks::{corpus_id, service_id, status};
  // We are starting a new run, first catalog the current metadata in our historical records.
  let mut description = description_opt.unwrap_or_else(|| String::from("mark for rerun "));
  // auto-generate a report message from the selected filters
  description.push_str("(filters:");
  if severity_opt.is_none() && category_opt.is_none() && what_opt.is_none() {
    description.push_str(" entire corpus");
  } else {
    if let Some(ref severity) = severity_opt {
      description.push_str(" severity=");
      description.push_str(severity);
    }
    if let Some(ref category) = category_opt {
      description.push_str(" category=");
      description.push_str(category);
    }
    if let Some(ref what) = what_opt {
      description.push_str(" what=");
      description.push_str(what);
    }
  }
  description.push(')');

  // Atomic rerun (R-11): the run record + the two-phase task reset (set the scope to a temporary
  // `mark`, delete its logs, flip `mark → TODO`) must all-or-nothing together. Otherwise a frontend
  // crash/restart mid-rerun could strand the whole scope in the `mark` value — a positive status
  // the dispatcher won't lease (it leases `TODO=0`) — so the tasks never re-convert until a
  // dispatcher restart's limbo recovery. One transaction makes the temporary `mark` never
  // observable and a crash a clean rollback. `mark_new_run`'s own transaction nests here as a
  // savepoint. (The closure param shadows `connection` so the body below uses the transaction
  // connection.)
  connection.transaction::<(), Error, _>(|connection| {
    mark_new_run(
      connection,
      corpus,
      service,
      owner_opt.unwrap_or_else(|| "admin".to_string()),
      description,
    )?;
    // Rerun = set status to TODO for all tasks, deleting old logs
    let mark: i32 = random_mark();

    // First, mark as blocked all of the tasks in the chosen scope, using a special mark
    match severity_opt {
      Some(severity) => match category_opt {
        Some(category) => match what_opt {
          // All tasks in a "what" class
          Some(what) => match severity.to_lowercase().as_str() {
            "warning" => LogWarning::mark_rerun_by_what(
              mark, corpus.id, service.id, &category, &what, connection,
            ),
            "error" => LogError::mark_rerun_by_what(
              mark, corpus.id, service.id, &category, &what, connection,
            ),
            "fatal" => LogFatal::mark_rerun_by_what(
              mark, corpus.id, service.id, &category, &what, connection,
            ),
            "invalid" => LogInvalid::mark_rerun_by_what(
              mark, corpus.id, service.id, &category, &what, connection,
            ),
            _ => {
              LogInfo::mark_rerun_by_what(mark, corpus.id, service.id, &category, &what, connection)
            },
          }?,
          // None: All tasks in a category
          None => match severity.to_lowercase().as_str() {
            "warning" => {
              LogWarning::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection)
            },
            "error" => {
              LogError::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection)
            },
            "fatal" => {
              LogFatal::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection)
            },
            "invalid" => {
              LogInvalid::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection)
            },
            _ => {
              LogInfo::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection)
            },
          }?,
        },
        None => {
          // All tasks in a certain status/severity
          let status_to_rerun: i32 = TaskStatus::from_key(&severity)
            .unwrap_or(TaskStatus::NoProblem)
            .raw();
          update(tasks::table)
            .filter(corpus_id.eq(corpus.id))
            .filter(service_id.eq(service.id))
            .filter(status.eq(status_to_rerun))
            .set(status.eq(mark))
            .execute(connection)?
        },
      },
      None => {
        // Entire corpus
        update(tasks::table)
          .filter(corpus_id.eq(corpus.id))
          .filter(service_id.eq(service.id))
          .filter(status.lt(0))
          .set(status.eq(mark))
          .execute(connection)?
      },
    };

    // Next, delete all logs for the blocked tasks.
    // Note that if we are using a negative blocking status, this query should get sped up via an
    // "Index Scan using log_taskid on logs"
    let affected_tasks = tasks::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .filter(status.eq(mark));
    let affected_tasks_ids = affected_tasks.select(tasks::id);

    let affected_log_infos = log_infos::table.filter(log_infos::task_id.eq_any(affected_tasks_ids));
    delete(affected_log_infos).execute(connection)?;
    let affected_log_warnings =
      log_warnings::table.filter(log_warnings::task_id.eq_any(affected_tasks_ids));
    delete(affected_log_warnings).execute(connection)?;
    let affected_log_errors =
      log_errors::table.filter(log_errors::task_id.eq_any(affected_tasks_ids));
    delete(affected_log_errors).execute(connection)?;
    let affected_log_fatals =
      log_fatals::table.filter(log_fatals::task_id.eq_any(affected_tasks_ids));
    delete(affected_log_fatals).execute(connection)?;
    let affected_log_invalids =
      log_invalids::table.filter(log_invalids::task_id.eq_any(affected_tasks_ids));
    delete(affected_log_invalids).execute(connection)?;

    // Lastly, switch all blocked tasks to TODO, and complete the rerun mark pass.
    update(affected_tasks)
      .set(status.eq(TaskStatus::TODO.raw()))
      .execute(connection)?;

    // The reran scope's reports are now stale (logs deleted, statuses reset). Drop its cached
    // report grains inside the same transaction so the next report view repopulates from the
    // fresh data — scoped to exactly this (corpus, service), never the global cube.
    super::rollup::invalidate_scope(connection, corpus.id, service.id)?;

    Ok(())
  })
}

pub(crate) fn mark_new_run(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
  owner: String,
  description: String,
) -> Result<(), Error> {
  // Step 1. Mark any open runs as completed.
  mark_run_completed(connection, corpus, service)?;
  // Step 2. Create this historical run
  let hrun = NewHistoricalRun {
    corpus_id: corpus.id,
    service_id: service.id,
    description,
    owner,
  };
  hrun.create(connection)?;
  // NB: this used to synchronously `REFRESH MATERIALIZED VIEW report_summary` here so the run
  // boundary showed up in reports immediately — but that is a ~2 min rebuild at production scale,
  // and `mark_new_run` runs on the rerun *request* thread, so it blocked the HTTP response for
  // minutes (KNOWN_ISSUES R-5). The refresh is now spawned **off the request path** by the rerun
  // entry points (`reports::rerun_report`, `concerns::serve_rerun`) via
  // `jobs::spawn_report_refresh`, and the dispatcher refreshes on drain + the regular interval.
  // Bookkeeping no longer triggers a refresh.
  Ok(())
}

fn mark_run_completed(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
) -> Result<(), Error> {
  let to_finish: Vec<HistoricalRun> = HistoricalRun::find_by(corpus, service, connection)?
    .into_iter()
    .filter(|run| run.end_time.is_none())
    .collect();
  if !to_finish.is_empty() {
    connection.transaction::<(), Error, _>(move |t_connection| {
      for run in to_finish.into_iter() {
        run.mark_completed(t_connection)?;
      }
      Ok(())
    })?;
  }
  Ok(())
}

pub fn save_historical_tasks(
  connection: &mut PgConnection,
  corpus: &Corpus,
  service: &Service,
) -> Result<usize, Error> {
  snapshot_tasks(connection, corpus.id, service.id)
}

/// Freeze the current per-task statuses of a `(corpus, service)` into `historical_tasks` — the
/// id-keyed core of [`save_historical_tasks`]. Called both by the human "save snapshot" and, on
/// **run-completion-on-drain**, to capture the just-finished run's outcomes as the **baseline** for
/// the next run's live run-diff (see `backend::Backend::complete_run_if_drained`). One
/// `INSERT … SELECT` of scope-size rows; retention/pruning of stale snapshots is a follow-up.
pub fn snapshot_tasks(
  connection: &mut PgConnection,
  corpus_id: i32,
  service_id: i32,
) -> Result<usize, Error> {
  insert_into(historical_tasks::table)
    .values(
      tasks::table
        .select((tasks::id, tasks::status))
        .filter(tasks::corpus_id.eq(corpus_id))
        .filter(tasks::service_id.eq(service_id)),
    )
    .into_columns((historical_tasks::task_id, historical_tasks::status))
    .execute(connection)
}
