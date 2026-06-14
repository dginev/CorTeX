#![allow(clippy::extra_unused_lifetimes)]
use diesel::result::Error;
use diesel::*;
use serde::Serialize;
use std::collections::HashMap;

use crate::concerns::CortexInsertable;
use crate::schema::corpora;
use crate::schema::services;
use crate::schema::tasks;

use super::services::Service;

// Corpora

#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug, Serialize)]
#[diesel(table_name = corpora)]
/// A minimal description of a document collection. Defined by a name, path and simple/complex file
/// system setup.
pub struct Corpus {
  /// auto-incremented postgres id
  pub id: i32,
  /// file system path to corpus root
  /// (a corpus is held in a single top-level directory)
  pub path: String,
  /// a human-readable name for this corpus
  pub name: String,
  /// are we using multiple files to represent a document entry?
  /// (if unsure, always use "true")
  pub complex: bool,
  /// a human-readable description of the corpus, maybe allow markdown here?
  pub description: String,
}

impl Corpus {
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_name(name_query: &str, connection: &mut PgConnection) -> Result<Self, Error> {
    use crate::schema::corpora::name;
    corpora::table.filter(name.eq(name_query)).first(connection)
  }
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_path(path_query: &str, connection: &mut PgConnection) -> Result<Self, Error> {
    use crate::schema::corpora::path;
    corpora::table.filter(path.eq(path_query)).first(connection)
  }
  /// Returns all registered corpora, ordered by name.
  pub fn all(connection: &mut PgConnection) -> Result<Vec<Self>, Error> {
    corpora::table
      .order(corpora::name.asc())
      .get_results(connection)
  }
  /// Return a hash representation of the corpus, usually for frontend reports
  pub fn to_hash(&self) -> HashMap<String, String> {
    let mut hm = HashMap::new();
    hm.insert("name".to_string(), self.name.clone());
    hm.insert("path".to_string(), self.path.clone());
    hm.insert("description".to_string(), self.description.clone());
    hm
  }

  /// Return a vector of services currently activated on this corpus
  pub fn select_services(&self, connection: &mut PgConnection) -> Result<Vec<Service>, Error> {
    use crate::schema::tasks::dsl::{corpus_id, service_id};
    let corpus_service_ids_query = tasks::table
      .select(service_id)
      .distinct()
      .filter(corpus_id.eq(self.id));
    let services_query = services::table.filter(services::id.eq_any(corpus_service_ids_query));
    let services: Vec<Service> = services_query.get_results(connection)?;
    Ok(services)
  }

  /// Deletes a corpus and **all** its dependent rows — the `log_*` messages, the tasks, and the
  /// corpus registration — consuming the object. Runs in a single transaction so a crash mid-delete
  /// can't leave a half-deleted corpus (crash-consistency, `docs/DESIGN_PRINCIPLES.md`).
  ///
  /// The `log_*` tables have **no** foreign key to `tasks` (the only FK is
  /// `historical_tasks.task_id → tasks ON DELETE CASCADE`), so their rows must be deleted
  /// explicitly **before** the tasks or they orphan — this is why deletion lives in one complete
  /// primitive rather than a bare `DELETE FROM corpora` (the CLAUDE.md "deleting a corpus orphans
  /// log_* rows" hazard, now closed at the source so every caller is safe).
  pub fn destroy(self, connection: &mut PgConnection) -> Result<usize, Error> {
    use crate::schema::{log_errors, log_fatals, log_infos, log_invalids, log_warnings};
    let corpus_id = self.id;
    let corpus_path = self.path;
    connection.transaction(|t_connection| {
      // The task ids of this corpus, rebuilt per delete (the subquery is consumed by `eq_any`).
      let task_ids = || {
        tasks::table
          .filter(tasks::corpus_id.eq(corpus_id))
          .select(tasks::id)
      };
      delete(log_infos::table.filter(log_infos::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      delete(log_warnings::table.filter(log_warnings::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      delete(log_errors::table.filter(log_errors::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      delete(log_fatals::table.filter(log_fatals::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      delete(log_invalids::table.filter(log_invalids::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      // all tasks for entries of this corpus (cascades to historical_tasks via its FK)
      delete(tasks::table)
        .filter(tasks::corpus_id.eq(corpus_id))
        .execute(t_connection)?;
      // the init task of this corpus
      delete(tasks::table)
        .filter(tasks::entry.eq(corpus_path))
        .filter(tasks::service_id.eq(1))
        .execute(t_connection)?;
      // the corpus registration
      delete(corpora::table)
        .filter(corpora::id.eq(corpus_id))
        .execute(t_connection)
    })
  }
}

/// Insertable `Corpus` struct
#[derive(Insertable)]
#[diesel(table_name = corpora)]
pub struct NewCorpus {
  /// file system path to corpus root
  /// (a corpus is held in a single top-level directory)
  pub path: String,
  /// a human-readable name for this corpus
  pub name: String,
  /// are we using multiple files to represent a document entry?
  /// (if unsure, always use "true")
  pub complex: bool,
  /// frontend-facing description of the corpus, maybe allow markdown here?
  pub description: String,
}
impl Default for NewCorpus {
  fn default() -> Self {
    NewCorpus {
      name: "mock corpus".to_string(),
      path: ".".to_string(),
      complex: true,
      description: String::new(),
    }
  }
}
impl CortexInsertable for NewCorpus {
  fn create(&self, connection: &mut PgConnection) -> Result<usize, Error> {
    insert_into(corpora::table).values(self).execute(connection)
  }
}
