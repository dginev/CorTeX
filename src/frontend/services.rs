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

use diesel::prelude::*;
use rocket::form::Form;
use rocket::http::Status;
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::Template;
use serde::{Deserialize, Serialize};

use crate::backend::{DatabaseUrl, DbPool};
use crate::concerns::CortexInsertable;
use crate::frontend::actor::{require_admin_to, Actor, AdminReject, AdminSession, ReturnTo};
use crate::frontend::corpora::start_activate;
use crate::frontend::helpers::decorate_uri_encodings;
use crate::frontend::params::TemplateContext;
use crate::models::{Corpus, NewService, Service, WorkerMetadata};

/// Magic service-id ceiling: ids `1=init` and `2=import` are infrastructure and must never be
/// destroyed (deleting `import` would wipe a corpus's document registry). Mirrors the same guard in
/// [`crate::frontend::corpora`].
const IMPORT_SERVICE_ID: i32 = 2;

/// A registered service as exposed over the API/UI — the service-registry view. `name` is the
/// stable external handle used by every service route.
#[derive(Debug, Serialize, schemars::JsonSchema)]
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
#[derive(Debug, Serialize, schemars::JsonSchema)]
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
  /// Seconds since this worker was last active (the more recent of its last dispatch / last
  /// return) — its **liveness age**. Across a large fleet, a value that keeps climbing flags a
  /// worker gone silent (crashed / disconnected); this is the agent-twin parity of the human
  /// screen's "N ago" + fresh/stale display. `0` if the timestamp is in the future (clock skew),
  /// never negative.
  pub seconds_since_last_active: i64,
  /// Whether the worker has been active within the last minute — the at-a-glance liveness flag
  /// (matches the human screen's `fresh`/`stale` threshold).
  pub fresh: bool,
}

impl From<WorkerMetadata> for WorkerDto {
  fn from(worker: WorkerMetadata) -> WorkerDto {
    // Liveness = time since the most recent activity (a dispatch *or* a return). Skew-safe: a
    // future timestamp yields 0 rather than panicking (cf. `since_string`).
    let last_active = match worker.time_last_return {
      Some(returned) => returned.max(worker.time_last_dispatch),
      None => worker.time_last_dispatch,
    };
    let seconds_since_last_active = std::time::SystemTime::now()
      .duration_since(last_active)
      .map(|elapsed| elapsed.as_secs() as i64)
      .unwrap_or(0);
    WorkerDto {
      in_flight: worker.total_dispatched - worker.total_returned,
      name: worker.name,
      total_dispatched: worker.total_dispatched,
      total_returned: worker.total_returned,
      last_dispatched_task_id: worker.last_dispatched_task_id,
      last_returned_task_id: worker.last_returned_task_id,
      seconds_since_last_active,
      fresh: seconds_since_last_active < 60,
    }
  }
}

/// Resolves a service name to its record, mapping a miss to `404`.
fn resolve(service: &str, connection: &mut diesel::PgConnection) -> Result<Service, Status> {
  Service::find_by_name(service, connection).map_err(|_| Status::NotFound)
}

/// The service registry (agent twin of the registry screen): every registered service. `503` if the
/// pool is exhausted.
#[rocket_okapi::openapi(tag = "Services")]
#[get("/api/services")]
pub fn api_services(pool: &State<DbPool>) -> Result<Json<Vec<ServiceDto>>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let services = Service::all(&mut connection).unwrap_or_default();
  Ok(Json(services.into_iter().map(ServiceDto::from).collect()))
}

/// Inserts a new service definition (`409` if the name is taken). Shared by the agent endpoint and
/// the human form. Normalizes an empty `inputconverter` to `None` (no prerequisite).
fn insert_service(pool: &DbPool, mut service: NewService) -> Result<(), Status> {
  service.inputconverter = service.inputconverter.filter(|s| !s.is_empty());
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  if Service::find_by_name(&service.name, &mut connection).is_ok() {
    return Err(Status::Conflict);
  }
  service
    .create(&mut connection)
    .map_err(|_| Status::InternalServerError)?;
  Ok(())
}

/// Request body for registering (defining) a new service.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ServiceRegisterRequest {
  /// Service name (external handle).
  pub name: String,
  /// Service version (e.g. `0.1`).
  pub version: f32,
  /// Expected input format (e.g. `tex`).
  pub inputformat: String,
  /// Produced output format (e.g. `html`).
  pub outputformat: String,
  /// Prerequisite input-conversion service, if any (empty = none).
  pub inputconverter: Option<String>,
  /// Whether the service needs more than a document's main textual content.
  pub complex: bool,
  /// Optional human-readable description.
  pub description: Option<String>,
}

/// Registers (defines) a new service in the registry — the agent twin of the registry screen's
/// "Register a service" form. **Token-gated** via the [`Actor`] guard; `401` without a valid token,
/// `409` if the service name already exists, `201` with the service on success. (This *defines* a
/// service; activating it on a corpus — creating tasks — is `POST /api/corpora/<c>/services/<s>`.)
#[rocket_okapi::openapi(tag = "Services")]
#[post("/api/services", format = "json", data = "<request>")]
pub fn register_service(
  request: Json<ServiceRegisterRequest>,
  _actor: Actor,
  pool: &State<DbPool>,
) -> Result<(Status, Json<ServiceDto>), Status> {
  let request = request.into_inner();
  let name = request.name.clone();
  insert_service(
    pool,
    NewService {
      name: request.name,
      version: request.version,
      inputformat: request.inputformat,
      outputformat: request.outputformat,
      inputconverter: request.inputconverter,
      complex: request.complex,
      description: request.description.unwrap_or_default(),
    },
  )?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let service =
    Service::find_by_name(&name, &mut connection).map_err(|_| Status::InternalServerError)?;
  Ok((Status::Created, Json(ServiceDto::from(service))))
}

/// Fields of the human "Register a service" form on the service-registry screen.
#[derive(FromForm)]
pub struct RegisterServiceForm {
  /// Service name (external handle).
  pub name: String,
  /// Service version.
  pub version: f32,
  /// Expected input format.
  pub inputformat: String,
  /// Produced output format.
  pub outputformat: String,
  /// Prerequisite input-conversion service, if any.
  pub inputconverter: Option<String>,
  /// Whether the service is complex.
  pub complex: bool,
  /// Optional description.
  pub description: Option<String>,
}

/// The human twin of [`register_service`]: the registry screen's "Register a service" form. **Gated
/// by the signed-in [`AdminSession`] cookie** (the registry screen is itself signed-in-only;
/// anonymous → sign-in), inserts the service, and redirects back to `/services`. `409` if the name
/// is taken.
#[post("/services/register", data = "<form>")]
pub fn register_service_human(
  form: Form<RegisterServiceForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Redirect, Status> {
  if session.is_none() {
    return Ok(Redirect::to("/admin/login"));
  }
  let form = form.into_inner();
  insert_service(
    pool,
    NewService {
      name: form.name,
      version: form.version,
      inputformat: form.inputformat,
      outputformat: form.outputformat,
      inputconverter: form.inputconverter,
      complex: form.complex,
      description: form.description.unwrap_or_default(),
    },
  )?;
  Ok(Redirect::to("/services"))
}

/// The corpus ids a service is already activated on (i.e. has at least one task for) — so the
/// "register on a corpus" screen can **exclude** them from the picker. Re-activating an existing
/// `(service, corpus)` pair is *destructive* (`register_service` wipes & re-creates that pair's
/// tasks + logs), so the UI only ever offers genuinely-new corpora.
fn corpora_activated_on(service_id: i32, connection: &mut diesel::PgConnection) -> Vec<i32> {
  use crate::schema::tasks;
  tasks::table
    .filter(tasks::service_id.eq(service_id))
    .select(tasks::corpus_id)
    .distinct()
    .load(connection)
    .unwrap_or_default()
}

/// Fields of the "Add a service" form: the new-service definition plus the names of the corpora
/// (zero or more checkboxes) to activate it on.
#[derive(FromForm)]
pub struct AddServiceForm {
  /// Service name (external handle).
  pub name: String,
  /// Service version.
  pub version: f32,
  /// Expected input format.
  pub inputformat: String,
  /// Produced output format.
  pub outputformat: String,
  /// Prerequisite input-conversion service, if any.
  pub inputconverter: Option<String>,
  /// Whether the service is complex.
  pub complex: bool,
  /// Optional description.
  pub description: Option<String>,
  /// The corpora to activate the new service on (the checked checkboxes; empty = define only).
  pub corpora: Vec<String>,
}

/// The "Add a service" screen: the full new-service form plus a checkbox list of every registered
/// corpus to activate the freshly-defined service on (zero or more). **Signed-in admins only** (an
/// unauthenticated browser is redirected to the sign-in page). The agent equivalent composes the
/// two documented primitives — `POST /api/services` (define) then `POST
/// /api/corpora/<c>/services/<s>` (activate) per corpus.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/services/new")]
pub fn add_service_page(
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let corpora: Vec<HashMap<String, String>> = Corpus::all(&mut connection)
    .unwrap_or_default()
    .iter()
    .map(Corpus::to_hash)
    .collect();
  let mut global = HashMap::new();
  global.insert("title".to_string(), "Add a service".to_string());
  global.insert(
    "description".to_string(),
    "Define a new processing service and optionally activate it on existing corpora".to_string(),
  );
  let mut context = TemplateContext {
    global,
    corpora: Some(corpora),
    ..TemplateContext::default()
  };
  decorate_uri_encodings(&mut context);
  Ok(Template::render("add-service", context))
}

/// Defines a new service and activates it on each checked corpus — one **background** activation
/// job per corpus (each creates a TODO task per imported document, so a large corpus can run for a
/// while; the operator tracks them on `/jobs`). **Gated by the signed-in [`AdminSession`] cookie**
/// (anonymous → sign-in). Redirects to `/jobs` when any corpus was selected (so the in-flight
/// registrations are immediately visible), else back to `/services`. `409` if the name already
/// exists.
#[post("/services/create", data = "<form>")]
pub fn create_service_human(
  form: Form<AddServiceForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<Redirect, Status> {
  let Some(session) = session else {
    return Ok(Redirect::to("/admin/login"));
  };
  let form = form.into_inner();
  insert_service(
    pool,
    NewService {
      name: form.name.clone(),
      version: form.version,
      inputformat: form.inputformat,
      outputformat: form.outputformat,
      inputconverter: form.inputconverter,
      complex: form.complex,
      description: form.description.unwrap_or_default(),
    },
  )?;
  // Activate on each selected corpus (a blank value defensively skipped). Each spawns its own
  // background `service_activate` job, attributed to the signed-in admin.
  let mut activated_any = false;
  for corpus in form.corpora.iter().filter(|name| !name.is_empty()) {
    start_activate(pool, &database_url.0, &session.owner, corpus, &form.name)?;
    activated_any = true;
  }
  Ok(Redirect::to(if activated_any {
    "/jobs"
  } else {
    "/services"
  }))
}

/// Fields of the per-service "Register on a corpus" form: the single corpus (a `<select>` choice
/// over existing corpora) to activate this already-defined service on.
#[derive(FromForm)]
pub struct ActivateOnCorpusForm {
  /// The corpus to activate this service on.
  pub corpus: String,
}

/// The "register an existing service on a corpus" screen: a `<select>` over the corpora this
/// service is **not yet** activated on (already-activated corpora are excluded — re-activating is
/// destructive). **Signed-in admins only** (anonymous → sign-in). `404` if the service is unknown.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/services/<service>/activate")]
pub fn activate_on_corpus_page(
  service: &str,
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let service_record = resolve(service, &mut connection)?;
  let activated_ids = corpora_activated_on(service_record.id, &mut connection);
  let all_corpora = Corpus::all(&mut connection).unwrap_or_default();
  let available: Vec<HashMap<String, String>> = all_corpora
    .iter()
    .filter(|corpus| !activated_ids.contains(&corpus.id))
    .map(Corpus::to_hash)
    .collect();
  let already: Vec<String> = all_corpora
    .iter()
    .filter(|corpus| activated_ids.contains(&corpus.id))
    .map(|corpus| corpus.name.clone())
    .collect();
  let mut global = HashMap::new();
  global.insert(
    "title".to_string(),
    format!("Register service {service} on a corpus"),
  );
  global.insert(
    "description".to_string(),
    format!("Register the service {service} on an additional corpus"),
  );
  global.insert("service_name".to_string(), service.to_string());
  global.insert("already_activated".to_string(), already.join(", "));
  let mut context = TemplateContext {
    global,
    corpora: Some(available),
    ..TemplateContext::default()
  };
  decorate_uri_encodings(&mut context);
  Ok(Template::render("service-activate", context))
}

/// Activates an existing `service` on the chosen `corpus` — a **background** `service_activate` job
/// (the operator tracks it on `/jobs`). **Gated by the signed-in [`AdminSession`] cookie**
/// (anonymous → sign-in). Redirects to `/jobs`. `404` on an unknown service/corpus. (The agent twin
/// is `POST /api/corpora/<c>/services/<s>`.)
#[post("/services/<service>/activate", data = "<form>")]
pub fn activate_on_corpus_human(
  service: &str,
  form: Form<ActivateOnCorpusForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<Redirect, Status> {
  let Some(session) = session else {
    return Ok(Redirect::to("/admin/login"));
  };
  start_activate(pool, &database_url.0, &session.owner, &form.corpus, service)?;
  Ok(Redirect::to("/jobs"))
}

/// The service-registry screen (HTML twin of [`api_services`]): the table of registered services,
/// each linking to its worker-fleet view. **Signed-in admins only** (an unauthenticated browser is
/// redirected to the sign-in page; the agent twin keeps the token guard). `503` if the pool is
/// exhausted.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/services")]
pub fn services_page(
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
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
#[rocket_okapi::openapi(tag = "Services")]
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
/// activity. **Signed-in admins only** (unauthenticated → sign-in page). `404` if the service is
/// unknown. Relocated from `bin/frontend.rs` onto the pooled library surface.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/workers/<service>")]
pub fn worker_report_page(
  service: &str,
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
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

/// Permanently deletes a service **and all of its tasks + log messages across every corpus** — the
/// destructive twin of [`register_service`], closing the R-6 orphan hazard at the data layer
/// ([`Service::destroy`]). **Token-gated** via the [`Actor`] guard (an unauthenticated wipe must
/// not be possible — `401` without a valid token) and double-guarded: the caller must echo the
/// service name via `?confirm=<service>`. The magic `init`/`import` services are infrastructure and
/// can never be deleted (`403`). Returns `204` on success, `400` if the confirmation doesn't match,
/// `403` for a protected service, `404` if unknown.
#[rocket_okapi::openapi(tag = "Services")]
#[delete("/api/services/<service>?<confirm>")]
pub fn delete_service(
  service: &str,
  confirm: Option<&str>,
  _actor: Actor,
  pool: &State<DbPool>,
) -> Status {
  if confirm != Some(service) {
    return Status::BadRequest;
  }
  let mut connection = match pool.get() {
    Ok(connection) => connection,
    Err(_) => return Status::ServiceUnavailable,
  };
  let service_record = match Service::find_by_name(service, &mut connection) {
    Ok(service) => service,
    Err(_) => return Status::NotFound,
  };
  // The magic init (1) / import (2) services are infrastructure — never destroyable.
  if service_record.id <= IMPORT_SERVICE_ID {
    return Status::Forbidden;
  }
  match service_record.destroy(&mut connection) {
    Ok(_) => Status::NoContent,
    Err(_) => Status::InternalServerError,
  }
}

/// Fields of the human "Delete service" form: the service name echoed as confirmation.
#[derive(FromForm)]
pub struct DeleteServiceForm {
  /// Must equal the service name to confirm the destructive action.
  pub confirm: String,
}

/// The human twin of [`delete_service`]: the registry screen's per-service "Delete" form. **Gated
/// by the signed-in [`AdminSession`] cookie** (anonymous → sign-in) *and* confirmation-gated
/// (echoes the service name), then redirects back to `/services`. `400` if the confirmation doesn't
/// match, `403` for a protected service, `404` if unknown.
#[post("/services/<service>/delete", data = "<form>")]
pub fn delete_service_human(
  service: &str,
  form: Form<DeleteServiceForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Redirect, Status> {
  if session.is_none() {
    return Ok(Redirect::to("/admin/login"));
  }
  if form.confirm != service {
    return Err(Status::BadRequest);
  }
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let service_record =
    Service::find_by_name(service, &mut connection).map_err(|_| Status::NotFound)?;
  // Guard the magic init/import services (see [`delete_service`]).
  if service_record.id <= IMPORT_SERVICE_ID {
    return Err(Status::Forbidden);
  }
  service_record
    .destroy(&mut connection)
    .map_err(|_| Status::InternalServerError)?;
  Ok(Redirect::to("/services"))
}

/// The route set for the services capability (registry + worker-fleet, screens + agent API).
pub fn routes() -> Vec<Route> {
  // NB: `api_services` + `api_service_workers` + `register_service` + `delete_service` are mounted
  // via `frontend::apidoc` (rocket_okapi).
  routes![
    register_service_human,
    add_service_page,
    create_service_human,
    activate_on_corpus_page,
    activate_on_corpus_human,
    services_page,
    worker_report_page,
    delete_service_human
  ]
}
