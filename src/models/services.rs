use std::collections::HashMap;

use diesel::insert_into;
use diesel::pg::PgConnection;
use diesel::result::Error;
use diesel::*;
use serde::{Deserialize, Serialize};

use super::worker_metadata::WorkerMetadata;
use crate::concerns::CortexInsertable;
use crate::schema::services;
use crate::schema::worker_metadata;

// Services
#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug, Serialize)]
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
#[derive(Insertable, Clone, Debug, Serialize, Deserialize)]
#[table_name = "services"]
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
  fn create(&self, connection: &PgConnection) -> Result<usize, Error> {
    insert_into(services::table)
      .values(self)
      .execute(connection)
  }
}

impl Service {
  /// ORM-like until diesel has a best practice
  pub fn find(service_id: i32, connection: &PgConnection) -> Result<Self, Error> {
    services::table.find(service_id).get_result(connection)
  }
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_name(name_query: &str, connection: &PgConnection) -> Result<Service, Error> {
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
  pub fn select_workers(&self, connection: &PgConnection) -> Result<Vec<WorkerMetadata>, Error> {
    let workers_query = worker_metadata::table
      .filter(worker_metadata::service_id.eq(self.id))
      .order(worker_metadata::name.asc());
    let workers: Vec<WorkerMetadata> = workers_query.get_results(connection)?;
    Ok(workers)
  }

  /// Return all services in the database, ordered by name
  pub fn all(connection: &PgConnection) -> Result<Vec<Service>, Error> {
    services::table.order(services::name.asc()).load(connection)
  }
}
