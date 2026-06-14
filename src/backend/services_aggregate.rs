use super::mark;
use crate::concerns::CortexInsertable;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, NewTask, Service};
use diesel::result::Error;
use diesel::*;

pub(crate) fn register_service(
  connection: &mut PgConnection,
  service: &Service,
  corpus_path: &str,
  owner: String,
  description: String,
) -> Result<(), Error> {
  use crate::schema::tasks::dsl::*;
  use crate::schema::{log_errors, log_fatals, log_infos, log_invalids, log_warnings};
  let corpus = Corpus::find_by_path(corpus_path, connection)?;
  let todo_raw = TaskStatus::TODO.raw();
  let service_id_val = service.id;
  let corpus_id_val = corpus.id;

  // The imported documents to (re)activate the service over (the magic `import` service's entries).
  let import_service = Service::find_by_name("import", connection)?;
  let entries: Vec<String> = tasks
    .filter(service_id.eq(import_service.id))
    .filter(corpus_id.eq(corpus.id))
    .select(entry)
    .load(connection)?;

  // (Re)activation atomically clears this <service, corpus> pair's prior tasks **and their `log_*`
  // rows**, then re-creates a TODO task per imported entry. The `log_*` tables have no FK to
  // `tasks` (the only FK is `historical_tasks → tasks ON DELETE CASCADE`), so deleting the tasks
  // alone would orphan their log rows on every re-activation — the same hazard closed in
  // `Corpus::destroy`. One transaction so a crash can't leave the service with its tasks deleted
  // but none re-created.
  connection.transaction::<(), Error, _>(|t_connection| {
    let prior_task_ids = || {
      tasks
        .filter(service_id.eq(service_id_val))
        .filter(corpus_id.eq(corpus_id_val))
        .select(id)
    };
    delete(log_infos::table.filter(log_infos::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(log_warnings::table.filter(log_warnings::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(log_errors::table.filter(log_errors::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(log_fatals::table.filter(log_fatals::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(log_invalids::table.filter(log_invalids::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(tasks)
      .filter(service_id.eq(service_id_val))
      .filter(corpus_id.eq(corpus_id_val))
      .execute(t_connection)?;
    for imported_entry in entries {
      let new_task = NewTask {
        entry: imported_entry,
        service_id: service_id_val,
        corpus_id: corpus_id_val,
        status: todo_raw,
      };
      new_task.create(t_connection)?;
    }
    Ok(())
  })?;
  // Finally, register a new run, completing potentially open ones for this pair, attributed to the
  // actor who activated the service (threaded from the UI/API; the CLI passes a `cli-admin`
  // default).
  mark::mark_new_run(connection, &corpus, service, owner, description)
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

/// Permanently destroys a service by name — its definition **plus all tasks + `log_*` rows across
/// every corpus**, in one transaction (orphan-free + crash-consistent; this *replaces* the latent
/// `delete_service_by_name`, which deleted only the `services` row and orphaned every dependent
/// `tasks`/`log_*` row — R-6). Refuses the magic `init` (1) and `import` (2) services, which are
/// infrastructure (deleting `import` would wipe a corpus's document registry): returns
/// [`Error::QueryBuilderError`] with a descriptive message rather than touching them. `Err` if the
/// service is unknown.
pub(crate) fn destroy_service_by_name(
  connection: &mut PgConnection,
  name: &str,
) -> Result<usize, Error> {
  let service = Service::find_by_name(name, connection)?;
  // Defense-in-depth: the frontend already rejects these with 403, but the data layer must never
  // let a careless caller wipe init/import. (Magic ids: 1=init, 2=import, >2=real services.)
  if service.id <= 2 {
    return Err(Error::QueryBuilderError(
      format!(
        "refusing to destroy the infrastructure service {:?} (id {})",
        name, service.id
      )
      .into(),
    ));
  }
  service.destroy(connection)
}
