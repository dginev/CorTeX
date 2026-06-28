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
use rocket_okapi::rapidoc::{
  GeneralConfig, NavConfig, NavItemSpacing, RapiDocConfig, SlotsConfig, make_rapidoc,
};
use rocket_okapi::settings::{OpenApiSettings, UrlObject};

// Each documented handler is imported alongside its `#[openapi]`-generated
// `okapi_add_operation_for_*` companion (emitted in the handler's own module) — both must be in
// scope for the `openapi_get_routes_spec!` call below. (Explicit rather than glob, so each module's
// same-named `routes` fn doesn't collide.)
use crate::frontend::admin::{
  api_logs, api_status, okapi_add_operation_for_api_logs_, okapi_add_operation_for_api_status_,
};
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
  okapi_add_operation_for_api_what_report_, okapi_add_operation_for_pause_all_api_,
  okapi_add_operation_for_pause_run_api_, okapi_add_operation_for_refresh_report_scope_api_,
  okapi_add_operation_for_refresh_reports_, okapi_add_operation_for_rerun_report_,
  okapi_add_operation_for_resume_all_api_, okapi_add_operation_for_resume_run_api_, pause_all_api,
  pause_run_api, refresh_report_scope_api, refresh_reports, rerun_report, resume_all_api,
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
  api_service_runtimes, api_service_workers, api_services, delete_service,
  okapi_add_operation_for_api_service_runtimes_, okapi_add_operation_for_api_service_workers_,
  okapi_add_operation_for_api_services_, okapi_add_operation_for_delete_service_,
  okapi_add_operation_for_register_service_, okapi_add_operation_for_set_service_lease_,
  register_service, set_service_lease,
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

/// A small script injected into the RapiDoc page's default slot. RapiDoc's `theme` is a fixed
/// Light/Dark with no auto mode and no toggle, so this: (1) follows the viewer's OS/browser
/// light/dark preference, (2) adds a persistent top-right toggle button that overrides it
/// (`localStorage`), and (3) sets a readable `primary-color` per mode — the default dark blue is
/// too dark on a dark background, washing out links. The static gh-pages page
/// (`scripts/build-docs-site.sh`) carries the same logic, so both surfaces behave identically.
const RAPIDOC_THEME_SCRIPT: &str = "<script>\
(function(){\
var rd=document.getElementById('rapidoc');if(!rd)return;\
var mq=window.matchMedia('(prefers-color-scheme: dark)');\
var KEY='cortex-docs-theme';var stored=null;try{stored=localStorage.getItem(KEY);}catch(e){}\
function theme(){return stored||(mq.matches?'dark':'light');}\
var btn=document.createElement('button');btn.id='theme-toggle';btn.type='button';\
btn.setAttribute('aria-label','Toggle light/dark theme');\
btn.style.cssText='position:fixed;top:.55rem;right:.7rem;z-index:20;font:13px sans-serif;\
padding:.3rem .6rem;border-radius:6px;border:1px solid rgba(128,128,128,.5);\
background:rgba(128,128,128,.15);color:inherit;cursor:pointer';\
function apply(){var t=theme();rd.setAttribute('theme',t);\
rd.setAttribute('primary-color',t==='dark'?'#6ab0f3':'#2a5d84');\
btn.textContent=t==='dark'?'☀ Light':'☾ Dark';}\
btn.addEventListener('click',function(){stored=theme()==='dark'?'light':'dark';\
try{localStorage.setItem(KEY,stored);}catch(e){}apply();});\
document.body.appendChild(btn);apply();\
mq.addEventListener('change',function(){if(!stored)apply();});\
})();\
</script>";

/// Builds the agent-API routes **and** the OpenAPI 3 spec from the single `#[openapi]` handler
/// list, then applies the nav summaries + `info` metadata. The one source of truth shared by
/// [`mount`] (which serves both live) and [`spec_json`] (which serializes the spec for static
/// publishing) — so the published docs can never drift from the served API.
fn routes_and_spec() -> (Vec<rocket::Route>, rocket_okapi::okapi::openapi3::OpenApi) {
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
    set_service_lease,
    api_service_runtimes,
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
    pause_all_api,
    resume_all_api,
    refresh_reports,
    refresh_report_scope_api,
    reindex,
    analyze,
    put_config,
    api_status,
    api_logs,
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
  (routes, spec)
}

/// The generated OpenAPI 3 document as pretty-printed JSON — the same bytes served live at
/// `GET /api/openapi.json`, but obtainable **without a running server or database** (the spec is
/// built purely from the route definitions). This is what `cortex openapi` prints and what the
/// published docs site (`scripts/build-docs-site.sh`) bundles, so the static docs stay in lock-step
/// with the served API.
pub fn spec_json() -> String {
  serde_json::to_string_pretty(&routes_and_spec().1).unwrap_or_default()
}

/// Mounts the generated agent-API documentation onto `rocket`:
/// - the `#[openapi]`-annotated agent routes (so they exist *and* are documented from one source),
/// - the OpenAPI 3 spec at `GET /api/openapi.json`,
/// - a RapiDoc browser page at `GET /api/docs`.
///
/// The annotated routes are mounted here (not in their modules' plain route groups) so
/// `rocket_okapi` can attach their operation metadata.
pub fn mount(rocket: Rocket<Build>) -> Rocket<Build> {
  let (routes, spec) = routes_and_spec();
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
        // The left nav is a route dictionary (`METHOD /path`), not prose: predictable to scan,
        // one line per endpoint. The descriptive summary + full description stay in the detail
        // panel. Compact spacing keeps the (40-route) list dense.
        nav: NavConfig {
          use_path_in_nav_bar: true,
          nav_item_spacing: NavItemSpacing::Compact,
          ..Default::default()
        },
        // Follow the viewer's OS/browser light/dark preference (the bundled template's `theme` is
        // otherwise fixed). The script lives in the default slot, which RapiDoc renders invisibly.
        slots: SlotsConfig {
          default: vec![RAPIDOC_THEME_SCRIPT.to_string()],
          ..Default::default()
        },
        ..Default::default()
      }),
    )
}

/// Give every operation in `spec` a one-line `summary` derived from its long `description`,
/// leaving the description intact for the detail panel. Idempotent — only fills an empty summary.
///
/// The summary is the operation's heading in the RapiDoc detail panel and the `summary` field in
/// the spec (useful to OpenAPI tooling); the left **nav** is path-based (`use_path_in_nav_bar`), so
/// it no longer depends on this text being short — hence the generous length guard in
/// [`short_summary`] rather than the old hard nav-width cap.
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
      if op.summary.is_none()
        && let Some(description) = op.description.as_deref()
      {
        op.summary = Some(short_summary(description));
      }
    }
  }
}

/// A one-line summary from a long description: drops a leading `` `METHOD /path` `` code-span (the
/// path is already shown in the nav and the detail-panel header) and markdown emphasis, then keeps
/// the lead clause up to the first clause boundary (`. ; : —`). That yields a terse, complete
/// heading rather than a run-on; a generous length guard only ellipsizes a pathologically long lead
/// clause that has no early boundary.
fn short_summary(description: &str) -> String {
  let mut text = description.trim();
  // Strip a leading backtick code span (e.g. "`GET /api/status`") + a following em-dash/colon/dash.
  if let Some(after_tick) = text.strip_prefix('`')
    && let Some(end) = after_tick.find('`')
  {
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
  // Lead clause: stop at the first sentence/clause boundary so a packed first sentence becomes a
  // terse heading (the full text stays in the description below). Trailing punctuation + markdown
  // emphasis are stripped.
  let first = text
    .split(['.', ';', ':', '—', '\n'])
    .next()
    .unwrap_or(text)
    .trim()
    .trim_end_matches([',', ';', ':']);
  let cleaned: String = first
    .chars()
    .filter(|c| !matches!(c, '`' | '*' | '_'))
    .collect();
  let cleaned = cleaned.trim();
  // Generous guard only: the path-based nav means this no longer has to fit a narrow nav column, so
  // a normal first sentence passes through whole; only a runaway one is ellipsized.
  const CAP: usize = 100;
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
  fn derives_terse_summaries() {
    // A leading "`GET /path` — …" code span is dropped (the nav + panel header already show the
    // path).
    assert_eq!(
      short_summary("`GET /api/status` — the agent twin of the dashboard feed. More detail here."),
      "the agent twin of the dashboard feed"
    );
    // A short plain description keeps its first sentence.
    assert_eq!(
      short_summary("Lists all registered corpora."),
      "Lists all registered corpora"
    );
    // The summary stops at the first clause boundary (`. ; : —`), so a packed first sentence
    // becomes a terse lead clause instead of a run-on heading (the full text stays in the
    // description).
    assert_eq!(
      short_summary("Registers a corpus and starts an in-process import job; returns 202 Accepted"),
      "Registers a corpus and starts an in-process import job"
    );
    assert_eq!(
      short_summary(
        "Inspects a single corpus: its activated services and per-service status counts"
      ),
      "Inspects a single corpus"
    );
    // A pathologically long lead clause with no early boundary (>100 chars) is ellipsized as a
    // guard.
    let long = "Lists every single registered corpus across the whole deployment together with all of its many associated services and their workers";
    let summary = short_summary(long);
    assert!(
      summary.chars().count() <= 100 && summary.ends_with('…'),
      "got {summary:?}"
    );
  }
}
