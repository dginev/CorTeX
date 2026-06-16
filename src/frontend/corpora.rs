// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Corpus-management capability: list/inspect/import/delete corpora as screens + API.
//!
//! Follows the symmetry contract — one shared [`CorpusDto`] renders as JSON for agents and (later)
//! as HTML for humans. Handlers live here; the app is assembled in [`crate::frontend::server`].
//! This is the first capability drained out of the binary's legacy routes; more land per increment.

use std::collections::{HashMap, HashSet};

use diesel::pg::PgConnection;
use rocket::form::Form;
use rocket::http::Status;
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::Template;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::backend::{
  create_sandbox, from_address, progress_report, DatabaseUrl, DbPool, SandboxSelection,
};
use crate::concerns::CortexInsertable;
use crate::frontend::actor::{require_admin_to, Actor, AdminReject, AdminSession, ReturnTo};
use crate::frontend::helpers::decorate_uri_encodings;
use crate::frontend::jobs::JobDto;
use crate::frontend::params::TemplateContext;
use crate::importer::Importer;
use crate::jobs::{self, JobProgress};
use crate::models::{Corpus, NewCorpus, Service};
use rocket_okapi::openapi;
use schemars::JsonSchema;

/// The magic `import` service id. Service ids `1` (`init`) and `2` (`import`) are infrastructure
/// (CLAUDE.md: real conversion services have id `> 2`); a service with id `≤` this is never
/// user-activatable in the picker nor deactivatable from a corpus.
const IMPORT_SERVICE_ID: i32 = 2;

/// A corpus as exposed over the API/UI. `name` is the stable external handle used by every route.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CorpusDto {
  /// Stable external handle (UUIDv7) — survives a rename, unlike `name`. Use for durable
  /// references.
  pub public_id: String,
  /// Human-readable corpus name (its external handle).
  pub name: String,
  /// Filesystem path to the corpus root.
  pub path: String,
  /// Human-readable description.
  pub description: String,
  /// Whether documents are multi-file (complex) rather than a single TeX file.
  pub complex: bool,
  /// Number of ingested documents (import-service tasks) — the corpus's scale at a glance.
  pub document_count: i64,
}

impl CorpusDto {
  /// Builds the DTO, attaching the document count looked up from the batched per-corpus map.
  /// Public so the `cortex` CLI can emit the identical shape (web ↔ CLI ↔ agent parity).
  pub fn build(corpus: Corpus, document_count: i64) -> Self {
    CorpusDto {
      public_id: corpus.public_id.to_string(),
      name: corpus.name,
      path: corpus.path,
      description: corpus.description,
      complex: corpus.complex,
      document_count,
    }
  }
}

/// Lists all registered corpora (the agent twin of the overview screen).
#[openapi(tag = "Corpora")]
#[get("/api/corpora")]
pub fn api_corpora(pool: &State<DbPool>) -> Json<Vec<CorpusDto>> {
  let Ok(mut connection) = pool.get() else {
    return Json(Vec::new());
  };
  let corpora = Corpus::all(&mut connection).unwrap_or_default();
  let counts = Corpus::document_counts(&mut connection);
  Json(
    corpora
      .into_iter()
      .map(|corpus| {
        let count = counts.get(&corpus.id).copied().unwrap_or(0);
        CorpusDto::build(corpus, count)
      })
      .collect(),
  )
}

/// Per-service status counts within a corpus (mirrors the progress report).
#[derive(Debug, Serialize, JsonSchema)]
pub struct ServiceStatusDto {
  /// Service name.
  pub name: String,
  /// Service version.
  pub version: f32,
  /// Total valid tasks (excludes invalids).
  pub total: i64,
  /// Completed with no notable problems.
  pub no_problem: i64,
  /// Completed with warnings.
  pub warning: i64,
  /// Completed with errors.
  pub error: i64,
  /// Fatal failures.
  pub fatal: i64,
  /// Invalid tasks (excluded from totals).
  pub invalid: i64,
  /// Queued / not-yet-processed tasks.
  pub todo: i64,
}

/// A corpus with its activated services and their status counts.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CorpusDetailDto {
  /// Corpus name (external handle).
  pub name: String,
  /// Filesystem path to the corpus root.
  pub path: String,
  /// Human-readable description.
  pub description: String,
  /// Whether documents are multi-file.
  pub complex: bool,
  /// Services activated on this corpus, with status counts.
  pub services: Vec<ServiceStatusDto>,
}

/// Inspects a single corpus: its activated services and per-service status counts.
#[openapi(tag = "Corpora")]
#[get("/api/corpora/<name>")]
pub fn api_corpus(name: &str, pool: &State<DbPool>) -> Result<Json<CorpusDetailDto>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let corpus = Corpus::find_by_name(name, &mut connection).map_err(|_| Status::NotFound)?;
  let services = corpus.select_services(&mut connection).unwrap_or_default();
  let mut service_status = Vec::new();
  for service in services {
    let report = progress_report(&mut connection, corpus.id, service.id);
    let count = |key: &str| report.get(key).copied().unwrap_or(0.0) as i64;
    service_status.push(ServiceStatusDto {
      name: service.name,
      version: service.version,
      total: count("total"),
      no_problem: count("no_problem"),
      warning: count("warning"),
      error: count("error"),
      fatal: count("fatal"),
      invalid: count("invalid"),
      todo: count("todo"),
    });
  }
  Ok(Json(CorpusDetailDto {
    name: corpus.name,
    path: corpus.path,
    description: corpus.description,
    complex: corpus.complex,
    services: service_status,
  }))
}

/// Request body for registering and importing a corpus.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ImportRequest {
  /// Corpus name (external handle).
  pub name: String,
  /// Filesystem path to the corpus root.
  pub path: String,
  /// Whether documents are multi-file (complex).
  pub complex: bool,
  /// Optional human-readable description.
  pub description: Option<String>,
}

/// Registers a corpus and starts an in-process import job; returns `202 Accepted` + the job handle.
/// Agents and humans poll `GET /api/jobs/<uuid>` (or the progress page) for completion.
/// **Token-gated** via the [`Actor`] guard (creating a corpus + a filesystem import job is a
/// consequential write); `401` without a valid token, `409` if the corpus name already exists,
/// `422` if the `path` is not a readable directory on the server.
#[rocket_okapi::openapi(tag = "Corpora")]
#[post("/api/corpora", format = "json", data = "<request>")]
pub fn import_corpus(
  request: Json<ImportRequest>,
  actor: Actor,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<(Status, Json<JobDto>), Status> {
  let request = request.into_inner();
  let job_uuid = start_import(
    pool,
    &database_url.0,
    &actor.owner,
    request.name,
    request.path,
    request.complex,
    request.description.unwrap_or_default(),
  )?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let job = jobs::find_job(&mut connection, job_uuid).ok_or(Status::InternalServerError)?;
  Ok((Status::Accepted, Json(JobDto::from(job))))
}

/// Registers a corpus and spawns its import job, returning the job uuid. The shared core of the
/// agent endpoint and the human form. `409` if the name already exists; `422` if the `path` is not
/// a readable directory on the server (pre-flighted so a doomed import is never started).
#[allow(clippy::too_many_arguments)]
fn start_import(
  pool: &DbPool,
  database_url: &str,
  actor: &str,
  name: String,
  path: String,
  complex: bool,
  description: String,
) -> Result<Uuid, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  if Corpus::find_by_name(&name, &mut connection).is_ok() {
    return Err(Status::Conflict);
  }
  // Pre-flight the source path (the in-process import reads it): reject a path that doesn't exist
  // or isn't a readable directory, so the admin/agent gets immediate feedback instead of a
  // registered corpus whose import silently finds nothing. `422` distinguishes it from the `409`
  // name clash.
  if !std::fs::metadata(path.trim_end())
    .map(|meta| meta.is_dir())
    .unwrap_or(false)
  {
    return Err(Status::UnprocessableEntity);
  }
  NewCorpus {
    name: name.clone(),
    path: path.clone(),
    complex,
    description,
  }
  .create(&mut connection)
  .map_err(|_| Status::InternalServerError)?;
  let corpus =
    Corpus::find_by_name(&name, &mut connection).map_err(|_| Status::InternalServerError)?;
  drop(connection);

  let database_url = database_url.to_string();
  let params = serde_json::json!({ "name": name, "path": path });
  jobs::spawn_job(
    pool.clone(),
    "corpus_import",
    actor,
    params,
    move |progress| run_import(&database_url, corpus, progress),
  )
  .map_err(|_| Status::InternalServerError)
}

/// Fields of the human "Add a corpus" form on the admin dashboard.
#[derive(FromForm)]
pub struct ImportForm {
  /// Corpus name (external handle).
  pub name: String,
  /// Filesystem path to the corpus root (server-side).
  pub path: String,
  /// Whether documents are multi-file (complex).
  pub complex: bool,
  /// Optional description.
  pub description: Option<String>,
}

/// The human twin of [`import_corpus`]: the admin dashboard's "Add a corpus" form. **Gated by the
/// signed-in [`AdminSession`] cookie** (no token typed in the form — an anonymous browser is
/// redirected to sign-in); registers + imports the corpus off the request path and redirects to
/// `/jobs`. `409` if the name is taken.
// The Err variant is a re-rendered form `Template` (the friendly-error path), which is chunky —
// fine for a one-shot request handler.
#[allow(clippy::result_large_err)]
#[post("/corpus/import", data = "<form>")]
pub fn import_corpus_human(
  form: Form<ImportForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<Redirect, Template> {
  let Some(session) = session else {
    return Ok(Redirect::to("/admin/login"));
  };
  let form = form.into_inner();
  let description = form.description.clone().unwrap_or_default();
  match start_import(
    pool,
    &database_url.0,
    &session.owner,
    form.name.clone(),
    form.path.clone(),
    form.complex,
    description.clone(),
  ) {
    Ok(uuid) => Ok(Redirect::to(format!("/jobs/{uuid}"))),
    // A failed submit re-renders the form with a friendly message + the values preserved, rather
    // than a bare error page that loses the admin's input.
    Err(status) => {
      let message = match status.code {
        409 => format!(
          "A corpus named “{}” already exists — choose a different name.",
          form.name
        ),
        422 => format!(
          "“{}” is not a readable directory on the server — check the path.",
          form.path
        ),
        503 => "The database is temporarily unavailable — please try again.".to_string(),
        _ => "Could not register the corpus — check the values and try again.".to_string(),
      };
      Err(render_corpora_new(
        Some(&message),
        &form.name,
        &form.path,
        &description,
        form.complex,
      ))
    },
  }
}

/// The body of a `corpus_import` job: run the importer in-process against `corpus`, reporting
/// progress, and return the number of import-service tasks created.
fn run_import(database_url: &str, corpus: Corpus, progress: &JobProgress) -> Result<Value, String> {
  let corpus_id = corpus.id;
  let mut importer = Importer {
    corpus,
    backend: from_address(database_url),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };
  progress.step(0, None, "importing corpus");
  importer.process().map_err(|error| error.to_string())?;
  let imported = count_service_tasks(&mut importer.backend.connection, corpus_id, 2);
  progress.step(imported, Some(imported), "import complete");
  Ok(serde_json::json!({ "imported": imported }))
}

/// Counts the tasks registered for a `(corpus, service)` pair.
fn count_service_tasks(connection: &mut PgConnection, corpus: i32, service: i32) -> i32 {
  use crate::schema::tasks::dsl::{corpus_id, service_id, tasks};
  use diesel::prelude::*;
  tasks
    .filter(corpus_id.eq(corpus))
    .filter(service_id.eq(service))
    .count()
    .get_result::<i64>(connection)
    .unwrap_or(0) as i32
}

/// Request body for carving a **sandbox** corpus out of a parent by a message-condition filter
/// (Arm 5). The `(service, severity, category, what)` dimensions match the report drill-down.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SandboxRequest {
  /// Name for the new sandbox corpus (its external handle; must be unique).
  pub name: String,
  /// The service whose conversion results are filtered.
  pub service_id: i32,
  /// Severity key (`no_problem` | `warning` | `error` | `fatal` | `invalid`).
  pub severity: String,
  /// Optional message-category narrowing.
  pub category: Option<String>,
  /// Optional `what` narrowing within the category.
  pub what: Option<String>,
}

impl From<&SandboxRequest> for SandboxSelection {
  fn from(request: &SandboxRequest) -> Self {
    SandboxSelection {
      service_id: request.service_id,
      severity: request.severity.clone(),
      category: request.category.clone(),
      what: request.what.clone(),
    }
  }
}

/// Carves a **sandbox corpus** from `<parent>` by a message-condition filter and starts the job
/// that populates it; returns `202 Accepted` + the job handle to poll. **Token-gated** via the
/// [`Actor`] guard; `401` without a valid token, `404` if the parent is unknown, `409` if the
/// sandbox name is taken. The sandbox is a first-class corpus an agent can then run/rerun to
/// iterate a campaign.
#[rocket_okapi::openapi(tag = "Corpora")]
#[post("/api/corpora/<parent>/sandbox", format = "json", data = "<request>")]
pub fn create_sandbox_corpus(
  parent: &str,
  request: Json<SandboxRequest>,
  actor: Actor,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<(Status, Json<JobDto>), Status> {
  let request = request.into_inner();
  let job_uuid = start_sandbox(pool, &database_url.0, &actor.owner, parent, &request)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let job = jobs::find_job(&mut connection, job_uuid).ok_or(Status::InternalServerError)?;
  Ok((Status::Accepted, Json(JobDto::from(job))))
}

/// Resolves the parent (`404`), rejects a taken sandbox name (`409`), and spawns the
/// `corpus_sandbox` job. The shared core of the agent endpoint and the human form.
fn start_sandbox(
  pool: &DbPool,
  database_url: &str,
  actor: &str,
  parent: &str,
  request: &SandboxRequest,
) -> Result<Uuid, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let parent_corpus =
    Corpus::find_by_name(parent, &mut connection).map_err(|_| Status::NotFound)?;
  if Corpus::find_by_name(&request.name, &mut connection).is_ok() {
    return Err(Status::Conflict);
  }
  drop(connection);

  let database_url = database_url.to_string();
  let name = request.name.clone();
  let selection = SandboxSelection::from(request);
  let params = serde_json::json!({
    "parent": parent, "name": name, "selection": serde_json::to_value(&selection).ok(),
  });
  jobs::spawn_job(
    pool.clone(),
    "corpus_sandbox",
    actor,
    params,
    move |progress| run_sandbox(&database_url, parent_corpus, name, selection, progress),
  )
  .map_err(|_| Status::InternalServerError)
}

/// Fields of the human "Create a sandbox" form on the corpus page.
#[derive(FromForm)]
pub struct SandboxForm {
  /// New sandbox corpus name.
  pub name: String,
  /// Service whose results are filtered.
  pub service_id: i32,
  /// Severity key.
  pub severity: String,
  /// Optional category narrowing (empty string = none).
  pub category: Option<String>,
  /// Optional `what` narrowing (empty string = none).
  pub what: Option<String>,
}

/// The human twin of [`create_sandbox_corpus`]: the corpus page's "Create a sandbox" form. **Gated
/// by the signed-in [`AdminSession`] cookie**; carves the sandbox off the request path and
/// redirects to `/jobs`. `404` unknown parent, `409` name taken.
#[post("/corpus/<parent>/sandbox", data = "<form>")]
pub fn create_sandbox_human(
  parent: &str,
  form: Form<SandboxForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<Redirect, Status> {
  let Some(session) = session else {
    return Ok(Redirect::to("/admin/login"));
  };
  let form = form.into_inner();
  // Treat empty optional inputs as "no narrowing".
  let blank_to_none = |value: Option<String>| value.filter(|text| !text.trim().is_empty());
  let request = SandboxRequest {
    name: form.name,
    service_id: form.service_id,
    severity: form.severity,
    category: blank_to_none(form.category),
    what: blank_to_none(form.what),
  };
  let uuid = start_sandbox(pool, &database_url.0, &session.owner, parent, &request)?;
  Ok(Redirect::to(format!("/jobs/{uuid}")))
}

/// The body of a `corpus_sandbox` job: carve the sandbox in-process and report the captured-entry
/// count. Returns `{ sandbox, entries }`.
fn run_sandbox(
  database_url: &str,
  parent: Corpus,
  name: String,
  selection: SandboxSelection,
  progress: &JobProgress,
) -> Result<Value, String> {
  progress.step(0, None, &format!("carving sandbox '{name}'"));
  let mut backend = from_address(database_url);
  let outcome = create_sandbox(&mut backend.connection, &parent, &name, &selection)
    .map_err(|error| error.to_string())?;
  let captured = outcome.entry_count as i32;
  progress.step(captured, Some(captured), "sandbox created");
  Ok(serde_json::json!({ "sandbox": outcome.sandbox.name, "entries": outcome.entry_count }))
}

/// Extends an existing corpus with newly-arrived entries; starts an in-process job and returns
/// `202 Accepted` + the job handle. **Token-gated** via the [`Actor`] guard; `401` without a valid
/// token, `404` if the corpus is unknown.
#[rocket_okapi::openapi(tag = "Corpora")]
#[post("/api/corpora/<name>/extend")]
pub fn extend_corpus(
  name: &str,
  actor: Actor,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<(Status, Json<JobDto>), Status> {
  let job_uuid = start_extend(pool, &database_url.0, &actor.owner, name)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let job = jobs::find_job(&mut connection, job_uuid).ok_or(Status::InternalServerError)?;
  Ok((Status::Accepted, Json(JobDto::from(job))))
}

/// Spawns a corpus-extend job for an existing corpus (`404` if unknown), returning the job uuid.
/// Shared by the agent endpoint and the human form.
fn start_extend(
  pool: &DbPool,
  database_url: &str,
  actor: &str,
  name: &str,
) -> Result<Uuid, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let corpus = Corpus::find_by_name(name, &mut connection).map_err(|_| Status::NotFound)?;
  drop(connection);
  let database_url = database_url.to_string();
  let params = serde_json::json!({ "name": name });
  jobs::spawn_job(
    pool.clone(),
    "corpus_extend",
    actor,
    params,
    move |progress| run_extend(&database_url, corpus, progress),
  )
  .map_err(|_| Status::InternalServerError)
}

/// The human twin of [`extend_corpus`]: the corpus screen's "Re-scan for new entries" button.
/// **Gated by the signed-in [`AdminSession`] cookie** (anonymous → sign-in); spawns the extend job
/// and redirects to `/jobs`. `404` if the corpus is unknown.
#[post("/corpus/<name>/extend")]
pub fn extend_corpus_human(
  name: &str,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<Redirect, Status> {
  let Some(session) = session else {
    return Ok(Redirect::to("/admin/login"));
  };
  let uuid = start_extend(pool, &database_url.0, &session.owner, name)?;
  Ok(Redirect::to(format!("/jobs/{uuid}")))
}

/// The body of a `corpus_extend` job: import newly-arrived entries and propagate them to the real
/// (non-init/import) services, returning the resulting import-task count.
fn run_extend(database_url: &str, corpus: Corpus, progress: &JobProgress) -> Result<Value, String> {
  let corpus_id = corpus.id;
  let corpus_path = corpus.path.clone();
  let mut importer = Importer {
    corpus,
    backend: from_address(database_url),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };
  progress.step(0, None, "extending corpus");
  importer
    .extend_corpus()
    .map_err(|error| error.to_string())?;
  let services = importer
    .corpus
    .select_services(&mut importer.backend.connection)
    .unwrap_or_default();
  for service in services.iter().filter(|service| service.id > 2) {
    importer
      .backend
      .extend_service(service, &corpus_path)
      .map_err(|error| error.to_string())?;
  }
  let imported = count_service_tasks(&mut importer.backend.connection, corpus_id, 2);
  progress.step(imported, Some(imported), "extend complete");
  Ok(serde_json::json!({ "import_tasks": imported }))
}

/// Activates a registered `service` on a `corpus`: creates a TODO task per imported document so the
/// workers begin converting it. **Token-gated** via the [`Actor`] guard (the run is attributed to
/// the authenticated actor); the work runs as a background job — poll `GET /api/jobs/<uuid>` for
/// the pending/done status. `401` without a valid token, `404` on an unknown corpus/service, `409`
/// if the service is **already registered** on the corpus (registration is idempotent-neutral — no
/// re-activation wipes existing results; use *extend*/*rerun* instead), `202` with the job handle
/// on success.
#[rocket_okapi::openapi(tag = "Corpora")]
#[post("/api/corpora/<corpus>/services/<service>")]
pub fn activate_service(
  corpus: &str,
  service: &str,
  actor: Actor,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<(Status, Json<JobDto>), Status> {
  let job_uuid = start_activate(pool, &database_url.0, &actor.owner, corpus, service)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let job = jobs::find_job(&mut connection, job_uuid).ok_or(Status::InternalServerError)?;
  Ok((Status::Accepted, Json(JobDto::from(job))))
}

/// Resolves the `(corpus, service)`, spawns the activation job (attributed to `actor`), and returns
/// the job uuid. `404` on an unknown corpus/service. Shared by the agent endpoint, the corpus
/// screen's human form, and the "Add a service" screen (which activates a freshly-defined service
/// on each checked corpus — see [`crate::frontend::services`]).
pub(crate) fn start_activate(
  pool: &DbPool,
  database_url: &str,
  actor: &str,
  corpus: &str,
  service: &str,
) -> Result<Uuid, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let corpus_record =
    Corpus::find_by_name(corpus, &mut connection).map_err(|_| Status::NotFound)?;
  let service_record =
    Service::find_by_name(service, &mut connection).map_err(|_| Status::NotFound)?;
  // Idempotent-neutral: refuse to re-register an already-registered (service, corpus) pair. The
  // activation is destructive (`register_service` wipes & re-creates the pair's tasks + their
  // `log_*` rows), so a re-register would throw away completed results. Reject with `409` and spawn
  // **no** job — no action taken. (To add newly-imported documents use *extend*; to re-process use
  // *rerun*.) The backend `register_service` enforces the same invariant as defense-in-depth.
  if count_service_tasks(&mut connection, corpus_record.id, service_record.id) > 0 {
    return Err(Status::Conflict);
  }
  drop(connection);
  let database_url = database_url.to_string();
  let owner = actor.to_string();
  let params = serde_json::json!({ "corpus": corpus, "service": service });
  jobs::spawn_job(
    pool.clone(),
    "service_activate",
    actor,
    params,
    move |progress| {
      run_activate(
        &database_url,
        corpus_record,
        service_record,
        owner,
        progress,
      )
    },
  )
  .map_err(|_| Status::InternalServerError)
}

/// Fields of the human "Activate a service" form on the corpus screen.
#[derive(FromForm)]
pub struct ActivateForm {
  /// The registered service to activate on this corpus.
  pub service: String,
}

/// The human twin of [`activate_service`]: the corpus screen's "Activate a service" form. **Gated
/// by the signed-in [`AdminSession`] cookie** (anonymous → sign-in); spawns the activation job and
/// redirects to `/jobs`. `404` on an unknown corpus/service.
#[post("/corpus/<corpus>/activate", data = "<form>")]
pub fn activate_service_human(
  corpus: &str,
  form: Form<ActivateForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<Redirect, Status> {
  let Some(session) = session else {
    return Ok(Redirect::to("/admin/login"));
  };
  let uuid = start_activate(pool, &database_url.0, &session.owner, corpus, &form.service)?;
  Ok(Redirect::to(format!("/jobs/{uuid}")))
}

/// The body of a `service_activate` job: register `service` on `corpus` (creating a TODO task per
/// imported document), attributing the new run to `owner`, and return the task count created.
fn run_activate(
  database_url: &str,
  corpus: Corpus,
  service: Service,
  owner: String,
  progress: &JobProgress,
) -> Result<Value, String> {
  let (corpus_id, service_id) = (corpus.id, service.id);
  let corpus_path = corpus.path.clone();
  let corpus_name = corpus.name.clone();
  let service_name = service.name.clone();
  let mut backend = from_address(database_url);
  progress.step(
    0,
    None,
    &format!("registering {service_name} on {corpus_name}"),
  );
  backend
    .register_service(
      &service,
      &corpus_path,
      owner,
      format!("Activated service {service_name} on {corpus_name}"),
    )
    .map_err(|error| error.to_string())?;
  let activated = count_service_tasks(&mut backend.connection, corpus_id, service_id);
  progress.step(
    activated,
    Some(activated),
    &format!("registered {service_name} on {corpus_name} ({activated} tasks)"),
  );
  Ok(serde_json::json!({ "tasks": activated, "corpus": corpus_name, "service": service_name }))
}

/// Deletes a corpus and all of its tasks and log messages. **Token-gated** via the [`Actor`] guard
/// (an unauthenticated wipe of a corpus must not be possible — `401` without a valid token) and
/// double-guarded: the caller must also echo the corpus name via `?confirm=<name>` to proceed
/// (prevents accidental wipes; the UI confirms the same way). Returns 204 on success, 400 if the
/// confirmation does not match, 404 if unknown.
#[rocket_okapi::openapi(tag = "Corpora")]
#[delete("/api/corpora/<name>?<confirm>")]
pub fn delete_corpus(
  name: &str,
  confirm: Option<&str>,
  actor: Actor,
  pool: &State<DbPool>,
) -> Status {
  if confirm != Some(name) {
    return Status::BadRequest;
  }
  let mut connection = match pool.get() {
    Ok(connection) => connection,
    Err(_) => return Status::ServiceUnavailable,
  };
  let corpus = match Corpus::find_by_name(name, &mut connection) {
    Ok(corpus) => corpus,
    Err(_) => return Status::NotFound,
  };
  match delete_corpus_cascade(&mut connection, corpus) {
    Ok(()) => {
      tracing::info!(actor = %actor.owner, corpus = name, "corpus deleted via API");
      Status::NoContent
    },
    Err(status) => status,
  }
}

/// Fields of the human "Delete corpus" form: the name echoed as confirmation.
#[derive(FromForm)]
pub struct DeleteForm {
  /// Must equal the corpus name to confirm the destructive action.
  pub confirm: String,
}

/// The human twin of [`delete_corpus`]: the corpus screen's "Delete corpus" form. **Gated by the
/// signed-in [`AdminSession`] cookie** (anonymous → sign-in) *and* confirmation-gated (the form
/// echoes the corpus name), then redirects to the overview. `400` if the confirmation doesn't
/// match, `404` if unknown.
#[post("/corpus/<name>/delete", data = "<form>")]
pub fn delete_corpus_human(
  name: &str,
  form: Form<DeleteForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Redirect, Status> {
  if session.is_none() {
    return Ok(Redirect::to("/admin/login"));
  }
  if form.confirm != name {
    return Err(Status::BadRequest);
  }
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let corpus = Corpus::find_by_name(name, &mut connection).map_err(|_| Status::NotFound)?;
  delete_corpus_cascade(&mut connection, corpus)?;
  // Land on the overview with a confirmation flash (corpus names are URL-safe by construction).
  Ok(Redirect::to(format!("/?deleted={name}")))
}

/// Deactivates (retires) a `service` from a `corpus`: deletes that pair's tasks + log messages (the
/// service definition and its work on other corpora are untouched — the symmetric counterpart of
/// [`activate_service`]). **Token-gated** via the [`Actor`] guard and confirmation-gated
/// (`?confirm=<service>`, echoing the service name). Returns `204` on success, `400` if the
/// confirmation doesn't match, `404` if the corpus or service is unknown.
#[rocket_okapi::openapi(tag = "Corpora")]
#[delete("/api/corpora/<corpus>/services/<service>?<confirm>")]
pub fn deactivate_service(
  corpus: &str,
  service: &str,
  confirm: Option<&str>,
  actor: Actor,
  pool: &State<DbPool>,
) -> Status {
  if confirm != Some(service) {
    return Status::BadRequest;
  }
  let mut connection = match pool.get() {
    Ok(connection) => connection,
    Err(_) => return Status::ServiceUnavailable,
  };
  let corpus_record = match Corpus::find_by_name(corpus, &mut connection) {
    Ok(corpus) => corpus,
    Err(_) => return Status::NotFound,
  };
  let service_record = match Service::find_by_name(service, &mut connection) {
    Ok(service) => service,
    Err(_) => return Status::NotFound,
  };
  // The magic `init` (1) / `import` (2) services are infrastructure — deactivating `import` would
  // wipe the corpus's document registry. Never deactivatable.
  if service_record.id <= IMPORT_SERVICE_ID {
    return Status::Forbidden;
  }
  match service_record.deactivate_from_corpus(&corpus_record, &mut connection) {
    Ok(_) => {
      tracing::info!(actor = %actor.owner, corpus, service, "service deactivated from corpus via API");
      Status::NoContent
    },
    Err(_) => Status::InternalServerError,
  }
}

/// Fields of the human per-service "Deactivate" form: the service echoed as confirmation.
#[derive(FromForm)]
pub struct DeactivateForm {
  /// Must equal the service name to confirm the destructive action.
  pub confirm: String,
}

/// The human twin of [`deactivate_service`]: the corpus screen's per-service "Deactivate" form.
/// **Gated by the signed-in [`AdminSession`] cookie** (anonymous → sign-in) *and*
/// confirmation-gated (echoes the service name), then redirects back to the corpus page. `400` if
/// the confirmation doesn't match, `404` if unknown.
#[post("/corpus/<corpus>/services/<service>/deactivate", data = "<form>")]
pub fn deactivate_service_human(
  corpus: &str,
  service: &str,
  form: Form<DeactivateForm>,
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
  let corpus_record =
    Corpus::find_by_name(corpus, &mut connection).map_err(|_| Status::NotFound)?;
  let service_record =
    Service::find_by_name(service, &mut connection).map_err(|_| Status::NotFound)?;
  // Guard the magic init/import services (see [`deactivate_service`]).
  if service_record.id <= IMPORT_SERVICE_ID {
    return Err(Status::Forbidden);
  }
  service_record
    .deactivate_from_corpus(&corpus_record, &mut connection)
    .map_err(|_| Status::InternalServerError)?;
  Ok(Redirect::to(format!(
    "/corpus/{corpus}?deactivated={service}"
  )))
}

/// Acknowledgement for a save-snapshot: the `(corpus, service)` frozen and how many per-task status
/// rows were appended to `historical_tasks`.
#[derive(Serialize, JsonSchema)]
pub struct SnapshotAckDto {
  /// Corpus the snapshot froze.
  pub corpus: String,
  /// Service the snapshot froze.
  pub service: String,
  /// The authenticated initiator the snapshot is attributed to.
  pub actor: String,
  /// Per-task status rows appended to `historical_tasks` (the size of the frozen snapshot).
  pub saved: usize,
}

/// Freezes the current per-task statuses of a `(corpus, service)` into `historical_tasks` — the
/// agent twin of the report screen's "save snapshot" action (`POST /savetasks/...`), so an agent
/// can capture a baseline before a rerun campaign and later diff against it (`GET
/// /api/runs/.../tasks`). **Token-gated** via the [`Actor`] guard; the snapshot is **append-only**
/// (history stays immutable over the API — there is deliberately no snapshot delete/modify
/// endpoint; pruning is a human-admin operation, see [`crate::frontend::retention`]). `401` without
/// a valid token, `404` on an unknown corpus/service, `202` with the appended-row count on success.
/// Uses a fresh connection (not the request pool) since the snapshot is a single bulk `INSERT …
/// SELECT` over every task and shouldn't pin a pooled slot.
#[rocket_okapi::openapi(tag = "Management")]
#[post("/api/corpora/<corpus>/services/<service>/snapshot")]
pub fn snapshot_tasks(
  corpus: &str,
  service: &str,
  actor: Actor,
  database_url: &State<DatabaseUrl>,
) -> Result<(Status, Json<SnapshotAckDto>), Status> {
  let mut backend = from_address(&database_url.0);
  let corpus_record =
    Corpus::find_by_name(corpus, &mut backend.connection).map_err(|_| Status::NotFound)?;
  let service_record =
    Service::find_by_name(service, &mut backend.connection).map_err(|_| Status::NotFound)?;
  let saved = backend
    .save_historical_tasks(&corpus_record, &service_record)
    .map_err(|_| Status::InternalServerError)?;
  tracing::info!(actor = %actor.owner, corpus, service, saved, "snapshot via API");
  Ok((
    Status::Accepted,
    Json(SnapshotAckDto {
      corpus: corpus.to_string(),
      service: service.to_string(),
      actor: actor.owner,
      saved,
    }),
  ))
}

/// Removes a corpus's log messages (the `log_*` tables have no FK cascade), then its tasks and the
/// corpus row itself.
fn delete_corpus_cascade(connection: &mut PgConnection, corpus: Corpus) -> Result<(), Status> {
  // `Corpus::destroy` is the complete, transactional deletion primitive (log_* + tasks + corpus,
  // atomic + orphan-free); the handler only maps its error to an HTTP status.
  corpus
    .destroy(connection)
    .map(|_| ())
    .map_err(|_| Status::InternalServerError)
}

// --- Human screens (HTML twins of the corpora API above) ---------------------------------------
//
// Relocated from `bin/frontend.rs` onto the library surface so they share the connection **pool**
// (no per-request `Backend::default()` fresh libpq connect) and are testable via `rocket::local`.

/// The overview screen (HTML twin of [`api_corpora`]): the table of registered corpora — the
/// admin landing page. `503` if the pool is exhausted.
#[get("/?<deleted>")]
pub fn overview_page(deleted: Option<&str>, pool: &State<DbPool>) -> Result<Template, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let counts = Corpus::document_counts(&mut connection);
  let corpora = Corpus::all(&mut connection)
    .unwrap_or_default()
    .iter()
    .map(|corpus| {
      let mut hash = corpus.to_hash();
      // Document scale at a glance on the landing (0 → omitted client-side); grouped for
      // readability.
      if let Some(count) = counts.get(&corpus.id) {
        hash.insert(
          "document_count".to_string(),
          crate::frontend::helpers::group_thousands(*count),
        );
      }
      hash
    })
    .collect::<Vec<_>>();
  let mut global = HashMap::new();
  global.insert(
    "title".to_string(),
    "Overview of available Corpora".to_string(),
  );
  global.insert(
    "description".to_string(),
    "An analysis framework for corpora of TeX/LaTeX documents - overview page".to_string(),
  );
  // The landing page carries the full hero wordmark, so the shared nav suppresses its brand logo
  // here (it shows on every *other* page).
  global.insert("is_landing".to_string(), "true".to_string());
  // `?deleted=<name>` flashes a confirmation after a corpus delete (the post-redirect-get lands
  // here), so a destructive action gets explicit feedback, not just "it vanished from the list".
  if let Some(name) = deleted {
    global.insert("deleted".to_string(), name.to_string());
  }
  let mut context = TemplateContext {
    global,
    corpora: Some(corpora),
    ..TemplateContext::default()
  };
  decorate_uri_encodings(&mut context);
  Ok(Template::render("overview", context))
}

/// The corpus screen (HTML twin of [`api_corpus`]): the services registered on a corpus. `404` if
/// the corpus is unknown, `503` if the pool is exhausted.
#[get("/corpus/<name>?<deactivated>")]
pub fn corpus_page(
  name: &str,
  deactivated: Option<&str>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Template, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let corpus = Corpus::find_by_name(name, &mut connection).map_err(|_| Status::NotFound)?;
  let mut global = HashMap::new();
  // `?deactivated=<service>` flashes a confirmation after a service deactivation lands back here.
  if let Some(service) = deactivated {
    global.insert("deactivated".to_string(), service.to_string());
  }
  global.insert(
    "title".to_string(),
    format!("Registered services for {}", corpus.name),
  );
  global.insert(
    "description".to_string(),
    format!(
      "An analysis framework for corpora of TeX/LaTeX documents - registered services for {}",
      corpus.name
    ),
  );
  global.insert("corpus_name".to_string(), corpus.name.clone());
  global.insert("corpus_description".to_string(), corpus.description.clone());
  // Each activated service, enriched with its per-severity task counts (the same numbers the agent
  // `api_corpus` reports) so the corpus screen is a progress dashboard, not just a service list.
  let service_records = corpus.select_services(&mut connection).unwrap_or_default();
  let mut services = Vec::with_capacity(service_records.len());
  for service in &service_records {
    let mut hash = service.to_hash();
    let report = progress_report(&mut connection, corpus.id, service.id);
    for key in [
      "total",
      "no_problem",
      "warning",
      "error",
      "fatal",
      "invalid",
      "todo",
    ] {
      hash.insert(
        key.to_string(),
        (report.get(key).copied().unwrap_or(0.0) as i64).to_string(),
      );
    }
    services.push(hash);
  }
  // The "register a service on this corpus" picker (the corpus-side mirror of the service screen's
  // "register on a corpus" <select>): all real (non-init/import) services **not yet activated on
  // this corpus**. Already-activated services are excluded — re-registering is rejected
  // (idempotent-neutral, 409) and must not be offered.
  let all_services = Service::all(&mut connection)
    .unwrap_or_default()
    .iter()
    .filter(|service| {
      service.id > IMPORT_SERVICE_ID
        && !service_records
          .iter()
          .any(|activated| activated.id == service.id)
    })
    .map(Service::to_hash)
    .collect::<Vec<_>>();
  let mut context = TemplateContext {
    global,
    services: Some(services),
    all_services: Some(all_services),
    is_admin: session.is_some(),
    ..TemplateContext::default()
  };
  decorate_uri_encodings(&mut context);
  Ok(Template::render("services", context))
}

/// The "Add a corpus" screen (the corpus analogue of `/services/new`): the import form on its own
/// page, linked from the admin dashboard. **Signed-in admins only** (anonymous → sign-in); the form
/// posts to the existing `POST /corpus/import`.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
/// Renders the "Add a corpus" form. Shared by the GET page and the POST error path, so a failed
/// submit (e.g. a name collision) re-renders the form with a friendly `error` and the typed values
/// preserved instead of a bare error page. The `corpus_*` keys carry the form values (the page meta
/// `description` is separate). Admin-only page, so `is_admin` is set.
fn render_corpora_new(
  error: Option<&str>,
  name: &str,
  path: &str,
  description: &str,
  complex: bool,
) -> Template {
  let mut global = HashMap::new();
  global.insert("title".to_string(), "Add a corpus".to_string());
  global.insert(
    "description".to_string(),
    "Register a new corpus and import its documents".to_string(),
  );
  if let Some(message) = error {
    global.insert("error".to_string(), message.to_string());
  }
  global.insert("name".to_string(), name.to_string());
  global.insert("path".to_string(), path.to_string());
  global.insert("corpus_description".to_string(), description.to_string());
  global.insert("complex".to_string(), complex.to_string());
  Template::render(
    "corpora-new",
    TemplateContext {
      global,
      is_admin: true,
      ..TemplateContext::default()
    },
  )
}

/// The "Add a corpus" form (`GET /corpora/new`). Signed-in admins only (anonymous → sign-in).
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/corpora/new")]
pub fn new_corpus_page(
  session: Option<AdminSession>,
  return_to: ReturnTo,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
  Ok(render_corpora_new(None, "", "", "", false))
}

/// The route set for the corpus-management capability (API + human screens).
pub fn routes() -> Vec<Route> {
  // NB: the agent `/api/corpora*` routes (read + write) are mounted via `frontend::apidoc`
  // (rocket_okapi) so they land in the generated OpenAPI spec; only the human screens + form posts
  // are in this plain route group.
  routes![
    import_corpus_human,
    extend_corpus_human,
    delete_corpus_human,
    activate_service_human,
    deactivate_service_human,
    create_sandbox_human,
    overview_page,
    new_corpus_page,
    corpus_page
  ]
}
