use diesel::pg::PgConnection;
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
#[table_name = "corpora"]
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
  /// currently a "complex" setup is assumed in the arXiv organization,
  /// and will be imported following the arXiv convention.
  /// This can be revisited in the future.
  pub complex: bool,
  /// a human-readable description of the corpus, maybe allow markdown here?
  pub description: String,
  /// the importable file extension for a canonically organized corpus directory
  pub import_extension: String,
}

impl Corpus {
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_name(name_query: &str, connection: &PgConnection) -> Result<Self, Error> {
    use crate::schema::corpora::name;
    corpora::table.filter(name.eq(name_query)).first(connection)
  }
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_path(path_query: &str, connection: &PgConnection) -> Result<Self, Error> {
    use crate::schema::corpora::path;
    corpora::table.filter(path.eq(path_query)).first(connection)
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
  pub fn select_services(&self, connection: &PgConnection) -> Result<Vec<Service>, Error> {
    use crate::schema::tasks::dsl::{corpus_id, service_id};
    let corpus_service_ids_query = tasks::table
      .select(service_id)
      .distinct()
      .filter(corpus_id.eq(self.id));
    let services_query = services::table.filter(services::id.eq_any(corpus_service_ids_query));
    let services: Vec<Service> = services_query.get_results(connection)?;
    Ok(services)
  }

  /// Deletes a corpus and its dependent tasks from the DB, consuming the object
  pub fn destroy(self, connection: &PgConnection) -> Result<usize, Error> {
    // all tasks for entries of this corpus
    delete(tasks::table)
      .filter(tasks::corpus_id.eq(self.id))
      .execute(connection)?;
    // the init task of this corpus
    delete(tasks::table)
      .filter(tasks::entry.eq(self.path))
      .filter(tasks::service_id.eq(1))
      .execute(connection)?;
    // the corpus registration
    delete(corpora::table)
      .filter(corpora::id.eq(self.id))
      .execute(connection)
  }

  /// Return all corpora in the database, ordered by name
  pub fn all(connection: &PgConnection) -> Result<Vec<Corpus>, Error> {
    corpora::table.order(corpora::name.asc()).load(connection)
  }
}

/// Insertable `Corpus` struct
#[derive(Insertable)]
#[table_name = "corpora"]
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
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(corpora::table).values(self).execute(connection)
  }
}
