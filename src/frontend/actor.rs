// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! The [`Actor`] request guard: the authenticated initiator of a mutating request.
//!
//! Identity is tokens-first (no OAuth on the critical path). A request carries a rerun token via
//! the `X-Cortex-Token` header or a `?token=` query parameter; the guard resolves it to an owner
//! through `config().auth.rerun_tokens`, or fails the request with `401`. Mutating routes take an
//! `Actor` so the initiator is **threaded into the owner of every write** (attributable actions —
//! the observability mandate) and so writes are denied by default (an empty token map rejects
//! everyone, rather than letting anyone wipe results).

use diesel::pg::PgConnection;
use rocket::http::Status;
use rocket::request::{FromRequest, Outcome, Request};
use rocket::response::{Redirect, Responder};
use rocket::State;

use crate::backend::DbPool;
use crate::config::config;
use crate::models::Session;

/// The authenticated initiator of a mutating request, resolved from a rerun token.
pub struct Actor {
  /// The human-readable owner the token maps to (recorded as the `owner` of the resulting action).
  pub owner: String,
}

/// Resolves a rerun token to its owner, mirroring the [`Actor`] guard's lookup. For **form-based**
/// human submissions (a `<form method=post>` token field), where the guard — which only reads the
/// `X-Cortex-Token` header or `?token=` query — can't see a token in the request body.
pub fn owner_for_token(token: &str) -> Option<String> {
  config().auth.rerun_tokens.get(token).cloned()
}

/// The raw credential carriers on a request, extracted **without any lookup** (cheap, sync): the
/// bearer token (the `X-Cortex-Token` header or `?token=` query) and the [`ADMIN_COOKIE`] session
/// cookie. The audit fairing extracts these synchronously so it can resolve them to an owner *off*
/// the response path (the cookie now needs a DB session lookup — see [`resolve_carriers`]). A token
/// in a POST **form body** (the un-signed-in human forms) is deliberately not visible here.
pub struct ActorCarriers {
  /// A bearer token from the `X-Cortex-Token` header or `?token=` query, if present.
  pub token: Option<String>,
  /// The [`ADMIN_COOKIE`] session-id cookie value, if present.
  pub session_cookie: Option<String>,
}

/// Extracts the [`ActorCarriers`] from a request (no lookups).
pub fn actor_carriers(request: &Request<'_>) -> ActorCarriers {
  ActorCarriers {
    token: request
      .headers()
      .get_one("X-Cortex-Token")
      .map(str::to_string)
      .or_else(|| request.query_value::<String>("token").and_then(Result::ok)),
    session_cookie: request
      .cookies()
      .get(ADMIN_COOKIE)
      .map(|cookie| cookie.value().to_string()),
  }
}

/// Resolves [`ActorCarriers`] to an owner: the bearer token against the configured admin tokens,
/// the session cookie against the `sessions` table (hence the `connection`). The token wins if both
/// are present (an explicit API credential is the more specific intent). `None` if neither
/// resolves.
pub fn resolve_carriers(connection: &mut PgConnection, carriers: &ActorCarriers) -> Option<String> {
  if let Some(owner) = carriers.token.as_deref().and_then(owner_for_token) {
    return Some(owner);
  }
  carriers
    .session_cookie
    .as_deref()
    .and_then(|id| Session::resolve_owner(connection, id))
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for Actor {
  type Error = ();

  async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
    let token = request
      .headers()
      .get_one("X-Cortex-Token")
      .map(str::to_string)
      .or_else(|| request.query_value::<String>("token").and_then(Result::ok));
    match token.and_then(|token| config().auth.rerun_tokens.get(&token).cloned()) {
      Some(owner) => Outcome::Success(Actor { owner }),
      None => Outcome::Error((Status::Unauthorized, ())),
    }
  }
}

/// Documents the [`Actor`] guard for the generated OpenAPI spec (`frontend::apidoc`): every
/// endpoint that takes an `Actor` advertises a `CortexToken` **ApiKey** security scheme — the
/// `X-Cortex-Token` request header — so the docs show which calls are token-gated.
impl<'r> rocket_okapi::request::OpenApiFromRequest<'r> for Actor {
  fn from_request_input(
    _gen: &mut rocket_okapi::gen::OpenApiGenerator,
    _name: String,
    _required: bool,
  ) -> rocket_okapi::Result<rocket_okapi::request::RequestHeaderInput> {
    use rocket_okapi::okapi::openapi3::{SecurityRequirement, SecurityScheme, SecuritySchemeData};
    let security_scheme = SecurityScheme {
      description: Some(
        "A CorTeX rerun token, sent in the `X-Cortex-Token` request header (a `?token=` query \
         parameter is also accepted). It maps to an owner in `auth.rerun_tokens`; a missing or \
         unknown token is rejected with `401`."
          .to_owned(),
      ),
      data: SecuritySchemeData::ApiKey {
        name: "X-Cortex-Token".to_owned(),
        location: "header".to_owned(),
      },
      extensions: Default::default(),
    };
    let mut security_req = SecurityRequirement::new();
    security_req.insert("CortexToken".to_owned(), Vec::new());
    Ok(rocket_okapi::request::RequestHeaderInput::Security(
      "CortexToken".to_owned(),
      security_scheme,
      security_req,
    ))
  }
}

/// The cookie carrying a signed-in admin's session token (set by the `/admin/login` page).
pub const ADMIN_COOKIE: &str = "cortex_admin";

/// A signed-in admin's **browser** session — the [`Actor`]'s counterpart for the human admin UI.
/// The [`ADMIN_COOKIE`] cookie carries a random opaque **session id** (not a credential); this
/// guard resolves it against the server-side `sessions` table on every request, so sign-out (which
/// deletes the row) immediately ends the session and a forged cookie is a useless random id.
/// Established by the admin token *or* a passkey at sign-in. Gated admin screens take an
/// `AdminSession`; an unauthenticated browser is sent to the sign-in page (handled per-route via
/// `Option<AdminSession>`).
pub struct AdminSession {
  /// The owner the session belongs to (recorded as the actor of admin actions).
  pub owner: String,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for AdminSession {
  type Error = ();

  async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
    let Some(session_id) = request
      .cookies()
      .get(ADMIN_COOKIE)
      .map(|c| c.value().to_string())
    else {
      return Outcome::Error((Status::Unauthorized, ()));
    };
    // Resolve the session id against the `sessions` table (the pool is managed state).
    let owner = match request.guard::<&State<DbPool>>().await {
      Outcome::Success(pool) => pool
        .get()
        .ok()
        .and_then(|mut connection| Session::resolve_owner(&mut connection, &session_id)),
      _ => None,
    };
    match owner {
      Some(owner) => Outcome::Success(AdminSession { owner }),
      None => Outcome::Error((Status::Unauthorized, ())),
    }
  }
}

/// The rejection of an admin-gated **human screen**: either a redirect to the sign-in page (the
/// browser isn't signed in) or a genuine error status (e.g. `404` unknown resource, `503` pool
/// exhausted). This lets a gated page keep its real error cases while sending an unauthenticated
/// browser to sign in — rather than showing it a bare `401`. The **agent APIs are unaffected**:
/// they keep the token-based [`Actor`] guard, so a machine still gets a clean `401`, not an HTML
/// redirect.
// The Redirect variant is intentionally larger than the Status variant — this enum exists precisely
// to carry *either*, and it is only ever a short-lived error value, never stored en masse.
#[allow(clippy::large_enum_variant)]
#[derive(Responder)]
pub enum AdminReject {
  /// Not signed in → the sign-in page (`303`).
  Redirect(Redirect),
  /// A genuine error reached *after* authorization (unknown resource, pool exhausted, …).
  Status(Status),
}

impl From<Status> for AdminReject {
  fn from(status: Status) -> Self { AdminReject::Status(status) }
}

/// Requires a signed-in admin for a **human screen**, else a redirect to the sign-in page. The
/// first line of every admin-gated page handler (which returns `Result<Template, AdminReject>`): a
/// handler's existing `Status` errors convert through `?` (see [`AdminReject`]'s `From<Status>`),
/// so it keeps its real `404`/`503` while unauthenticated browsers are bounced to `/admin/login`.
// The `Err` (AdminReject) carries a Redirect; large by clippy's heuristic but it is a transient
// one-shot value on the request path, not a hot return — same rationale as the page handlers.
#[allow(clippy::result_large_err)]
pub fn require_admin(session: Option<AdminSession>) -> Result<AdminSession, AdminReject> {
  session.ok_or_else(|| AdminReject::Redirect(Redirect::to("/admin/login")))
}
