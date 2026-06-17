// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! The **accounting** pillar (AAA — `docs/archive/AAA_DESIGN.md`): a Rocket fairing that records
//! every mutating admin request to the `audit_log`, so "who did what, when, to what, with what
//! outcome" is observable. Centralizing it in one fairing (rather than a call in each write
//! handler) means no endpoint can forget to log and new endpoints are audited automatically —
//! drift-proof, in the spirit of the symmetry contract.

use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::{Method, Status};
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Request, Response, Route, State};
use rocket_dyn_templates::{context, Template};
use serde::Serialize;

use crate::backend::DbPool;
use crate::frontend::actor::{
  actor_carriers, resolve_carriers, sign_in_url, Actor, AdminSession, ReturnTo,
};
use crate::models::{AuditEntry, NewAuditEntry};

/// Rows per audit page. The read is most-recent-first and **page-based** (`?page=`), so this bounds
/// every response and lets an admin walk back through history 100 at a time — never asking for the
/// whole table.
const AUDIT_PAGE_SIZE: i64 = 100;

/// Records every **mutating** request (`POST`/`PUT`/`PATCH`/`DELETE`) to the `audit_log`: the
/// resolved [`crate::frontend::actor`] (empty if unauthenticated — itself a useful signal), the
/// matched route's name as the **action** (Rocket sets it to the handler fn, e.g. `delete_corpus`),
/// the request path as the **target**, and the response status as the **outcome**.
///
/// **Best-effort & non-blocking**: a failed audit write is logged and swallowed — accounting must
/// never fail the action it observes (`docs/DESIGN_PRINCIPLES.md`) — and the insert runs on a
/// blocking task so a brief diesel round-trip never stalls the async response path.
pub struct AuditFairing;

#[rocket::async_trait]
impl Fairing for AuditFairing {
  fn info(&self) -> Info {
    Info {
      name: "Audit log",
      kind: Kind::Response,
    }
  }

  async fn on_response<'r>(&self, request: &'r Request<'_>, response: &mut Response<'r>) {
    // Only mutating methods are admin "actions taken"; reads are out of scope for the audit log.
    if !matches!(
      request.method(),
      Method::Post | Method::Put | Method::Patch | Method::Delete
    ) {
      return;
    }
    // The pool is managed state; if it is somehow absent there is nowhere to record (skip
    // silently).
    let Some(pool) = request.rocket().state::<DbPool>() else {
      return;
    };
    let action = request
      .route()
      .and_then(|route| route.name.as_deref().map(str::to_string))
      .unwrap_or_else(|| request.method().as_str().to_string());
    let target = request.uri().path().to_string();
    // Extract the credential carriers synchronously (cheap), then resolve the actor *inside* the
    // blocking task — the session-cookie lookup is a DB query that must not run on the async
    // reactor.
    let carriers = actor_carriers(request);
    let outcome = response.status().code.to_string();
    let pool = pool.clone();
    rocket::tokio::task::spawn_blocking(move || match pool.get() {
      Ok(mut connection) => {
        let actor = resolve_carriers(&mut connection, &carriers).unwrap_or_default();
        let entry = NewAuditEntry::new(actor, action, target).outcome(outcome);
        if let Err(error) = entry.record(&mut connection) {
          tracing::error!(?entry, %error, "audit: failed to record entry");
        }
      },
      Err(error) => {
        tracing::warn!(action, target, %error, "audit: pool exhausted, dropped audit row");
      },
    });
  }
}

/// A recorded admin action as exposed over the API/UI — the read view of the `audit_log`. The
/// timestamp is a formatted string (the model's `at` is a chrono `NaiveDateTime`, not serialized
/// directly — see `models::audit`).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct AuditDto {
  /// Auto-incremented id (monotonic; usable as a cursor).
  pub id: i64,
  /// The identity that acted (empty if the action was unauthenticated).
  pub actor: String,
  /// The action verb (the matched route name, e.g. `delete_corpus`).
  pub action: String,
  /// The resource acted on (the request path).
  pub target: String,
  /// The outcome (an HTTP status code).
  pub outcome: String,
  /// Optional short context.
  pub details: String,
  /// When it happened, formatted `YYYY-MM-DD HH:MM:SS` (server clock).
  pub at: String,
}

impl From<AuditEntry> for AuditDto {
  fn from(entry: AuditEntry) -> AuditDto {
    AuditDto {
      id: entry.id,
      actor: entry.actor,
      action: entry.action,
      target: entry.target,
      outcome: entry.outcome,
      details: entry.details,
      at: crate::frontend::helpers::iso_utc(entry.at),
    }
  }
}

/// One page of the audit log — the shared shape for the agent endpoint and the human screen.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct AuditPage {
  /// The audit rows for this page, most-recent first (at most [`AUDIT_PAGE_SIZE`]).
  pub entries: Vec<AuditDto>,
  /// The 0-based page index this response covers.
  pub page: i64,
  /// Rows per page (the cap).
  pub page_size: i64,
  /// Whether an older page exists (more history beyond this one).
  pub has_next: bool,
}

/// Loads one page of the audit log (most-recent first, optional `actor` filter, [`AUDIT_PAGE_SIZE`]
/// rows per page) as the shared [`AuditPage`] — the core of the agent endpoint and the human screen
/// (symmetry contract). Fetches one extra row to report `has_next` without a second COUNT query.
fn load_audit(pool: &DbPool, actor: Option<&str>, page: i64) -> Result<AuditPage, Status> {
  let page = page.max(0);
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let mut entries = AuditEntry::list(
    &mut connection,
    actor,
    AUDIT_PAGE_SIZE + 1,
    page * AUDIT_PAGE_SIZE,
  )
  .map_err(|_| Status::InternalServerError)?;
  let has_next = entries.len() as i64 > AUDIT_PAGE_SIZE;
  entries.truncate(AUDIT_PAGE_SIZE as usize);
  Ok(AuditPage {
    entries: entries.into_iter().map(AuditDto::from).collect(),
    page,
    page_size: AUDIT_PAGE_SIZE,
    has_next,
  })
}

/// The audit log (agent twin of the `/admin/audit` screen): admin actions, most-recent first,
/// optionally filtered to one `actor`, **paginated** — [`AUDIT_PAGE_SIZE`] rows per `page`
/// (0-based; `has_next` flags more history). **Token-gated** — reading who-did-what is sensitive,
/// so it takes an [`Actor`] like the writes it records. `503` if the pool is exhausted.
#[rocket_okapi::openapi(tag = "Management")]
#[get("/api/audit?<page>&<actor>")]
pub fn api_audit(
  page: Option<i64>,
  actor: Option<String>,
  _caller: Actor,
  pool: &State<DbPool>,
) -> Result<Json<AuditPage>, Status> {
  Ok(Json(load_audit(pool, actor.as_deref(), page.unwrap_or(0))?))
}

/// The audit-log screen (`GET /admin/audit`): the human view of recent admin actions, **signed-in
/// admins only** (an unauthenticated browser is redirected to the sign-in page). Optional `?actor=`
/// and `?limit=` mirror the agent endpoint.
// `Redirect` is a chunky responder, so the `Err` variant trips `result_large_err` — irrelevant for
// a one-shot page handler (mirrors `admin::admin_page`).
#[allow(clippy::result_large_err)]
#[get("/admin/audit?<page>&<actor>")]
pub fn audit_page(
  page: Option<i64>,
  actor: Option<String>,
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, Redirect> {
  let session = session.ok_or_else(|| Redirect::to(sign_in_url(false, Some(&return_to.0))))?;
  // Best-effort, like the dashboard: a pool/db hiccup renders an empty page, never an error page.
  let audit = load_audit(pool, actor.as_deref(), page.unwrap_or(0)).unwrap_or(AuditPage {
    entries: Vec::new(),
    page: 0,
    page_size: AUDIT_PAGE_SIZE,
    has_next: false,
  });
  let global = serde_json::json!({
    "title": "Audit log",
    "description": "Recent CorTeX admin actions and who took them",
  });
  Ok(Template::render(
    "audit",
    context! {
      global,
      owner: session.owner,
      rows: audit.entries,
      page: audit.page,
      has_next: audit.has_next,
      has_prev: audit.page > 0,
      prev_page: (audit.page - 1).max(0),
      next_page: audit.page + 1,
      actor_filter: actor,
    },
  ))
}

/// The human audit screen (the agent `api_audit` is mounted via `frontend::apidoc`).
pub fn routes() -> Vec<Route> { routes![audit_page] }
