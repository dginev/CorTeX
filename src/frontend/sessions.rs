// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Active admin **sessions** management — the security-oversight completion of the session model
//! (`models::session`, `docs/archive/WEBAUTHN_DESIGN.md`): see who is currently signed in (token or
//! passkey) and revoke a compromised identity's sessions. Uniform authz — any signed-in admin may
//! view + revoke.
//!
//! **Session ids are never exposed** (the id *is* the bearer credential): the read surface shows
//! owner / method / times only, and revocation is **per-owner** (by the non-secret owner name), not
//! per-opaque-id. "This is your current session" is computed server-side by comparing the row id to
//! the request's cookie, without surfacing either.

use rocket::http::{CookieJar, Status};
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::{context, Template};
use serde::Serialize;

use crate::backend::DbPool;
use crate::frontend::actor::{
  require_admin_to, Actor, AdminReject, AdminSession, ReturnTo, ADMIN_COOKIE,
};
use crate::models::Session;

/// An active admin session as exposed over the API/UI. **No session id** (it is the credential):
/// owner + how they signed in + when, and whether it is the viewer's current browser session.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SessionDto {
  /// The signed-in identity (the audit-log actor / token owner).
  pub owner: String,
  /// How the session was established: `token` or `passkey`.
  pub method: String,
  /// When the session was opened, as an RFC 3339 UTC timestamp (localized to the viewer's zone in
  /// the UI; directly parseable over the agent API).
  pub created_at: String,
  /// When the session expires, as an RFC 3339 UTC timestamp (localized to the viewer's zone in the
  /// UI; directly parseable over the agent API).
  pub expires_at: String,
  /// Whether this row is the requesting browser's own current session (UI only; always `false`
  /// over the agent API, which has no browser cookie).
  pub current: bool,
}

/// Loads the active sessions as DTOs, marking the one whose id matches `current_id` (the viewer's
/// cookie) — the shared core of the agent endpoint and the human screen (symmetry contract).
fn load_sessions(pool: &DbPool, current_id: Option<&str>) -> Result<Vec<SessionDto>, Status> {
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let sessions = Session::active(&mut connection).map_err(|_| Status::InternalServerError)?;
  Ok(
    sessions
      .into_iter()
      .map(|session| SessionDto {
        current: current_id == Some(session.id.as_str()),
        owner: session.owner,
        method: session.method,
        created_at: crate::frontend::helpers::iso_utc(session.created_at),
        expires_at: crate::frontend::helpers::iso_utc(session.expires_at),
      })
      .collect(),
  )
}

/// The active sessions (agent twin of the `/admin/sessions` screen): who is currently signed in.
/// **Token-gated** (the active-identity list is sensitive). `503` if the pool is exhausted.
#[rocket_okapi::openapi(tag = "Management")]
#[get("/api/sessions")]
pub fn api_sessions(_caller: Actor, pool: &State<DbPool>) -> Result<Json<Vec<SessionDto>>, Status> {
  Ok(Json(load_sessions(pool, None)?))
}

/// The active-sessions screen (`GET /admin/sessions`): who is signed in, with per-identity revoke.
/// Signed-in admins only (unauthenticated → sign-in page, returning here).
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/admin/sessions")]
pub fn sessions_page(
  session: Option<AdminSession>,
  return_to: ReturnTo,
  cookies: &CookieJar<'_>,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  let session = require_admin_to(session, &return_to)?;
  let current_id = cookies
    .get(ADMIN_COOKIE)
    .map(|cookie| cookie.value().to_string());
  // Best-effort, like the other admin screens: a db hiccup renders an empty table, never a 500.
  let sessions = load_sessions(pool, current_id.as_deref()).unwrap_or_default();
  let global = serde_json::json!({
    "title": "Active sessions",
    "description": "Admins currently signed in to CorTeX",
  });
  Ok(Template::render(
    "sessions",
    context! { global, owner: session.owner, sessions },
  ))
}

/// Revokes **all** of an identity's sessions (`POST /admin/sessions/revoke?<owner>`) — sign-out-
/// everywhere, or kicking out a compromised account. Referenced by the non-secret owner name (never
/// a session id). Redirects back to the screen. Signed-in admins only.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[post("/admin/sessions/revoke?<owner>")]
pub fn revoke_owner_sessions(
  owner: String,
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Redirect, AdminReject> {
  require_admin_to(session, &return_to)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let _ = Session::revoke_all_for(&mut connection, &owner);
  Ok(Redirect::to("/admin/sessions"))
}

/// The human sessions screen + revoke (the agent `api_sessions` is mounted via `frontend::apidoc`).
pub fn routes() -> Vec<Route> { routes![sessions_page, revoke_owner_sessions] }
