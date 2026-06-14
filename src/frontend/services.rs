// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Services capability: the worker-fleet view for a service, as a human screen + agent API.
//!
//! Follows the symmetry contract — the worker-fleet screen (`GET /workers/<service>`) and its agent
//! twin (`GET /api/services/<service>/workers`) live in one module, both pooled (no per-request
//! `Backend::default()`). The agent twin surfaces the per-worker dispatch/return tallies and the
//! in-flight backlog, the operational signal for spotting a stuck or struggling worker (directly
//! useful for watching the hardened dispatcher's fleet).

use std::collections::HashMap;

use rocket::http::Status;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::Template;
use serde::Serialize;

use crate::backend::DbPool;
use crate::frontend::helpers::decorate_uri_encodings;
use crate::frontend::params::TemplateContext;
use crate::models::{Service, WorkerMetadata};

/// A registered service as exposed over the API/UI — the service-registry view. `name` is the
/// stable external handle used by every service route.
#[derive(Debug, Serialize)]
pub struct ServiceDto {
  /// Service name (its external handle); `init`/`import` are the magic internal services.
  pub name: String,
  /// Service version.
  pub version: f32,
  /// Expected input format (e.g. `tex`).
  pub inputformat: String,
  /// Produced output format (e.g. `html`).
  pub outputformat: String,
  /// Prerequisite input-conversion service, if any.
  pub inputconverter: Option<String>,
  /// Whether the service needs more than a document's main textual content.
  pub complex: bool,
  /// Human-readable description.
  pub description: String,
}

impl From<Service> for ServiceDto {
  fn from(service: Service) -> ServiceDto {
    ServiceDto {
      name: service.name,
      version: service.version,
      inputformat: service.inputformat,
      outputformat: service.outputformat,
      inputconverter: service.inputconverter,
      complex: service.complex,
      description: service.description,
    }
  }
}

/// A worker's dispatch/return tallies for a service — the machine-readable fleet-health view.
#[derive(Debug, Serialize)]
pub struct WorkerDto {
  /// Worker identity (usually `hostname:pid`).
  pub name: String,
  /// Tasks ever dispatched to this worker.
  pub total_dispatched: i32,
  /// Tasks ever returned by this worker.
  pub total_returned: i32,
  /// Dispatched-but-not-yet-returned tasks (`dispatched - returned`); a large or growing value
  /// flags a stuck or struggling worker.
  pub in_flight: i32,
  /// The id of the most recent task dispatched to this worker.
  pub last_dispatched_task_id: i64,
  /// The id of the most recent task this worker returned (`None` if it never has).
  pub last_returned_task_id: Option<i64>,
}

impl From<WorkerMetadata> for WorkerDto {
  fn from(worker: WorkerMetadata) -> WorkerDto {
    WorkerDto {
      in_flight: worker.total_dispatched - worker.total_returned,
      name: worker.name,
      total_dispatched: worker.total_dispatched,
      total_returned: worker.total_returned,
      last_dispatched_task_id: worker.last_dispatched_task_id,
      last_returned_task_id: worker.last_returned_task_id,
    }
  }
}

/// Resolves a service name to its record, mapping a miss to `404`.
fn resolve(service: &str, connection: &mut diesel::PgConnection) -> Result<Service, Status> {
  Service::find_by_name(service, connection).map_err(|_| Status::NotFound)
}

/// The service registry (agent twin of the registry screen): every registered service. `503` if the
/// pool is exhausted.
#[get("/api/services")]
pub fn api_services(pool: &State<DbPool>) -> Result<Json<Vec<ServiceDto>>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let services = Service::all(&mut connection).unwrap_or_default();
  Ok(Json(services.into_iter().map(ServiceDto::from).collect()))
}

/// The service-registry screen (HTML twin of [`api_services`]): the table of registered services,
/// each linking to its worker-fleet view. `503` if the pool is exhausted.
#[get("/services")]
pub fn services_page(pool: &State<DbPool>) -> Result<Template, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let services: Vec<HashMap<String, String>> = Service::all(&mut connection)
    .unwrap_or_default()
    .iter()
    .map(Service::to_hash)
    .collect();
  let mut global = HashMap::new();
  global.insert("title".to_string(), "Registered services".to_string());
  global.insert(
    "description".to_string(),
    "All processing services registered with the CorTeX framework".to_string(),
  );
  let mut context = TemplateContext {
    global,
    services: Some(services),
    ..TemplateContext::default()
  };
  decorate_uri_encodings(&mut context);
  Ok(Template::render("service-registry", context))
}

/// The worker-fleet status for a service (agent twin of the workers screen): per-worker dispatch/
/// return tallies and in-flight backlog. `404` if the service is unknown.
#[get("/api/services/<service>/workers")]
pub fn api_service_workers(
  service: &str,
  pool: &State<DbPool>,
) -> Result<Json<Vec<WorkerDto>>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let service = resolve(service, &mut connection)?;
  let workers = service.select_workers(&mut connection).unwrap_or_default();
  Ok(Json(workers.into_iter().map(WorkerDto::from).collect()))
}

/// The worker-fleet screen (HTML twin): the dispatcher's registered workers for a service and their
/// activity. `404` if the service is unknown. Relocated from `bin/frontend.rs` onto the pooled
/// library surface.
#[get("/workers/<service>")]
pub fn worker_report_page(service: &str, pool: &State<DbPool>) -> Result<Template, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let service_record = resolve(service, &mut connection)?;
  let workers: Vec<HashMap<String, String>> = service_record
    .select_workers(&mut connection)
    .unwrap_or_default()
    .into_iter()
    .map(Into::into)
    .collect();
  let mut global = HashMap::new();
  global.insert(
    "title".to_string(),
    format!("Worker report for service {service} "),
  );
  global.insert(
    "description".to_string(),
    format!("Worker report for service {service} as registered by the CorTeX dispatcher"),
  );
  global.insert("service_name".to_string(), service.to_string());
  global.insert(
    "service_description".to_string(),
    service_record.description.clone(),
  );
  let mut context = TemplateContext {
    global,
    workers: Some(workers),
    ..TemplateContext::default()
  };
  decorate_uri_encodings(&mut context);
  Ok(Template::render("workers", context))
}

/// The route set for the services capability (registry + worker-fleet, screens + agent API).
pub fn routes() -> Vec<Route> {
  routes![
    api_services,
    services_page,
    api_service_workers,
    worker_report_page
  ]
}
