use crate::schema::{log_errors, log_fatals, log_infos, log_invalids, log_warnings, tasks};
use diesel::pg::PgConnection;
use diesel::result::Error;
use diesel::*;

use crate::concerns::{CortexInsertable, MarkRerun};
use crate::helpers::{random_mark, TaskReport, TaskStatus};
use crate::models::{
  Corpus, LogError, LogFatal, LogInfo, LogInvalid, LogRecord, LogWarning, NewTask, Service,
};

pub(crate) fn mark_imported(
  connection: &PgConnection,
  imported_tasks: &[NewTask],
) -> Result<usize, Error>
{
  // Insert, but only if the task is new (allow for extension calls with the same method)
  insert_into(tasks::table)
    .values(imported_tasks)
    .on_conflict_do_nothing()
    .execute(connection)
}

pub(crate) fn mark_done(connection: &PgConnection, reports: &[TaskReport]) -> Result<(), Error> {
  use crate::schema::tasks::{id, status};

  connection.transaction::<(), Error, _>(|| {
    for report in reports.iter() {
      // Update the status
      update(tasks::table)
        .filter(id.eq(report.task.id))
        .set(status.eq(report.status.raw()))
        .execute(connection)?;
      // Next, delete all previous log messages for this task.id
      delete(log_infos::table)
        .filter(log_infos::task_id.eq(report.task.id))
        .execute(connection)?;
      delete(log_warnings::table)
        .filter(log_warnings::task_id.eq(report.task.id))
        .execute(connection)?;
      delete(log_errors::table)
        .filter(log_errors::task_id.eq(report.task.id))
        .execute(connection)?;
      delete(log_fatals::table)
        .filter(log_fatals::task_id.eq(report.task.id))
        .execute(connection)?;
      delete(log_invalids::table)
        .filter(log_invalids::task_id.eq(report.task.id))
        .execute(connection)?;
      // Clean slate, so proceed to add the new messages
      for message in &report.messages {
        if message.severity() != "status" {
          message.create(&connection)?;
        }
      }
      // TODO: Update dependenct services, when integrated in DB
    }
    Ok(())
  })?;
  Ok(())
}

pub(crate) fn mark_rerun(
  connection: &PgConnection,
  corpus: &Corpus,
  service: &Service,
  severity_opt: Option<String>,
  category_opt: Option<String>,
  what_opt: Option<String>,
) -> Result<(), Error>
{
  use crate::schema::tasks::{corpus_id, service_id, status};
  // Rerun = set status to TODO for all tasks, deleting old logs
  let mark: i32 = random_mark();

  // First, mark as blocked all of the tasks in the chosen scope, using a special mark
  match severity_opt {
    Some(severity) => match category_opt {
      Some(category) => match what_opt {
        // All tasks in a "what" class
        Some(what) => r#try!(match severity.to_lowercase().as_str() {
          "warning" => LogWarning::mark_rerun_by_what(
            mark, corpus.id, service.id, &category, &what, connection,
          ),
          "error" => {
            LogError::mark_rerun_by_what(mark, corpus.id, service.id, &category, &what, connection,)
          },
          "fatal" => {
            LogFatal::mark_rerun_by_what(mark, corpus.id, service.id, &category, &what, connection,)
          },
          "invalid" => LogInvalid::mark_rerun_by_what(
            mark, corpus.id, service.id, &category, &what, connection,
          ),
          _ => {
            LogInfo::mark_rerun_by_what(mark, corpus.id, service.id, &category, &what, connection,)
          },
        }),
        // None: All tasks in a category
        None => r#try!(match severity.to_lowercase().as_str() {
          "warning" => {
            LogWarning::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection,)
          },
          "error" => {
            LogError::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection,)
          },
          "fatal" => {
            LogFatal::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection,)
          },
          "invalid" => {
            LogInvalid::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection,)
          },
          _ => LogInfo::mark_rerun_by_category(mark, corpus.id, service.id, &category, connection,),
        }),
      },
      None => {
        // All tasks in a certain status/severity
        let status_to_rerun: i32 = TaskStatus::from_key(&severity)
          .unwrap_or(TaskStatus::NoProblem)
          .raw();
        r#try!(update(tasks::table)
          .filter(corpus_id.eq(corpus.id))
          .filter(service_id.eq(service.id))
          .filter(status.eq(status_to_rerun))
          .set(status.eq(mark))
          .execute(connection))
      },
    },
    None => {
      // Entire corpus
      r#try!(update(tasks::table)
        .filter(corpus_id.eq(corpus.id))
        .filter(service_id.eq(service.id))
        .filter(status.lt(0))
        .set(status.eq(mark))
        .execute(connection))
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
  r#try!(delete(affected_log_infos).execute(connection));
  let affected_log_warnings =
    log_warnings::table.filter(log_warnings::task_id.eq_any(affected_tasks_ids));
  r#try!(delete(affected_log_warnings).execute(connection));
  let affected_log_errors =
    log_errors::table.filter(log_errors::task_id.eq_any(affected_tasks_ids));
  r#try!(delete(affected_log_errors).execute(connection));
  let affected_log_fatals =
    log_fatals::table.filter(log_fatals::task_id.eq_any(affected_tasks_ids));
  r#try!(delete(affected_log_fatals).execute(connection));
  let affected_log_invalids =
    log_invalids::table.filter(log_invalids::task_id.eq_any(affected_tasks_ids));
  r#try!(delete(affected_log_invalids).execute(connection));

  // Lastly, switch all blocked tasks to TODO, and complete the rerun mark pass.
  r#try!(update(affected_tasks)
    .set(status.eq(TaskStatus::TODO.raw()))
    .execute(connection));

  Ok(())
}
