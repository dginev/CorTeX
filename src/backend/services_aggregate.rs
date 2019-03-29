use diesel::pg::PgConnection;
use diesel::result::Error;
use diesel::*;
use crate::models::{NewTask, Service, Corpus};
use crate::helpers::TaskStatus;
use crate::schema::{services};
use crate::concerns::{CortexInsertable};

pub(crate) fn register_service(connection: &PgConnection, service: &Service, corpus_path: &str) -> Result<(), Error> {
  use crate::schema::tasks::dsl::*;
  let corpus = r#try!(Corpus::find_by_path(corpus_path, connection));
  let todo_raw = TaskStatus::TODO.raw();

  // First, delete existing tasks for this <service, corpus> pair.
  delete(tasks)
    .filter(service_id.eq(service.id))
    .filter(corpus_id.eq(corpus.id))
    .execute(connection)?;

  // TODO: when we want to get completeness, also:
  // - also erase log entries
  // - update dependencies
  let import_service = r#try!(Service::find_by_name("import", connection));
  let entries: Vec<String> = r#try!(tasks
    .filter(service_id.eq(import_service.id))
    .filter(corpus_id.eq(corpus.id))
    .select(entry)
    .load(connection));
  connection.transaction::<(), Error, _>(|| {
    for imported_entry in entries {
      let new_task = NewTask {
        entry: imported_entry,
        service_id: service.id,
        corpus_id: corpus.id,
        status: todo_raw,
      };
      new_task.create(connection)?;
    }
    Ok(())
  })
}

pub(crate) fn extend_service(connection: &PgConnection,  service: &Service, corpus_path: &str) -> Result<(), Error> {
  use crate::schema::tasks::dsl::*;
  let corpus = Corpus::find_by_path(corpus_path, connection)?;
  let todo_raw = TaskStatus::TODO.raw();

  // TODO: when we want to get completeness, also:
  // - update dependencies
  let import_service = r#try!(Service::find_by_name("import", connection));
  let entries: Vec<String> = r#try!(tasks
    .filter(service_id.eq(import_service.id))
    .filter(corpus_id.eq(corpus.id))
    .select(entry)
    .load(connection));
  connection.transaction::<(), Error, _>(|| {
    for imported_entry in entries {
      let new_task = NewTask {
        entry: imported_entry,
        service_id: service.id,
        corpus_id: corpus.id,
        status: todo_raw,
      };
      new_task.create_if_new(connection)?;
    }
    Ok(())
  })
}

pub(crate) fn delete_service_by_name(connection: &PgConnection,  name: &str) -> Result<usize, Error> {
  delete(services::table)
    .filter(services::name.eq(name))
    .execute(connection)
}