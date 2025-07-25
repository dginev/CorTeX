use super::mark;
use crate::concerns::CortexInsertable;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, NewTask, Service};
use crate::schema::services;
use diesel::result::Error;
use diesel::*;

pub(crate) fn register_service(
  connection: &mut PgConnection,
  service: &Service,
  corpus_path: &str,
) -> Result<(), Error> {
  use crate::schema::tasks::dsl::*;
  let corpus = Corpus::find_by_path(corpus_path, connection)?;
  let todo_raw = TaskStatus::TODO.raw();

  // First, delete existing tasks for this <service, corpus> pair.
  delete(tasks)
    .filter(service_id.eq(service.id))
    .filter(corpus_id.eq(corpus.id))
    .execute(connection)?;

  // TODO: when we want to get completeness, also:
  // - also erase log entries
  // - update dependencies
  let import_service = Service::find_by_name("import", connection)?;
  let entries: Vec<String> = tasks
    .filter(service_id.eq(import_service.id))
    .filter(corpus_id.eq(corpus.id))
    .select(entry)
    .load(connection)?;
  connection.transaction::<(), Error, _>(|t_connection| {
    for imported_entry in entries {
      let new_task = NewTask {
        entry: imported_entry,
        service_id: service.id,
        corpus_id: corpus.id,
        status: todo_raw,
      };
      new_task.create(t_connection)?;
    }
    Ok(())
  })?;
  // Finally, register a new run, completing potentially open ones for this pair
  // TODO: When we add register service capacity for the UI, extend this with owner+description
  // information
  mark::mark_new_run(
    connection,
    &corpus,
    service,
    "cli-admin".to_string(),
    "Newly registered service, initial run.".to_string(),
  )
}

pub(crate) fn extend_service(
  connection: &mut PgConnection,
  service: &Service,
  corpus_path: &str,
) -> Result<(), Error> {
  use crate::schema::tasks::dsl::*;
  let corpus = Corpus::find_by_path(corpus_path, connection)?;
  let todo_raw = TaskStatus::TODO.raw();

  // TODO: when we want to get completeness, also:
  // - update dependencies
  let import_service = Service::find_by_name("import", connection)?;
  // TODO: performance can be improved with a convention here.
  // when inserting a new task in the import service, use "TODO" (0) severity
  // when this extension function succeeds, update severity to success (-1)
  // Currently we try to reinsert all imported tasks, which is wasteful.
  let entries: Vec<String> = tasks
    .filter(service_id.eq(import_service.id))
    .filter(corpus_id.eq(corpus.id))
    .select(entry)
    .load(connection)?;
  connection.transaction::<(), Error, _>(|t_connection| {
    for imported_entry in entries {
      let new_task = NewTask {
        entry: imported_entry,
        service_id: service.id,
        corpus_id: corpus.id,
        status: todo_raw,
      };
      new_task.create_if_new(t_connection)?;
    }
    Ok(())
  })
}

pub(crate) fn delete_service_by_name(
  connection: &mut PgConnection,
  name: &str,
) -> Result<usize, Error> {
  delete(services::table)
    .filter(services::name.eq(name))
    .execute(connection)
}
