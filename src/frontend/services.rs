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
use diesel::sql_query;
use rocket::form::Form;
use rocket::http::Status;
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::Template;
use serde::{Deserialize, Serialize};

use crate::backend::{DatabaseUrl, DbPool};
use crate::concerns::CortexInsertable;
use crate::frontend::actor::{Actor, AdminReject, AdminSession, ReturnTo, require_admin_to};
use crate::frontend::corpora::start_activate;
use crate::frontend::helpers::{decorate_uri_encodings, group_thousands};
use crate::frontend::params::{MAX_REPORT_OFFSET, MAX_REPORT_PAGE_SIZE, TemplateContext};
use crate::models::{Corpus, NewService, Service, WorkerMetadata};

/// Magic service-id ceiling: ids `1=init` and `2=import` are infrastructure and must never be
/// destroyed (deleting `import` would wipe a corpus's document registry). Mirrors the same guard in
/// [`crate::frontend::corpora`].
const IMPORT_SERVICE_ID: i32 = 2;

/// A registered service as exposed over the API/UI — the service-registry view. `name` is the
/// stable external handle used by every service route.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ServiceDto {
  /// Stable external handle (UUIDv7) — survives a rename, unlike `name`. Use for durable
  /// references.
  pub public_id: String,
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
  /// Per-service lease / visibility-timeout override in seconds (D-17), or `null` to use the
  /// global `dispatcher.lease_timeout_seconds`. Set via `PUT /api/services/<service>/lease`.
  pub lease_timeout_seconds: Option<i32>,
}

impl From<Service> for ServiceDto {
  fn from(service: Service) -> ServiceDto {
    ServiceDto {
      public_id: service.public_id.to_string(),
      name: service.name,
      version: service.version,
      inputformat: service.inputformat,
      outputformat: service.outputformat,
      inputconverter: service.inputconverter,
      complex: service.complex,
      description: service.description,
      lease_timeout_seconds: service.lease_timeout_seconds,
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
pub fn api_services(_caller: Actor, pool: &State<DbPool>) -> Result<Json<Vec<ServiceDto>>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let services = Service::all(&mut connection).unwrap_or_default();
  Ok(Json(services.into_iter().map(ServiceDto::from).collect()))
}

/// Inserts a new service definition (`409` if the name is taken). Shared by the agent endpoint and
/// the human form. Normalizes an empty `inputconverter` to `None` (no prerequisite).
fn insert_service(pool: &DbPool, mut service: NewService) -> Result<(), Status> {
  // Reject a blank name on the agent path too (the HTML form enforces `required`) — a service with
  // an empty handle is unreachable by every name-keyed route.
  if service.name.trim().is_empty() {
    return Err(Status::BadRequest);
  }
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

/// Registers (defines) a new service in the registry — the agent twin of the "Add a service" screen
/// (`/services/new`, `create_service_human`). **Token-gated** via the [`Actor`] guard; `401`
/// without a valid token, `409` if the service name already exists, `201` with the service on
/// success. (This *defines* a service; activating it on a corpus — creating tasks — is `POST
/// /api/corpora/<c>/services/<s>`.)
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

/// Request body for setting a service's per-service lease timeout (D-17).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LeaseUpdateRequest {
  /// New lease / visibility-timeout in seconds (must be positive), or `null` to clear the override
  /// and fall back to the global `dispatcher.lease_timeout_seconds`.
  pub seconds: Option<i32>,
}

/// Rejects a non-positive lease (`Some(<= 0)`); `None` (clear) and any positive value pass. Shared
/// by the agent and human lease setters.
fn valid_lease(seconds: Option<i32>) -> bool { !matches!(seconds, Some(value) if value <= 0) }

/// Sets (or clears) a service's per-service lease / visibility timeout — the agent twin of the
/// registry screen's inline "lease" form (D-17). **Token-gated** via the [`Actor`] guard (`401`
/// without a valid token); `400` if `seconds` is non-positive, `404` if the service is unknown,
/// `200` with the updated [`ServiceDto`] on success. `null` clears the override (the service falls
/// back to the global dispatcher lease). Takes effect on the next dispatch — an already-leased task
/// keeps the timeout captured when it was leased.
#[rocket_okapi::openapi(tag = "Services")]
#[put("/api/services/<service>/lease", format = "json", data = "<request>")]
pub fn set_service_lease(
  service: &str,
  request: Json<LeaseUpdateRequest>,
  _actor: Actor,
  pool: &State<DbPool>,
) -> Result<Json<ServiceDto>, Status> {
  let seconds = request.into_inner().seconds;
  if !valid_lease(seconds) {
    return Err(Status::BadRequest);
  }
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let service_record = resolve(service, &mut connection)?;
  service_record
    .set_lease_timeout(seconds, &mut connection)
    .map_err(|_| Status::InternalServerError)?;
  let updated = resolve(service, &mut connection)?;
  Ok(Json(ServiceDto::from(updated)))
}

/// Fields of the registry screen's inline "set lease" form. A blank value parses to `None` (clear).
#[derive(FromForm)]
pub struct SetLeaseForm {
  /// New lease in seconds; blank clears the per-service override (the global default applies).
  pub seconds: Option<i32>,
}

/// The human twin of [`set_service_lease`]: the registry screen's inline per-service lease form.
/// **Gated by the signed-in [`AdminSession`] cookie** (anonymous → sign-in). A blank value clears
/// the override; a non-positive value is `400`. Redirects back to `/services`; `404` if unknown.
#[post("/services/<service>/lease", data = "<form>")]
pub fn set_service_lease_human(
  service: &str,
  form: Form<SetLeaseForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Redirect, Status> {
  if session.is_none() {
    return Ok(Redirect::to("/admin/login"));
  }
  let seconds = form.into_inner().seconds;
  if !valid_lease(seconds) {
    return Err(Status::BadRequest);
  }
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let service_record =
    Service::find_by_name(service, &mut connection).map_err(|_| Status::NotFound)?;
  service_record
    .set_lease_timeout(seconds, &mut connection)
    .map_err(|_| Status::InternalServerError)?;
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
/// Renders the "Add a service" form. Shared by the GET page and the POST error path, so a failed
/// submit (e.g. a name collision) re-renders with a friendly `error` and every typed value
/// preserved — including which corpora were checked — instead of a bare error page. `svc_*` keys
/// carry the form values (the page meta `title`/`description` are separate). Admin-only page
/// (`is_admin`).
#[allow(clippy::too_many_arguments)]
fn render_add_service(
  pool: &DbPool,
  error: Option<&str>,
  name: &str,
  version: &str,
  inputformat: &str,
  outputformat: &str,
  inputconverter: &str,
  description: &str,
  complex: bool,
  selected: &[String],
) -> Template {
  let mut corpora: Vec<HashMap<String, String>> = match pool.get() {
    Ok(mut connection) => Corpus::all(&mut connection)
      .unwrap_or_default()
      .iter()
      .map(Corpus::to_hash)
      .collect(),
    Err(_) => Vec::new(),
  };
  // Mark every corpus checked/unchecked (the key must always exist so the template's
  // `corpus.checked` lookup never errors), re-checking the ones the admin had selected.
  for corpus in &mut corpora {
    let is_selected = corpus
      .get("name")
      .is_some_and(|cname| selected.iter().any(|s| s == cname));
    corpus.insert("checked".to_string(), is_selected.to_string());
  }
  let mut global = HashMap::new();
  global.insert("title".to_string(), "Add a service".to_string());
  global.insert(
    "description".to_string(),
    "Define a new processing service and optionally activate it on existing corpora".to_string(),
  );
  if let Some(message) = error {
    global.insert("error".to_string(), message.to_string());
  }
  global.insert("svc_name".to_string(), name.to_string());
  global.insert("svc_version".to_string(), version.to_string());
  global.insert("svc_inputformat".to_string(), inputformat.to_string());
  global.insert("svc_outputformat".to_string(), outputformat.to_string());
  global.insert("svc_inputconverter".to_string(), inputconverter.to_string());
  global.insert("svc_description".to_string(), description.to_string());
  global.insert("svc_complex".to_string(), complex.to_string());
  let mut context = TemplateContext {
    global,
    corpora: Some(corpora),
    is_admin: true,
    ..TemplateContext::default()
  };
  decorate_uri_encodings(&mut context);
  Template::render("add-service", context)
}

/// The "Add a service" screen (`GET /services/new`): the full new-service form + a checkbox list of
/// corpora to activate it on. **Signed-in admins only** (anonymous → sign-in). The agent equivalent
/// composes `POST /api/services` (define) + `POST /api/corpora/<c>/services/<s>` (activate) per
/// corpus.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/services/new")]
pub fn add_service_page(
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
  // Sensible defaults for a fresh form.
  Ok(render_add_service(
    pool,
    None,
    "",
    "0.1",
    "tex",
    "html",
    "",
    "",
    true,
    &[],
  ))
}

/// Defines a new service and activates it on each checked corpus — one **background** activation
/// job per corpus (each creates a TODO task per imported document, so a large corpus can run for a
/// while; the operator tracks them on `/jobs`). **Gated by the signed-in [`AdminSession`] cookie**
/// (anonymous → sign-in). Redirects to `/jobs` when any corpus was selected (so the in-flight
/// registrations are immediately visible), else back to `/services`. `409` if the name already
/// exists.
// The Err variant is a re-rendered form `Template` (the friendly-error path), which is chunky —
// fine for a one-shot request handler.
#[allow(clippy::result_large_err)]
#[post("/services/create", data = "<form>")]
pub fn create_service_human(
  form: Form<AddServiceForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<Redirect, Template> {
  let Some(session) = session else {
    return Ok(Redirect::to("/admin/login"));
  };
  let form = form.into_inner();
  if let Err(status) = insert_service(
    pool,
    NewService {
      name: form.name.clone(),
      version: form.version,
      inputformat: form.inputformat.clone(),
      outputformat: form.outputformat.clone(),
      inputconverter: form.inputconverter.clone(),
      complex: form.complex,
      description: form.description.clone().unwrap_or_default(),
    },
  ) {
    // A failed definition re-renders the form with a friendly message + every value preserved
    // (including the checked corpora), rather than a bare error page that loses the admin's input.
    let message = match status.code {
      409 => format!(
        "A service named “{}” already exists — choose a different name.",
        form.name
      ),
      503 => "The database is temporarily unavailable — please try again.".to_string(),
      _ => "Could not define the service — check the values and try again.".to_string(),
    };
    return Err(render_add_service(
      pool,
      Some(&message),
      &form.name,
      &form.version.to_string(),
      &form.inputformat,
      &form.outputformat,
      form.inputconverter.as_deref().unwrap_or(""),
      form.description.as_deref().unwrap_or(""),
      form.complex,
      &form.corpora,
    ));
  }
  // The service is defined; activate it on each selected corpus (a blank value defensively
  // skipped). Each spawns its own background `service_activate` job, attributed to the signed-in
  // admin. Best-effort: a failed activation on one corpus doesn't undo the (already-created)
  // service.
  let mut activated_any = false;
  for corpus in form.corpora.iter().filter(|name| !name.is_empty()) {
    if start_activate(pool, &database_url.0, &session.owner, corpus, &form.name).is_ok() {
      activated_any = true;
    }
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
  let uuid = start_activate(pool, &database_url.0, &session.owner, &form.corpus, service)?;
  Ok(Redirect::to(format!("/jobs/{uuid}")))
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
  _caller: Actor,
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
  let worker_records = service_record
    .select_workers(&mut connection)
    .unwrap_or_default();
  // Fleet-health summary so a ~200-worker fleet reads at a glance instead of by scanning every row.
  let fleet_total = worker_records.len();
  let fleet_fresh = worker_records.iter().filter(|w| w.is_fresh()).count();
  let fleet_dispatched: i64 = worker_records
    .iter()
    .map(|w| i64::from(w.total_dispatched))
    .sum();
  let fleet_returned: i64 = worker_records
    .iter()
    .map(|w| i64::from(w.total_returned))
    .sum();
  let workers: Vec<HashMap<String, String>> = worker_records.into_iter().map(Into::into).collect();
  let mut global = HashMap::new();
  global.insert("fleet_total".to_string(), fleet_total.to_string());
  global.insert("fleet_fresh".to_string(), fleet_fresh.to_string());
  global.insert(
    "fleet_stale".to_string(),
    fleet_total.saturating_sub(fleet_fresh).to_string(),
  );
  // Throughput is cumulative (lifetime) and can reach millions on a long-lived corpus, so group the
  // digits for readability. (Deliberately no "in flight" total here — `dispatched - returned` is a
  // lifetime gap inflated by reaps/redispatches, not currently-in-flight work; the per-worker table
  // surfaces the actionable per-worker gap.)
  global.insert(
    "fleet_dispatched".to_string(),
    group_thousands(fleet_dispatched),
  );
  global.insert(
    "fleet_returned".to_string(),
    group_thousands(fleet_returned),
  );
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

/// One conversion's recorded wall-time (from the worker's `Info:runtime_ms:<N>` log line), for
/// the slowest-conversions table.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RuntimeRowDto {
  /// Corpus the document belongs to (a service may run on several).
  pub corpus: String,
  /// Document short name (paper id) — feed to the document view for forensics.
  pub paper: String,
  /// Task id.
  pub task_id: i64,
  /// Recorded conversion wall-time in milliseconds.
  pub runtime_ms: i32,
}

/// One bar of the runtime-distribution histogram: a millisecond range and how many conversions fell
/// in it.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RuntimeBucketDto {
  /// Human label for the range (e.g. `1–2s`).
  pub label: String,
  /// Number of conversions whose runtime fell in this range.
  pub count: i64,
}

/// The per-service conversion-runtime report: a distribution summary, an aggregate histogram (the
/// bar chart), and the paginated slowest conversions. Sourced from the worker's
/// `Info:runtime_ms:<N>` log lines, so it only populates after a run with a runtime-emitting
/// worker. Heavier than the other service views (it scans `log_infos`), hence its own page.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ServiceRuntimeDto {
  /// Service name.
  pub service: String,
  /// Conversions that recorded a runtime.
  pub total: i64,
  /// Mean runtime (ms).
  pub avg_ms: i32,
  /// Median (p50) runtime (ms).
  pub p50_ms: i32,
  /// p90 runtime (ms).
  pub p90_ms: i32,
  /// p99 runtime (ms).
  pub p99_ms: i32,
  /// Slowest single runtime (ms).
  pub max_ms: i32,
  /// Distribution histogram across fixed ms buckets — the aggregate bar chart.
  pub histogram: Vec<RuntimeBucketDto>,
  /// Pagination offset echoed back.
  pub offset: i64,
  /// Page size echoed back (the cap actually applied).
  pub page_size: i64,
  /// The slowest conversions on this page (descending runtime).
  pub slowest: Vec<RuntimeRowDto>,
}

/// Fixed histogram bucket labels for the runtime distribution (log-ish ms ranges). The index
/// matches the `CASE` bucket computed in [`service_runtime_report`].
const RUNTIME_BUCKET_LABELS: [&str; 11] = [
  "<100ms",
  "100–250ms",
  "250–500ms",
  "0.5–1s",
  "1–2s",
  "2–5s",
  "5–10s",
  "10–30s",
  "30–60s",
  "60–120s",
  "≥120s",
];

/// Builds the [`ServiceRuntimeDto`] for a service from the denormalized `task_runtimes` table (one
/// validated runtime per task, mirrored from each `Info:runtime_ms:<N>` log line on the finalize
/// path) — distribution summary + histogram + the paginated slowest conversions. Three index-only
/// scans over `(service_id, runtime_ms)`.
fn service_runtime_report(
  connection: &mut diesel::PgConnection,
  service: &Service,
  offset: i64,
  page_size: i64,
) -> Result<ServiceRuntimeDto, Status> {
  use diesel::sql_types::{BigInt, Integer, Text};
  // All three scans read the denormalized `task_runtimes` table (one validated `runtime_ms` int per
  // task, with `service_id` inlined), so the per-service filter and aggregates run index-only over
  // `(service_id, runtime_ms)` — no `log_infos`-to-`tasks` join, no `::int` cast, no `~` guard.

  #[derive(QueryableByName)]
  struct SummaryRow {
    #[diesel(sql_type = BigInt)]
    total: i64,
    #[diesel(sql_type = Integer)]
    avg_ms: i32,
    #[diesel(sql_type = Integer)]
    p50: i32,
    #[diesel(sql_type = Integer)]
    p90: i32,
    #[diesel(sql_type = Integer)]
    p99: i32,
    #[diesel(sql_type = Integer)]
    max_ms: i32,
  }
  let summary: SummaryRow = sql_query(
    "SELECT count(*) AS total, \
     coalesce(round(avg(runtime_ms))::int, 0) AS avg_ms, \
     coalesce(percentile_disc(0.5) WITHIN GROUP (ORDER BY runtime_ms), 0) AS p50, \
     coalesce(percentile_disc(0.9) WITHIN GROUP (ORDER BY runtime_ms), 0) AS p90, \
     coalesce(percentile_disc(0.99) WITHIN GROUP (ORDER BY runtime_ms), 0) AS p99, \
     coalesce(max(runtime_ms), 0) AS max_ms FROM task_runtimes WHERE service_id = $1",
  )
  .bind::<Integer, _>(service.id)
  .get_result(connection)
  .map_err(|_| Status::InternalServerError)?;

  #[derive(QueryableByName)]
  struct BucketRow {
    #[diesel(sql_type = Integer)]
    bucket: i32,
    #[diesel(sql_type = BigInt)]
    n: i64,
  }
  let bucket_rows: Vec<BucketRow> = sql_query(
    "SELECT CASE WHEN runtime_ms<100 THEN 0 WHEN runtime_ms<250 THEN 1 WHEN runtime_ms<500 THEN 2 \
     WHEN runtime_ms<1000 THEN 3 WHEN runtime_ms<2000 THEN 4 WHEN runtime_ms<5000 THEN 5 \
     WHEN runtime_ms<10000 THEN 6 WHEN runtime_ms<30000 THEN 7 WHEN runtime_ms<60000 THEN 8 \
     WHEN runtime_ms<120000 THEN 9 ELSE 10 END AS bucket, count(*) AS n \
     FROM task_runtimes WHERE service_id = $1 GROUP BY 1 ORDER BY 1",
  )
  .bind::<Integer, _>(service.id)
  .load(connection)
  .map_err(|_| Status::InternalServerError)?;
  let mut counts = [0i64; RUNTIME_BUCKET_LABELS.len()];
  for row in &bucket_rows {
    if let Some(slot) = counts.get_mut(row.bucket as usize) {
      *slot = row.n;
    }
  }
  let histogram = RUNTIME_BUCKET_LABELS
    .iter()
    .zip(counts)
    .map(|(label, count)| RuntimeBucketDto {
      label: (*label).to_string(),
      count,
    })
    .collect();

  #[derive(QueryableByName)]
  struct SlowRow {
    #[diesel(sql_type = Text)]
    corpus: String,
    #[diesel(sql_type = Text)]
    entry: String,
    #[diesel(sql_type = BigInt)]
    task_id: i64,
    #[diesel(sql_type = Integer)]
    runtime_ms: i32,
  }
  let slow_rows: Vec<SlowRow> = sql_query(
    "SELECT c.name AS corpus, t.entry AS entry, tr.task_id AS task_id, tr.runtime_ms AS runtime_ms \
     FROM task_runtimes tr JOIN tasks t ON t.id = tr.task_id JOIN corpora c ON c.id = t.corpus_id \
     WHERE tr.service_id = $1 ORDER BY tr.runtime_ms DESC LIMIT $2 OFFSET $3",
  )
  .bind::<Integer, _>(service.id)
  .bind::<BigInt, _>(page_size)
  .bind::<BigInt, _>(offset)
  .load(connection)
  .map_err(|_| Status::InternalServerError)?;
  let slowest = slow_rows
    .into_iter()
    .map(|row| RuntimeRowDto {
      corpus: row.corpus,
      // The paper id is the entry's parent-dir name (`…/<paper>/<paper>.zip`).
      paper: std::path::Path::new(&row.entry)
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(row.entry),
      task_id: row.task_id,
      runtime_ms: row.runtime_ms,
    })
    .collect();

  Ok(ServiceRuntimeDto {
    service: service.name.clone(),
    total: summary.total,
    avg_ms: summary.avg_ms,
    p50_ms: summary.p50,
    p90_ms: summary.p90,
    p99_ms: summary.p99,
    max_ms: summary.max_ms,
    histogram,
    offset,
    page_size,
    slowest,
  })
}

/// Human-readable runtime: `<N> ms` under a second, else `<N.N> s`.
fn format_runtime(ms: i32) -> String {
  if ms < 1000 {
    format!("{ms} ms")
  } else {
    format!("{:.1} s", f64::from(ms) / 1000.0)
  }
}

/// Conversion-runtime report for a service (agent twin of the runtime screen): distribution summary
/// + histogram + paginated slowest conversions, from the worker's `runtime_ms` log lines.
/// **Token-gated** via the [`Actor`] guard (`401` without a token). Paginated
/// (`offset`/`page_size`, default 100, max `MAX_REPORT_PAGE_SIZE`; `offset` capped at
/// `MAX_REPORT_OFFSET`); `404` if the service is unknown.
#[rocket_okapi::openapi(tag = "Services")]
#[get("/api/services/<service>/runtimes?<offset>&<page_size>")]
pub fn api_service_runtimes(
  _caller: Actor,
  service: &str,
  offset: Option<i64>,
  page_size: Option<i64>,
  pool: &State<DbPool>,
) -> Result<Json<ServiceRuntimeDto>, Status> {
  let offset = offset.unwrap_or(0).clamp(0, MAX_REPORT_OFFSET);
  let page_size = page_size.unwrap_or(100).clamp(1, MAX_REPORT_PAGE_SIZE);
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let service_record = resolve(service, &mut connection)?;
  Ok(Json(service_runtime_report(
    &mut connection,
    &service_record,
    offset,
    page_size,
  )?))
}

/// The conversion-runtime screen for a service (HTML twin of [`api_service_runtimes`]): the
/// aggregate runtime histogram (bar chart) + the paginated slowest conversions, reached from the
/// worker screen. **Signed-in admins only** (anonymous → sign-in). Separate from the worker view
/// because it reads the `task_runtimes` rollup. `404` if the service is unknown.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/runtimes/<service>?<offset>&<page_size>")]
pub fn service_runtimes_page(
  service: &str,
  offset: Option<i64>,
  page_size: Option<i64>,
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
  let offset = offset.unwrap_or(0).clamp(0, MAX_REPORT_OFFSET);
  let page_size = page_size.unwrap_or(100).clamp(1, MAX_REPORT_PAGE_SIZE);
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let service_record = resolve(service, &mut connection)?;
  let report = service_runtime_report(&mut connection, &service_record, offset, page_size)?;

  // Histogram rows (reusing the `whats` list slot): each carries a CSS bar width as a percentage of
  // the busiest bucket, so the template is a pure-CSS bar chart (no JS).
  let max_bucket = report
    .histogram
    .iter()
    .map(|b| b.count)
    .max()
    .unwrap_or(0)
    .max(1);
  let histogram: Vec<HashMap<String, String>> = report
    .histogram
    .iter()
    .enumerate()
    .map(|(i, bucket)| {
      // Colour the bars by speed using the report design tokens: fast buckets (≤2 s) ok, mid
      // (2–30 s) warn, slow (≥30 s) fatal — so the distribution's health reads at a glance.
      let bar_class = if i <= 4 {
        "ok"
      } else if i <= 7 {
        "warn"
      } else {
        "fatal"
      };
      HashMap::from([
        ("label".to_string(), bucket.label.clone()),
        ("count".to_string(), group_thousands(bucket.count)),
        (
          "pct".to_string(),
          format!("{:.1}", 100.0 * bucket.count as f64 / max_bucket as f64),
        ),
        ("bar_class".to_string(), bar_class.to_string()),
      ])
    })
    .collect();
  // Slowest-conversion rows (reusing the `entries` list slot).
  let entries: Vec<HashMap<String, String>> = report
    .slowest
    .iter()
    .map(|row| {
      HashMap::from([
        ("corpus".to_string(), row.corpus.clone()),
        ("paper".to_string(), row.paper.clone()),
        ("task_id".to_string(), row.task_id.to_string()),
        ("runtime".to_string(), format_runtime(row.runtime_ms)),
        (
          "runtime_ms".to_string(),
          group_thousands(i64::from(row.runtime_ms)),
        ),
      ])
    })
    .collect();

  let mut global = HashMap::new();
  global.insert(
    "title".to_string(),
    format!("Conversion runtimes — {service}"),
  );
  global.insert(
    "description".to_string(),
    format!("Per-paper conversion wall-times for service {service}: distribution + slowest"),
  );
  global.insert("service_name".to_string(), service.to_string());
  global.insert("total".to_string(), group_thousands(report.total));
  global.insert("avg".to_string(), format_runtime(report.avg_ms));
  global.insert("p50".to_string(), format_runtime(report.p50_ms));
  global.insert("p90".to_string(), format_runtime(report.p90_ms));
  global.insert("p99".to_string(), format_runtime(report.p99_ms));
  global.insert("max".to_string(), format_runtime(report.max_ms));
  // Pagination: prev/next offsets + whether each exists (next exists only if more rows remain).
  global.insert("offset".to_string(), offset.to_string());
  global.insert("page_size".to_string(), page_size.to_string());
  global.insert("has_prev".to_string(), (offset > 0).to_string());
  global.insert(
    "prev_offset".to_string(),
    (offset - page_size).max(0).to_string(),
  );
  global.insert(
    "has_next".to_string(),
    (offset + page_size < report.total).to_string(),
  );
  global.insert("next_offset".to_string(), (offset + page_size).to_string());

  let mut context = TemplateContext {
    global,
    entries: Some(entries),
    whats: Some(histogram),
    is_admin: true,
    ..TemplateContext::default()
  };
  decorate_uri_encodings(&mut context);
  Ok(Template::render("service-runtimes", context))
}

/// The route set for the services capability (registry + worker-fleet, screens + agent API).
pub fn routes() -> Vec<Route> {
  // NB: `api_services` + `api_service_workers` + `register_service` + `delete_service` +
  // `set_service_lease` + `api_service_runtimes` are mounted via `frontend::apidoc` (rocket_okapi).
  routes![
    add_service_page,
    create_service_human,
    activate_on_corpus_page,
    activate_on_corpus_human,
    services_page,
    worker_report_page,
    service_runtimes_page,
    delete_service_human,
    set_service_lease_human
  ]
}
