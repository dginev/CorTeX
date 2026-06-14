#![allow(clippy::extra_unused_lifetimes)]
use std::collections::HashMap;

use diesel::result::Error;
use diesel::*;

use crate::schema::services;
use crate::schema::worker_metadata;

use super::worker_metadata::WorkerMetadata;
use crate::concerns::CortexInsertable;

// Services
#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug)]
/// A `CorTeX` processing service
pub struct Service {
  /// auto-incremented postgres id
  pub id: i32,
  /// a human-readable name
  pub name: String,
  /// a floating-point number to mark the current version (e.g. 0.01)
  pub version: f32,
  /// the expected input format (e.g. tex)
  pub inputformat: String,
  /// the produced output format (e.g. html)
  pub outputformat: String,
  // pub xpath : String,
  // pub resource : String,
  /// prerequisite input conversion service, if any
  pub inputconverter: Option<String>,
  /// is this service requiring more than the main textual content of a document?
  /// mark "true" if unsure
  pub complex: bool,
  /// a human-readable description
  pub description: String,
}
/// Insertable struct for `Service`
#[derive(Insertable, Clone, Debug)]
#[diesel(table_name = services)]
pub struct NewService {
  /// a human-readable name
  pub name: String,
  /// a floating-point number to mark the current version (e.g. 0.01)
  pub version: f32,
  /// the expected input format (e.g. tex)
  pub inputformat: String,
  /// the produced output format (e.g. html)
  pub outputformat: String,
  // pub xpath : String,
  // pub resource : String,
  /// prerequisite input conversion service, if any
  pub inputconverter: Option<String>,
  /// is this service requiring more than the main textual content of a document?
  /// mark "true" if unsure
  pub complex: bool,
  /// a human-readable description
  pub description: String,
}
impl CortexInsertable for NewService {
  fn create(&self, connection: &mut PgConnection) -> Result<usize, Error> {
    insert_into(services::table)
      .values(self)
      .execute(connection)
  }
}

impl Service {
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_name(name_query: &str, connection: &mut PgConnection) -> Result<Service, Error> {
    use crate::schema::services::name;
    services::table
      .filter(name.eq(name_query))
      .get_result(connection)
  }

  /// Returns all registered services, ordered by name (the service registry). Includes the magic
  /// `init` (id 1) and `import` (id 2) services alongside the real conversion services (id > 2).
  pub fn all(connection: &mut PgConnection) -> Result<Vec<Self>, Error> {
    services::table
      .order(services::name.asc())
      .get_results(connection)
  }

  /// Returns a hash representation of the `Service`, usually for frontend reports
  pub fn to_hash(&self) -> HashMap<String, String> {
    let mut hm = HashMap::new();
    hm.insert("id".to_string(), self.id.to_string());
    hm.insert("name".to_string(), self.name.clone());
    hm.insert("description".to_string(), self.description.clone());
    hm.insert("version".to_string(), self.version.to_string());
    hm.insert("inputformat".to_string(), self.inputformat.clone());
    hm.insert("outputformat".to_string(), self.outputformat.clone());
    hm.insert(
      "inputconverter".to_string(),
      match self.inputconverter.clone() {
        Some(ic) => ic,
        None => "None".to_string(),
      },
    );
    hm.insert("complex".to_string(), self.complex.to_string());
    hm
  }

  /// Return the dispatcher's registered workers for this service
  pub fn select_workers(
    &self,
    connection: &mut PgConnection,
  ) -> Result<Vec<WorkerMetadata>, Error> {
    let workers_query = worker_metadata::table
      .filter(worker_metadata::service_id.eq(self.id))
      .order(worker_metadata::name.asc());
    let workers: Vec<WorkerMetadata> = workers_query.get_results(connection)?;
    Ok(workers)
  }

  /// Deactivates (retires) this service from a single corpus: deletes the `(corpus, service)`
  /// pair's tasks **and their `log_*` rows** in one transaction, returning the number of tasks
  /// removed. The service *definition* and its work on other corpora are untouched. The `log_*`
  /// tables have no FK to `tasks`, so their rows are deleted explicitly **before** the tasks or
  /// they orphan (the same hazard closed in [`super::Corpus::destroy`]); transactional so a crash
  /// can't half-delete. `historical_runs` tallies survive (no FK to tasks), while per-task
  /// `historical_tasks` snapshots cascade away with the tasks — the same semantics as deleting a
  /// corpus.
  pub fn deactivate_from_corpus(
    &self,
    corpus: &super::Corpus,
    connection: &mut PgConnection,
  ) -> Result<usize, Error> {
    use crate::schema::tasks;
    use crate::schema::{log_errors, log_fatals, log_infos, log_invalids, log_warnings};
    let service_id_val = self.id;
    let corpus_id_val = corpus.id;
    connection.transaction(|t_connection| {
      let pair_task_ids = || {
        tasks::table
          .filter(tasks::service_id.eq(service_id_val))
          .filter(tasks::corpus_id.eq(corpus_id_val))
          .select(tasks::id)
      };
      delete(log_infos::table.filter(log_infos::task_id.eq_any(pair_task_ids())))
        .execute(t_connection)?;
      delete(log_warnings::table.filter(log_warnings::task_id.eq_any(pair_task_ids())))
        .execute(t_connection)?;
      delete(log_errors::table.filter(log_errors::task_id.eq_any(pair_task_ids())))
        .execute(t_connection)?;
      delete(log_fatals::table.filter(log_fatals::task_id.eq_any(pair_task_ids())))
        .execute(t_connection)?;
      delete(log_invalids::table.filter(log_invalids::task_id.eq_any(pair_task_ids())))
        .execute(t_connection)?;
      delete(
        tasks::table
          .filter(tasks::service_id.eq(service_id_val))
          .filter(tasks::corpus_id.eq(corpus_id_val)),
      )
      .execute(t_connection)
    })
  }
}
