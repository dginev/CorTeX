// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! API documentation: a generated **OpenAPI 3** spec plus a RapiDoc browser page, both built by
//! `rocket_okapi` directly from the `#[openapi]`-annotated agent routes — the spec is generated
//! from the single source of truth (the real Rocket route + its return type) and so can never drift
//! from the served API. This is the symmetry contract extended to the docs (see
//! `docs/api-spike/COMPARISON.md` + OPEN_QUESTIONS #7; rocket_okapi was chosen over utoipa).
//!
//! Adding an endpoint to the docs is two steps: put `#[openapi(tag = "…")]` above its
//! `#[get/post/…]` attribute, and list its handler in the [`openapi_get_routes_spec!`] call in
//! [`mount`]. The DTOs it returns must derive `schemars::JsonSchema`.

use rocket::http::ContentType;
use rocket::{Build, Rocket, State};
use rocket_okapi::openapi_get_routes_spec;
use rocket_okapi::rapidoc::{make_rapidoc, GeneralConfig, RapiDocConfig};
use rocket_okapi::settings::{OpenApiSettings, UrlObject};

// Each documented handler is imported alongside its `#[openapi]`-generated
// `okapi_add_operation_for_*` companion (emitted in the handler's own module) — both must be in
// scope for the `openapi_get_routes_spec!` call below. (Explicit rather than glob, so each module's
// same-named `routes` fn doesn't collide.)
use crate::frontend::corpora::{
  activate_service, api_corpora, api_corpus, deactivate_service, delete_corpus, extend_corpus,
  import_corpus, okapi_add_operation_for_activate_service_, okapi_add_operation_for_api_corpora_,
  okapi_add_operation_for_api_corpus_, okapi_add_operation_for_deactivate_service_,
  okapi_add_operation_for_delete_corpus_, okapi_add_operation_for_extend_corpus_,
  okapi_add_operation_for_import_corpus_,
};
use crate::frontend::jobs::{
  api_job, api_jobs, okapi_add_operation_for_api_job_, okapi_add_operation_for_api_jobs_,
};
use crate::frontend::management::{
  analyze, api_config, api_index, healthz, okapi_add_operation_for_analyze_,
  okapi_add_operation_for_api_config_, okapi_add_operation_for_api_index_,
  okapi_add_operation_for_healthz_, okapi_add_operation_for_put_config_,
  okapi_add_operation_for_reindex_, put_config, reindex,
};
use crate::frontend::reports::{
  api_category_report, api_what_report, okapi_add_operation_for_api_category_report_,
  okapi_add_operation_for_api_what_report_, okapi_add_operation_for_refresh_reports_,
  okapi_add_operation_for_rerun_report_, refresh_reports, rerun_report,
};
use crate::frontend::runs::{
  api_run_current, api_run_diff, api_run_task_diffs, api_runs,
  okapi_add_operation_for_api_run_current_, okapi_add_operation_for_api_run_diff_,
  okapi_add_operation_for_api_run_task_diffs_, okapi_add_operation_for_api_runs_,
};
use crate::frontend::services::{
  api_service_workers, api_services, okapi_add_operation_for_api_service_workers_,
  okapi_add_operation_for_api_services_, okapi_add_operation_for_register_service_,
  register_service,
};

/// The generated OpenAPI document, serialized once at mount time and served verbatim.
struct SpecJson(String);

/// Serves the generated OpenAPI 3 document (the machine-readable API contract).
#[get("/api/openapi.json")]
fn openapi_json(spec: &State<SpecJson>) -> (ContentType, String) {
  (ContentType::JSON, spec.0.clone())
}

/// Mounts the generated agent-API documentation onto `rocket`:
/// - the `#[openapi]`-annotated agent routes (so they exist *and* are documented from one source),
/// - the OpenAPI 3 spec at `GET /api/openapi.json`,
/// - a RapiDoc browser page at `GET /api/docs`.
///
/// The annotated routes are mounted here (not in their modules' plain route groups) so
/// `rocket_okapi` can attach their operation metadata.
pub fn mount(rocket: Rocket<Build>) -> Rocket<Build> {
  let settings = OpenApiSettings::default();
  // Every `#[openapi]` agent handler is listed here; the macro returns the routes + the spec built
  // from them. (Expand this list as more endpoints are annotated.)
  let (routes, spec) = openapi_get_routes_spec![
    settings:
    api_corpora,
    api_corpus,
    api_services,
    api_service_workers,
    api_jobs,
    api_job,
    api_runs,
    api_run_current,
    api_run_diff,
    api_run_task_diffs,
    api_category_report,
    api_what_report,
    api_index,
    api_config,
    healthz,
    register_service,
    import_corpus,
    extend_corpus,
    activate_service,
    deactivate_service,
    delete_corpus,
    rerun_report,
    refresh_reports,
    reindex,
    analyze,
    put_config,
  ];
  let spec_json = serde_json::to_string_pretty(&spec).unwrap_or_default();
  rocket
    .manage(SpecJson(spec_json))
    .mount("/", routes)
    .mount("/", routes![openapi_json])
    .mount(
      "/api/docs",
      make_rapidoc(&RapiDocConfig {
        title: Some("CorTeX agent API".to_string()),
        general: GeneralConfig {
          spec_urls: vec![UrlObject::new("CorTeX API", "/api/openapi.json")],
          ..Default::default()
        },
        ..Default::default()
      }),
    )
}
