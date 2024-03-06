#![allow(clippy::implicit_hasher, clippy::extra_unused_lifetimes)]
use diesel::result::Error;
use diesel::*;

use super::{Corpus, Service};
use crate::concerns::{CortexDeletable, CortexInsertable};
use crate::schema::tasks;

// Tasks

#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug, PartialEq, Eq, QueryableByName)]
#[table_name = "tasks"]
/// A `CorTeX` task, for a given corpus-service pair
pub struct Task {
  /// task primary key, auto-incremented by postgresql
  pub id: i64,
  /// id of the service owning this task
  pub service_id: i32,
  /// id of the corpus hosting this task
  pub corpus_id: i32,
  /// current processing status of this task
  pub status: i32,
  /// entry path on the file system
  pub entry: String,
}

#[derive(Insertable, Debug, Clone)]
#[table_name = "tasks"]
/// A new task, to be inserted into `CorTeX`
pub struct NewTask {
  /// id of the service owning this task
  pub service_id: i32,
  /// id of the corpus hosting this task
  pub corpus_id: i32,
  /// current processing status of this task
  pub status: i32,
  /// entry path on the file system
  pub entry: String,
}

impl CortexInsertable for NewTask {
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(tasks::table).values(self).execute(connection)
  }
}

impl CortexDeletable for Task {
  fn delete_by(&self, connection: &PgConnection, field: &str) -> Result<usize, Error> {
    match field {
      "entry" => self.delete_by_entry(connection),
      "service_id" => self.delete_by_service_id(connection),
      "id" => self.delete_by_id(connection),
      _ => Err(Error::QueryBuilderError(
        format!("unknown Task model field: {field}").into(),
      )),
    }
  }
}
impl Task {
  /// Delete task by entry
  pub fn delete_by_entry(&self, connection: &PgConnection) -> Result<usize, Error> {
    use crate::schema::tasks::dsl::entry;
    delete(tasks::table.filter(entry.eq(&self.entry))).execute(connection)
  }

  /// Delete all tasks matching this task's service id
  pub fn delete_by_service_id(&self, connection: &PgConnection) -> Result<usize, Error> {
    use crate::schema::tasks::dsl::service_id;
    delete(tasks::table.filter(service_id.eq(&self.service_id))).execute(connection)
  }

  /// Delete task by id
  pub fn delete_by_id(&self, connection: &PgConnection) -> Result<usize, Error> {
    use crate::schema::tasks::dsl::id;
    delete(tasks::table.filter(id.eq(self.id))).execute(connection)
  }

  /// Find task by id, error if none
  pub fn find(taskid: i64, connection: &PgConnection) -> Result<Task, Error> {
    tasks::table.find(taskid).first(connection)
  }

  /// Find task by entry, error if none
  pub fn find_by_entry(entry: &str, connection: &PgConnection) -> Result<Task, Error> {
    tasks::table
      .filter(tasks::entry.eq(entry))
      .first(connection)
  }

  /// Find task by name-suffix of an entry, error if none
  pub fn find_by_name(
    name: &str,
    corpus: &Corpus,
    service: &Service,
    connection: &PgConnection,
  ) -> Result<Task, Error> {
    use crate::schema::tasks::dsl::{corpus_id, service_id};
    tasks::table
      .filter(corpus_id.eq(corpus.id))
      .filter(service_id.eq(service.id))
      .filter(tasks::entry.like(&format!("%{name}.zip")))
      .first(connection)
  }
}

impl CortexDeletable for NewTask {
  fn delete_by(&self, connection: &PgConnection, field: &str) -> Result<usize, Error> {
    match field {
      "entry" => self.delete_by_entry(connection),
      "service_id" => self.delete_by_service_id(connection),
      _ => Err(Error::QueryBuilderError(
        format!("unknown Task model field: {field}").into(),
      )),
    }
  }
}

impl NewTask {
  fn delete_by_entry(&self, connection: &PgConnection) -> Result<usize, Error> {
    use crate::schema::tasks::dsl::entry;
    delete(tasks::table.filter(entry.eq(&self.entry))).execute(connection)
  }
  fn delete_by_service_id(&self, connection: &PgConnection) -> Result<usize, Error> {
    use crate::schema::tasks::dsl::service_id;
    delete(tasks::table.filter(service_id.eq(&self.service_id))).execute(connection)
  }
  /// Creates the task unless already present in the DB (entry conflict)
  pub fn create_if_new(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(tasks::table)
      .values(self)
      .on_conflict_do_nothing()
      .execute(connection)
  }
}
