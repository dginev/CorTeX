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
//! `docs/archive/api-spike/COMPARISON.md` + OPEN_QUESTIONS #7; rocket_okapi was chosen over
//! utoipa).
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
use crate::frontend::admin::{api_status, okapi_add_operation_for_api_status_};
use crate::frontend::audit::{api_audit, okapi_add_operation_for_api_audit_};
use crate::frontend::corpora::{
  activate_service, api_corpora, api_corpus, create_sandbox_corpus, deactivate_service,
  delete_corpus, export_dataset, extend_corpus, import_corpus,
  okapi_add_operation_for_activate_service_, okapi_add_operation_for_api_corpora_,
  okapi_add_operation_for_api_corpus_, okapi_add_operation_for_create_sandbox_corpus_,
  okapi_add_operation_for_deactivate_service_, okapi_add_operation_for_delete_corpus_,
  okapi_add_operation_for_export_dataset_, okapi_add_operation_for_extend_corpus_,
  okapi_add_operation_for_import_corpus_, okapi_add_operation_for_snapshot_tasks_, snapshot_tasks,
};
use crate::frontend::jobs::{
  api_job, api_jobs, okapi_add_operation_for_api_job_, okapi_add_operation_for_api_jobs_,
};
use crate::frontend::management::{
  analyze, api_config, api_health, api_index, healthz, okapi_add_operation_for_analyze_,
  okapi_add_operation_for_api_config_, okapi_add_operation_for_api_health_,
  okapi_add_operation_for_api_index_, okapi_add_operation_for_healthz_,
  okapi_add_operation_for_put_config_, okapi_add_operation_for_reindex_, put_config, reindex,
};
use crate::frontend::reports::{
  api_category_report, api_document, api_entry_list, api_service_overview, api_what_report,
  okapi_add_operation_for_api_category_report_, okapi_add_operation_for_api_document_,
  okapi_add_operation_for_api_entry_list_, okapi_add_operation_for_api_service_overview_,
  okapi_add_operation_for_api_what_report_, okapi_add_operation_for_pause_run_api_,
  okapi_add_operation_for_refresh_reports_, okapi_add_operation_for_rerun_report_,
  okapi_add_operation_for_resume_run_api_, pause_run_api, refresh_reports, rerun_report,
  resume_run_api,
};
use crate::frontend::retention::{
  api_historical_stats, okapi_add_operation_for_api_historical_stats_,
};
use crate::frontend::runs::{
  api_all_runs, api_run_current, api_run_diff, api_run_task_diffs, api_runs,
  okapi_add_operation_for_api_all_runs_, okapi_add_operation_for_api_run_current_,
  okapi_add_operation_for_api_run_diff_, okapi_add_operation_for_api_run_task_diffs_,
  okapi_add_operation_for_api_runs_,
};
use crate::frontend::services::{
  api_service_workers, api_services, delete_service, okapi_add_operation_for_api_service_workers_,
  okapi_add_operation_for_api_services_, okapi_add_operation_for_delete_service_,
  okapi_add_operation_for_register_service_, register_service,
};
use crate::frontend::sessions::{
  api_revoke_sessions, api_sessions, okapi_add_operation_for_api_revoke_sessions_,
  okapi_add_operation_for_api_sessions_,
};

/// The generated OpenAPI document, serialized once at mount time and served verbatim.
struct SpecJson(String);

/// Serves the generated OpenAPI 3 document (the machine-readable API contract).
#[get("/api/openapi.json")]
fn openapi_json(spec: &State<SpecJson>) -> (ContentType, String) {
  (ContentType::JSON, spec.0.clone())
}

/// Agent onboarding copy for the OpenAPI `info.description` — the first thing a tool reading
/// `/api/openapi.json` (or browsing `/api/docs`) sees. `rocket_okapi` defaults to a bare title with
/// no description, so an agent had no in-spec orientation; this is the agent twin of the human
/// dashboard's at-a-glance context (authentication + where to start). Rendered as Markdown by
/// RapiDoc.
const AGENT_API_OVERVIEW: &str = "\
CorTeX is a distributed corpus-conversion framework for scholarly documents. This is its **agent \
API** — the machine twin of the human admin screens: every endpoint returns the *same* structured \
DTO a screen renders, so an agent and an operator always see identical live and historical state.\n\
\n\
## Authenticating\n\
\n\
Read-only report endpoints are public; **management and write** endpoints (and `/metrics`) are \
**token-gated**. Supply your token either way:\n\
\n\
- query string — `?token=<TOKEN>`\n\
- header — `X-Cortex-Token: <TOKEN>`\n\
\n\
A missing or invalid token returns `401`. Every write is attributed to an actor and recorded in the \
operational journal.\n\
\n\
## Where to start\n\
\n\
- `GET /api/status` — at-a-glance system snapshot (corpora, the active worker fleet, the \
pending-conversion backlog, the latest run).\n\
- `GET /api/health` — deep health check (connection pool, dispatcher ports, corpus storage).\n\
- `GET /api/corpora` and `GET /api/reports/<corpus>/<service>/<severity>` — the conversion report \
hierarchy (paginated).\n\
- `GET /api/runs` and `GET /api/runs/<corpus>/<service>/diff` — live and historical run state.\n\
- `GET /metrics` — Prometheus gauges.\n\
\n\
Conversion history (`/api/runs…`) is **append-only over the API** — never deletable or mutable via \
`/api` (pruning is a human-admin action). See `MANUAL.md` for the full operator and agent guide.";

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
  let (routes, mut spec) = openapi_get_routes_spec![
    settings:
    api_corpora,
    api_corpus,
    api_services,
    api_service_workers,
    api_jobs,
    api_job,
    api_all_runs,
    api_runs,
    api_run_current,
    api_run_diff,
    api_run_task_diffs,
    api_service_overview,
    api_category_report,
    api_what_report,
    api_entry_list,
    api_document,
    api_index,
    api_config,
    healthz,
    api_health,
    register_service,
    delete_service,
    import_corpus,
    extend_corpus,
    export_dataset,
    create_sandbox_corpus,
    activate_service,
    deactivate_service,
    snapshot_tasks,
    delete_corpus,
    rerun_report,
    pause_run_api,
    resume_run_api,
    refresh_reports,
    reindex,
    analyze,
    put_config,
    api_status,
    api_audit,
    api_sessions,
    api_revoke_sessions,
    api_historical_stats,
  ];
  // Give every operation a short one-line `summary` for the RapiDoc left-nav; the full doc comment
  // stays as the `description` in the detail panel. Without a summary RapiDoc fell back to the long
  // description, making the nav unreadable (U-2).
  add_nav_summaries(&mut spec);
  // The OpenAPI `info` is an agent's first contact with the API — fill in the title + an onboarding
  // description (authentication + entry points), which `rocket_okapi` otherwise leaves bare.
  spec.info.title = "CorTeX agent API".to_string();
  spec.info.description = Some(AGENT_API_OVERVIEW.to_string());
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

/// Give every operation in `spec` a short one-line `summary` (for the RapiDoc left-nav) derived
/// from its long `description`, leaving the description intact for the detail panel. Idempotent —
/// only fills an empty summary.
fn add_nav_summaries(spec: &mut rocket_okapi::okapi::openapi3::OpenApi) {
  for path_item in spec.paths.values_mut() {
    for op in [
      path_item.get.as_mut(),
      path_item.post.as_mut(),
      path_item.put.as_mut(),
      path_item.delete.as_mut(),
      path_item.patch.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
      if op.summary.is_none() {
        if let Some(description) = op.description.as_deref() {
          op.summary = Some(short_summary(description));
        }
      }
    }
  }
}

/// A terse nav label from a long description: drops a leading `` `METHOD /path` `` code-span (the
/// nav already shows the method + path) and markdown emphasis, keeps the first sentence, and caps
/// the length so the RapiDoc left-nav stays readable.
fn short_summary(description: &str) -> String {
  let mut text = description.trim();
  // Strip a leading backtick code span (e.g. "`GET /api/status`") + a following em-dash/colon/dash.
  if let Some(after_tick) = text.strip_prefix('`') {
    if let Some(end) = after_tick.find('`') {
      let rest = after_tick[end + 1..].trim_start();
      let rest = rest
        .strip_prefix('—')
        .or_else(|| rest.strip_prefix(':'))
        .or_else(|| rest.strip_prefix('-'))
        .unwrap_or(rest)
        .trim_start();
      if !rest.is_empty() {
        text = rest;
      }
    }
  }
  // First sentence / line, minus trailing punctuation and markdown emphasis.
  let first = text
    .split(['.', '\n'])
    .next()
    .unwrap_or(text)
    .trim()
    .trim_end_matches([',', ';', ':']);
  let cleaned: String = first
    .chars()
    .filter(|c| !matches!(c, '`' | '*' | '_'))
    .collect();
  let cleaned = cleaned.trim();
  const CAP: usize = 64;
  if cleaned.chars().count() > CAP {
    let truncated: String = cleaned.chars().take(CAP - 1).collect();
    format!("{}…", truncated.trim_end())
  } else {
    cleaned.to_string()
  }
}

#[cfg(test)]
mod summary_tests {
  use super::short_summary;

  #[test]
  fn derives_terse_nav_labels() {
    // A leading "`GET /path` — …" code span is dropped (the nav already shows the path).
    assert_eq!(
      short_summary("`GET /api/status` — the agent twin of the dashboard feed. More detail here."),
      "the agent twin of the dashboard feed"
    );
    // A short plain description keeps its first sentence.
    assert_eq!(
      short_summary("Lists all registered corpora."),
      "Lists all registered corpora"
    );
    // A long first sentence is capped with an ellipsis.
    let long = "Lists every single registered corpus across the whole deployment with all of its many associated services";
    let summary = short_summary(long);
    assert!(
      summary.chars().count() <= 64 && summary.ends_with('…'),
      "got {summary:?}"
    );
  }
}
