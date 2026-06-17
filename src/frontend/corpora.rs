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
  create_sandbox, export_html_dataset, from_address, progress_report, DatabaseUrl, DbPool, GroupBy,
  SandboxSelection,
};
use crate::concerns::CortexInsertable;
use crate::frontend::actor::{require_admin_to, Actor, AdminReject, AdminSession, ReturnTo};
use crate::frontend::helpers::{decorate_uri_encodings, uri_escape};
use crate::frontend::jobs::JobDto;
use crate::frontend::params::TemplateContext;
use crate::helpers::TaskStatus;
use crate::importer::Importer;
use crate::jobs::{self, JobProgress};
use crate::models::{Corpus, NewCorpus, Service};
use rocket_okapi::openapi;
use schemars::JsonSchema;
use std::path::PathBuf;

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
  /// Name of the parent corpus if this is a **sandbox** (a carved subset), else `null`. Lets a
  /// caller tell sandboxes from ordinary corpora — and find their parent — from the list alone,
  /// without a per-corpus detail fetch.
  pub parent: Option<String>,
}

impl CorpusDto {
  /// Builds the DTO, attaching the document count looked up from the batched per-corpus map and the
  /// parent corpus name (for sandboxes; `None` otherwise — resolve it from the same `Corpus::all`
  /// listing so there is no extra query). Public so the `cortex` CLI can emit the identical shape
  /// (web ↔ CLI ↔ agent parity).
  pub fn build(corpus: Corpus, document_count: i64, parent: Option<String>) -> Self {
    CorpusDto {
      public_id: corpus.public_id.to_string(),
      name: corpus.name,
      path: corpus.path,
      description: corpus.description,
      complex: corpus.complex,
      document_count,
      parent,
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
  // id → name over the loaded listing, so a sandbox's parent name resolves with no extra query.
  let names_by_id: HashMap<i32, String> = corpora.iter().map(|c| (c.id, c.name.clone())).collect();
  Json(
    corpora
      .into_iter()
      .map(|corpus| {
        let count = counts.get(&corpus.id).copied().unwrap_or(0);
        let parent = corpus
          .parent_corpus_id
          .and_then(|pid| names_by_id.get(&pid).cloned());
        CorpusDto::build(corpus, count, parent)
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

/// Provenance of a **sandbox** corpus: the parent it was carved from and the carve predicate. Only
/// present on sandbox corpora (`null` for ordinary corpora).
#[derive(Debug, Serialize, JsonSchema)]
pub struct SandboxProvenanceDto {
  /// Name of the parent corpus this sandbox was carved from.
  pub parent: String,
  /// Compact human-readable summary of the carve filter (e.g. `severity=warning, entry~2506.`).
  pub filter: String,
  /// The structured selection predicate (`service_id`, `severity`, `category`, `what`, `entry`,
  /// `max_entries`) — the same JSON stored on the corpus.
  pub selection: Option<serde_json::Value>,
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
  /// Sandbox provenance (parent + carve filter), or `null` if this is an ordinary corpus.
  pub sandbox: Option<SandboxProvenanceDto>,
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
  let sandbox =
    sandbox_provenance(&corpus, &mut connection).map(|(parent, filter)| SandboxProvenanceDto {
      parent,
      filter,
      selection: corpus.selection.clone(),
    });
  Ok(Json(CorpusDetailDto {
    name: corpus.name,
    path: corpus.path,
    description: corpus.description,
    complex: corpus.complex,
    sandbox,
    services: service_status,
  }))
}

/// A corpus's sandbox provenance — the parent name + a human-readable carve-filter summary — or
/// `None` if it is an ordinary (non-sandbox) corpus. Shared by the agent detail API and the human
/// corpus page so both surfaces show identical provenance.
fn sandbox_provenance(corpus: &Corpus, connection: &mut PgConnection) -> Option<(String, String)> {
  let parent_id = corpus.parent_corpus_id?;
  let parent = Corpus::find_by_id(parent_id, connection).ok()?;
  let filter = corpus
    .selection
    .as_ref()
    .and_then(|value| serde_json::from_value::<SandboxSelection>(value.clone()).ok())
    .map(|selection| selection.filter_summary())
    .unwrap_or_default();
  Some((parent.name, filter))
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
  // Reject a blank name on the agent path too (the HTML form enforces `required`, but a raw
  // `POST /api/corpora` bypasses that) — an empty handle is unreachable by every name-keyed route.
  if name.trim().is_empty() {
    return Err(Status::BadRequest);
  }
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

/// Request body for exporting a corpus/service's converted HTML into ZIP archives
/// ([`export_dataset`]). Mirrors the `cortex export-dataset` CLI flags.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExportRequest {
  /// Server-side output directory for the archives + the `<corpus>-manifest.json` sidecar (created
  /// if missing).
  pub out: String,
  /// Bucketing: `month` (one archive per year-month) or `severity` (one per severity). Defaults to
  /// `month`.
  #[serde(default)]
  pub group_by: Option<String>,
  /// Severity keys to include (canonical: `no_problem` | `warning` | `error` | `fatal` |
  /// `invalid`). Defaults to `no_problem,warning,error` (matching the CLI).
  #[serde(default)]
  pub severities: Option<Vec<String>>,
  /// Optional per-archive size cap in **MB**: when set, each month/severity bucket is split into
  /// numbered chunks `<corpus>-<key>-NNN.zip` once it exceeds this many MB of (uncompressed) HTML
  /// — the published `.zip` is smaller. Omit for one archive per bucket (no size limit).
  #[serde(default)]
  pub max_archive_mb: Option<u64>,
}

/// The default severity set when a caller omits `severities` — kept identical to the
/// `cortex export-dataset` CLI default so all three surfaces export the same slice by default.
fn default_export_severities() -> Vec<String> {
  vec![
    "no_problem".to_string(),
    "warning".to_string(),
    "error".to_string(),
  ]
}

/// Exports a corpus/service's already-converted HTML into ZIP archives off the shared filesystem as
/// an in-process **background job** (no conversion is run); returns `202 Accepted` + the job
/// handle, which agents and humans poll via `GET /api/jobs/<uuid>`. The agent twin of `cortex
/// export-dataset` (and the future web form), over the same [`export_html_dataset`] core.
/// **Token-gated** via the [`Actor`] guard (it reads `/data` and writes archives server-side);
/// `401` without a valid token, `404` if the corpus or service is unknown, `422` for an invalid
/// `group_by` or severity key (pre-flighted so a doomed export never starts).
#[openapi(tag = "Corpora")]
#[post(
  "/api/corpora/<corpus>/services/<service>/export-dataset",
  format = "json",
  data = "<request>"
)]
pub fn export_dataset(
  corpus: &str,
  service: &str,
  request: Json<ExportRequest>,
  actor: Actor,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<(Status, Json<JobDto>), Status> {
  let job_uuid = start_export(
    pool,
    &database_url.0,
    &actor.owner,
    corpus,
    service,
    request.into_inner(),
  )?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let job = jobs::find_job(&mut connection, job_uuid).ok_or(Status::InternalServerError)?;
  Ok((Status::Accepted, Json(JobDto::from(job))))
}

/// Validates the export request, resolves the corpus/service, and spawns the `dataset_export`
/// background job — the shared core of the agent endpoint and the (future) human form. `404` if the
/// corpus/service is unknown; `422` for a bad `group_by`/severity (so the caller gets immediate
/// feedback instead of a job that fails late).
fn start_export(
  pool: &DbPool,
  database_url: &str,
  actor: &str,
  corpus_name: &str,
  service_name: &str,
  request: ExportRequest,
) -> Result<Uuid, Status> {
  // Pre-flight the knobs (mirrors the CLI's exit-2 validation) before any DB work or job spawn.
  let group_by_key = request.group_by.unwrap_or_else(|| "month".to_string());
  let group_by = GroupBy::from_key(&group_by_key).ok_or(Status::UnprocessableEntity)?;
  let severity_keys = request.severities.unwrap_or_else(default_export_severities);
  let severities = severity_keys
    .iter()
    .map(|key| TaskStatus::from_key(key))
    .collect::<Option<Vec<_>>>()
    .ok_or(Status::UnprocessableEntity)?;
  if severities.is_empty() {
    return Err(Status::UnprocessableEntity);
  }

  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let corpus = Corpus::find_by_name(&corpus_name.to_lowercase(), &mut connection)
    .map_err(|_| Status::NotFound)?;
  let service = Service::find_by_name(&service_name.to_lowercase(), &mut connection)
    .map_err(|_| Status::NotFound)?;
  drop(connection);

  let out = PathBuf::from(request.out);
  let max_archive_mb = request.max_archive_mb;
  let database_url = database_url.to_string();
  let params = serde_json::json!({
    "corpus": corpus.name,
    "service": service.name,
    "out": out.display().to_string(),
    "group_by": group_by_key,
    "severities": severity_keys,
    "max_archive_mb": max_archive_mb,
  });
  jobs::spawn_job(
    pool.clone(),
    "dataset_export",
    actor,
    params,
    move |progress| {
      run_export(
        &database_url,
        corpus,
        service,
        severities,
        group_by,
        max_archive_mb,
        out,
        progress,
      )
    },
  )
  .map_err(|_| Status::InternalServerError)
}

/// The body of a `dataset_export` job: stream the corpus/service HTML into archives, threading the
/// exporter's milestone lines through the job's progress feed, and return the
/// [`DatasetExportOutcome`](crate::backend::DatasetExportOutcome) (archives + tallies) as the job
/// result.
#[allow(clippy::too_many_arguments)] // mirrors export_html_dataset's knobs; a struct is ceremony here
fn run_export(
  database_url: &str,
  corpus: Corpus,
  service: Service,
  severities: Vec<TaskStatus>,
  group_by: GroupBy,
  max_archive_mb: Option<u64>,
  out: PathBuf,
  progress: &JobProgress,
) -> Result<Value, String> {
  let mut backend = from_address(database_url);
  let outcome = export_html_dataset(
    &mut backend.connection,
    &corpus,
    &service,
    &severities,
    group_by,
    max_archive_mb,
    &out,
    |line| progress.step(0, None, line),
  )?;
  let total = outcome.total_entries as i32;
  progress.step(total, Some(total), "export complete");
  serde_json::to_value(&outcome).map_err(|error| error.to_string())
}

/// Fields of the human "Export dataset" form (the web twin of [`export_dataset`]).
#[derive(FromForm)]
pub struct ExportForm {
  /// Server-side output directory for the archives.
  pub out: String,
  /// Bucketing: `month` or `severity`.
  pub group_by: String,
  /// The checked severity keys (the multi-value checkbox group; empty = none selected).
  pub severities: Vec<String>,
  /// Optional per-archive size cap in MB (blank = no limit). A string so a blank field parses to
  /// "no limit" instead of a form error.
  pub max_archive_mb: Option<String>,
}

/// Renders the "Export dataset" form for a `(corpus, service)`. Shared by the GET screen and the
/// POST error path, so a failed submit re-renders with a friendly `error` and every typed value
/// preserved (the output path, grouping, and which severities were checked) instead of a bare error
/// page. Admin-only page (`is_admin`).
fn render_export_form(
  corpus: &str,
  service: &str,
  error: Option<&str>,
  out: &str,
  group_by: &str,
  selected: &[String],
  max_archive_mb: &str,
) -> Template {
  let mut global = HashMap::new();
  global.insert("title".to_string(), "Export dataset".to_string());
  global.insert(
    "description".to_string(),
    format!("Bundle {corpus} / {service} converted HTML into ZIP archives"),
  );
  global.insert("corpus_name".to_string(), corpus.to_string());
  global.insert("service_name".to_string(), service.to_string());
  global.insert("out".to_string(), out.to_string());
  global.insert("group_by".to_string(), group_by.to_string());
  global.insert("max_archive_mb".to_string(), max_archive_mb.to_string());
  if let Some(message) = error {
    global.insert("error".to_string(), message.to_string());
  }
  // Per-severity checked flags (the form preserves the admin's selection across a re-render). The
  // key set matches the canonical `TaskStatus` severities the exporter accepts.
  for key in ["no_problem", "warning", "error", "fatal", "invalid"] {
    let checked = selected.iter().any(|s| s == key);
    global.insert(format!("sev_{key}_checked"), checked.to_string());
  }
  let mut context = TemplateContext {
    global,
    is_admin: true,
    ..TemplateContext::default()
  };
  decorate_uri_encodings(&mut context);
  Template::render("export-dataset", context)
}

/// The "Export dataset" screen (`GET /export/<c>/<s>`): the human form that drives the same export
/// as `cortex export-dataset` and the agent [`export_dataset`]. **Signed-in admins only**
/// (anonymous → sign-in). Pre-fills a default output path + the CLI's default severity set. A
/// sibling top-level path (like `/runs/<c>/<s>`, `/history/<c>/<s>`) so it never collides with the
/// report ladder's `/corpus/<c>/<s>/<severity>` rung.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/export/<corpus>/<service>")]
pub fn export_dataset_page(
  corpus: &str,
  service: &str,
  session: Option<AdminSession>,
  return_to: ReturnTo,
) -> Result<Template, AdminReject> {
  require_admin_to(session, &return_to)?;
  let default_out = format!("/data/datasets/{corpus}-{service}");
  Ok(render_export_form(
    corpus,
    service,
    None,
    &default_out,
    "month",
    &default_export_severities(),
    "",
  ))
}

/// The human twin of [`export_dataset`]: the "Export dataset" form post. **Gated by the signed-in
/// [`AdminSession`] cookie** (anonymous → sign-in); spawns the background export via the shared
/// [`start_export`] core and redirects to the job's live-progress page. A failed submit re-renders
/// the form with a friendly message + the values preserved (404 unknown corpus/service, 422 bad
/// grouping / no severity).
// The Err variant is a re-rendered form `Template` (the friendly-error path), which is chunky —
// fine for a one-shot request handler.
#[allow(clippy::result_large_err)]
#[post("/export/<corpus>/<service>", data = "<form>")]
pub fn export_dataset_human(
  corpus: &str,
  service: &str,
  form: Form<ExportForm>,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
  database_url: &State<DatabaseUrl>,
) -> Result<Redirect, Template> {
  let Some(session) = session else {
    return Ok(Redirect::to("/admin/login"));
  };
  let form = form.into_inner();
  let request = ExportRequest {
    out: form.out.clone(),
    group_by: Some(form.group_by.clone()),
    severities: Some(form.severities.clone()),
    // Blank or non-numeric → no limit (a number input keeps it numeric in practice).
    max_archive_mb: form
      .max_archive_mb
      .as_deref()
      .map(str::trim)
      .filter(|s| !s.is_empty())
      .and_then(|s| s.parse::<u64>().ok()),
  };
  match start_export(
    pool,
    &database_url.0,
    &session.owner,
    corpus,
    service,
    request,
  ) {
    Ok(uuid) => Ok(Redirect::to(format!("/jobs/{uuid}"))),
    Err(status) => {
      let message = match status.code {
        404 => format!("Corpus “{corpus}” / service “{service}” not found — check the names."),
        422 => "Pick a grouping and at least one valid severity.".to_string(),
        503 => "The database is temporarily unavailable — please try again.".to_string(),
        _ => "Could not start the export — check the values and try again.".to_string(),
      };
      Err(render_export_form(
        corpus,
        service,
        Some(&message),
        &form.out,
        &form.group_by,
        &form.severities,
        form.max_archive_mb.as_deref().unwrap_or(""),
      ))
    },
  }
}

/// Request body for carving a **sandbox** corpus out of a parent by a filter (Arm 5). Task-status
/// and message-severity are independent, intersecting dimensions (Model C); `category`/`what`
/// narrow the message filter, mirroring the report drill-down.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SandboxRequest {
  /// Name for the new sandbox corpus (its external handle; must be unique).
  pub name: String,
  /// The service whose conversion results are filtered.
  pub service_id: i32,
  /// Optional **task-status** filter (`no_problem` | `warning` | `error` | `fatal` | `invalid`).
  #[serde(default)]
  pub status: Option<String>,
  /// Optional **message-severity** filter (`info` | `warning` | `error` | `fatal` | `invalid`) —
  /// matches tasks that emitted such a message, at any status. `category`/`what` narrow within it.
  #[serde(default)]
  pub message_severity: Option<String>,
  /// Optional message-category narrowing (needs `message_severity`).
  #[serde(default)]
  pub category: Option<String>,
  /// Optional `what` narrowing within the category (needs `category`).
  #[serde(default)]
  pub what: Option<String>,
  /// Optional substring the parent `entry` path must contain (`entry LIKE '%…%'`, e.g. `2506.` for
  /// one arXiv month). Empty/absent = no narrowing.
  #[serde(default)]
  pub entry: Option<String>,
  /// Optional hard cap on the number of entries captured (the first `n` by `entry` order). Absent
  /// or non-positive = no cap.
  #[serde(default)]
  pub max_entries: Option<i64>,
}

impl From<&SandboxRequest> for SandboxSelection {
  fn from(request: &SandboxRequest) -> Self {
    SandboxSelection {
      service_id: request.service_id,
      status: request.status.clone(),
      message_severity: request.message_severity.clone(),
      category: request.category.clone(),
      what: request.what.clone(),
      entry: request.entry.clone(),
      max_entries: request.max_entries,
      severity: None,
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
  // A blank sandbox name is unreachable junk — reject it (the web form enforces `required`).
  if request.name.trim().is_empty() {
    return Err(Status::BadRequest);
  }
  if Corpus::find_by_name(&request.name, &mut connection).is_ok() {
    return Err(Status::Conflict);
  }
  drop(connection);

  let database_url = database_url.to_string();
  let name = request.name.clone();
  let selection = SandboxSelection::from(request);
  // Pre-flight the selection (mirrors import/export) so a bad filter is an immediate 422, not a
  // `202` that an agent has to poll only to find the job failed.
  selection
    .validate()
    .map_err(|_| Status::UnprocessableEntity)?;
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
  /// Optional task-status filter (empty string = none).
  pub status: Option<String>,
  /// Optional message-severity filter (empty string = none).
  pub message_severity: Option<String>,
  /// Optional category narrowing (empty string = none).
  pub category: Option<String>,
  /// Optional `what` narrowing (empty string = none).
  pub what: Option<String>,
  /// Optional `entry` substring filter (empty string = none).
  pub entry: Option<String>,
  /// Optional hard cap on captured entries (empty/zero = none).
  pub max_entries: Option<i64>,
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
    status: blank_to_none(form.status),
    message_severity: blank_to_none(form.message_severity),
    category: blank_to_none(form.category),
    what: blank_to_none(form.what),
    entry: blank_to_none(form.entry),
    // A non-positive cap is treated as "no cap" (create_sandbox ignores it); keep the raw value.
    max_entries: form.max_entries.filter(|n| *n > 0),
  };
  match start_sandbox(pool, &database_url.0, &session.owner, parent, &request) {
    Ok(uuid) => Ok(Redirect::to(format!("/jobs/{uuid}"))),
    // A name collision re-shows the corpus page with a friendly flash instead of a bare 409 page —
    // the same courtesy the import form gives. The agent twin keeps its 409 status.
    Err(status) if status == Status::Conflict => Ok(Redirect::to(format!(
      "/corpus/{}?sandbox_taken={}",
      uri_escape(Some(parent.to_string())).unwrap_or_default(),
      uri_escape(Some(request.name)).unwrap_or_default()
    ))),
    Err(status) => Err(status),
  }
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
  // Pre-flight the corpus source path (extend re-scans it for new entries). If the data mount is
  // gone/unreadable, fail transparently with `422` instead of spawning a job that silently finds
  // nothing (`glob` over a missing dir yields an empty set, not an error) and reports "0 new" — the
  // same courtesy as import, so a vanished mount surfaces as an error rather than a quiet no-op.
  if !std::fs::metadata(corpus.path.trim_end())
    .map(|meta| meta.is_dir())
    .unwrap_or(false)
  {
    return Err(Status::UnprocessableEntity);
  }
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
  // Refuse a mid-run snapshot (in-progress tasks would resolve to a different status moments
  // later), matching the human `serve_savetasks` guard so both surfaces agree. `409` while any
  // task is TODO or Queued (status >= 0).
  let progress = progress_report(&mut backend.connection, corpus_record.id, service_record.id);
  let in_progress =
    progress.get("todo").copied().unwrap_or(0.0) + progress.get("queued").copied().unwrap_or(0.0);
  if in_progress > 0.0 {
    return Err(Status::Conflict);
  }
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
  let all = Corpus::all(&mut connection).unwrap_or_default();
  // id → name over the loaded listing, so a sandbox's parent name resolves with no extra query.
  let names_by_id: HashMap<i32, String> = all.iter().map(|c| (c.id, c.name.clone())).collect();
  let corpora = all
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
      // Mark sandboxes (carved subsets) so the list can badge them + link to their parent — the
      // human twin of `CorpusDto.parent`. `decorate_uri_encodings` adds `sandbox_parent_uri`.
      if let Some(parent) = corpus
        .parent_corpus_id
        .and_then(|pid| names_by_id.get(&pid))
      {
        hash.insert("sandbox_parent".to_string(), parent.clone());
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
#[get("/corpus/<name>?<deactivated>&<sandbox_taken>")]
pub fn corpus_page(
  name: &str,
  deactivated: Option<&str>,
  sandbox_taken: Option<&str>,
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
  // `?sandbox_taken=<name>` flashes a friendly error after a sandbox name collision (the human form
  // redirects here instead of dumping a bare 409 page).
  if let Some(taken) = sandbox_taken {
    global.insert(
      "sandbox_error".to_string(),
      format!("A corpus named “{taken}” already exists — choose a different sandbox name."),
    );
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
  // The filesystem root the "Extend" action re-scans — named in the corpus-actions form so an admin
  // sees exactly which directory will be walked.
  global.insert("corpus_path".to_string(), corpus.path.clone());
  // Sandbox provenance: if this corpus was carved from a parent, surface the parent + carve filter
  // (the agent twin `api_corpus` exposes the same via `CorpusDetailDto.sandbox`). `sandbox_parent`
  // gets a `_uri` variant from `decorate_uri_encodings` for the parent link.
  if let Some((parent, filter)) = sandbox_provenance(&corpus, &mut connection) {
    global.insert("sandbox_parent".to_string(), parent);
    global.insert("sandbox_filter".to_string(), filter);
  }
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
    export_dataset_page,
    export_dataset_human,
    overview_page,
    new_corpus_page,
    corpus_page
  ]
}
