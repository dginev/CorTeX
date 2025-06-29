use crate::schema::{
  historical_tasks, log_errors, log_fatals, log_infos, log_invalids, log_warnings, tasks,
};
use diesel::result::Error;
use diesel::*;

use super::RerunOptions;
use crate::concerns::{CortexInsertable, MarkRerun};
use crate::helpers::{random_mark, TaskReport, TaskStatus};
use crate::models::{
  Corpus, HistoricalRun, LogError, LogFatal, LogInfo, LogInvalid, LogRecord, LogWarning,
  NewHistoricalRun, NewTask, Service,
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

pub(crate) fn mark_done(
  connection: &mut PgConnection,
  reports: &[TaskReport],
) -> Result<(), Error> {
  use crate::schema::tasks::{id, status};

  connection.transaction::<(), Error, _>(|t_connection| {
    for report in reports.iter() {
      // Update the status
      update(tasks::table)
        .filter(id.eq(report.task.id))
        .set(status.eq(report.status.raw()))
        .execute(t_connection)?;
      // Next, delete all previous log messages for this task.id
      delete(log_infos::table)
        .filter(log_infos::task_id.eq(report.task.id))
        .execute(t_connection)?;
      delete(log_warnings::table)
        .filter(log_warnings::task_id.eq(report.task.id))
        .execute(t_connection)?;
      delete(log_errors::table)
        .filter(log_errors::task_id.eq(report.task.id))
        .execute(t_connection)?;
      delete(log_fatals::table)
        .filter(log_fatals::task_id.eq(report.task.id))
        .execute(t_connection)?;
      delete(log_invalids::table)
        .filter(log_invalids::task_id.eq(report.task.id))
        .execute(t_connection)?;
      // Clean slate, so proceed to add the new messages
      for message in &report.messages {
        if message.severity() != "status" {
          message.create(t_connection)?;
        }
      }
      // TODO: Update dependenct services, when integrated in DB
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
          "error" => {
            LogError::mark_rerun_by_what(mark, corpus.id, service.id, &category, &what, connection)
          },
          "fatal" => {
            LogFatal::mark_rerun_by_what(mark, corpus.id, service.id, &category, &what, connection)
          },
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
          _ => LogInfo::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection),
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

  Ok(())
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
  //  create batch copy query
  let insert_query = insert_into(historical_tasks::table)
    .values(
      tasks::table
        .select((tasks::id, tasks::status))
        .filter(tasks::corpus_id.eq(corpus.id))
        .filter(tasks::service_id.eq(service.id)),
    )
    .into_columns((historical_tasks::task_id, historical_tasks::status));
  insert_query.execute(connection)
}
