#![allow(clippy::extra_unused_lifetimes)]
use std::collections::HashMap;

use diesel::result::Error;
use diesel::*;

use crate::schema::services;
use crate::schema::worker_metadata;

use crate::concerns::CortexInsertable;
use super::worker_metadata::WorkerMetadata;

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

  /// Return a vector of services currently activated on this corpus
  pub fn select_workers(&self, connection: &mut PgConnection) -> Result<Vec<WorkerMetadata>, Error> {
    let workers_query = worker_metadata::table
      .filter(worker_metadata::service_id.eq(self.id))
      .order(worker_metadata::name.asc());
    let workers: Vec<WorkerMetadata> = workers_query.get_results(connection)?;
    Ok(workers)
  }
}
