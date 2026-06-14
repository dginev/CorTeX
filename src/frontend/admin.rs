// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Admin web UI: a single **signed-in** `/admin` dashboard that consolidates the admin actions
//! (service registry, background jobs, system health, settings, API docs, and "add a corpus") which
//! previously sprinkled the public homepage. Access uses the lightweight token scheme — an
//! [`AdminSession`] cookie (`frontend::actor`), set on the sign-in page below.

use rocket::form::Form;
use rocket::http::{Cookie, CookieJar, SameSite};
use rocket::response::Redirect;
use rocket::{Route, State};
use rocket_dyn_templates::{context, Template};

use crate::backend::DbPool;
use crate::frontend::actor::{
  owner_for_token, safe_next, sign_in_url, AdminSession, ReturnTo, ADMIN_COOKIE,
};
use crate::models::{Corpus, HistoricalRun, Session};

/// The admin dashboard (`GET /admin`): the consolidated home for admin actions. **Signed-in admins
/// only** — an unauthenticated browser is redirected to the sign-in page (`Err(Redirect)`).
// `Redirect` (Rocket's URI responder) is a chunky type, so the `Err` variant trips
// `result_large_err` — irrelevant for a one-shot request handler; the page-or-redirect `Result` is
// the idiomatic shape.
#[allow(clippy::result_large_err)]
#[get("/admin")]
pub fn admin_page(
  session: Option<AdminSession>,
  return_to: ReturnTo,
  pool: &State<DbPool>,
) -> Result<Template, Redirect> {
  let session = session.ok_or_else(|| Redirect::to(sign_in_url(false, Some(&return_to.0))))?;
  // At-a-glance status for the command center — all best-effort over one pooled connection: a db
  // hiccup degrades each card to zero/blank, never blocks the dashboard. Kept to cheap queries on
  // small tables (no dispatcher/storage probe — that is what the System Health screen is for).
  let mut corpus_count = 0usize;
  let mut active_jobs = 0usize;
  let mut active_sessions = 0usize;
  let mut last_run: Option<serde_json::Value> = None;
  if let Ok(mut connection) = pool.get() {
    corpus_count = Corpus::all(&mut connection).map_or(0, |corpora| corpora.len());
    active_jobs = crate::jobs::list_recent(&mut connection, true, 200).len();
    active_sessions = Session::active(&mut connection).map_or(0, |sessions| sessions.len());
    last_run = HistoricalRun::recent_all(&mut connection, 1)
      .ok()
      .and_then(|runs| runs.into_iter().next())
      .map(|run| {
        // The latest run is often still open (tallies frozen only at completion) — overlay live
        // progress so the card shows real task counts, not a misleading zero.
        let run = run.with_live_tallies(&mut connection);
        serde_json::json!({
          "when": run.start_time.format("%Y-%m-%d %H:%M").to_string(),
          "owner": run.owner,
          "description": run.description,
          "total": run.total,
          "in_progress": run.in_progress,
          "open": run.end_time.is_none(),
        })
      });
  }
  let global = serde_json::json!({
    "title": "Admin",
    "description": "CorTeX administration dashboard",
  });
  Ok(Template::render(
    "admin",
    context! { global, owner: session.owner, corpus_count, active_jobs, active_sessions, last_run },
  ))
}

/// The sign-in page (`GET /admin/login?<bad>&<next>`): a form to enter an admin token, plus a "sign
/// in with a passkey" affordance when passkeys are enabled. `?bad=true` flags a failed previous
/// attempt; `?next=` is the destination to return to after signing in (carried through the form).
#[get("/admin/login?<bad>&<next>")]
pub fn admin_login_page(
  bad: Option<bool>,
  next: Option<String>,
  webauthn: &State<Option<crate::frontend::webauthn::WebauthnState>>,
) -> Template {
  let global = serde_json::json!({
    "title": "Admin sign-in",
    "description": "Sign in to the CorTeX admin dashboard",
  });
  // Only carry a safe local `next` into the page (open-redirect guard; also avoids reflecting
  // junk).
  let next = next.filter(|path| path.starts_with('/') && !path.starts_with("//"));
  Template::render(
    "admin-login",
    context! { global, bad: bad.unwrap_or(false), next, passkeys_enabled: webauthn.inner().is_some() },
  )
}

/// The sign-in form fields.
#[derive(FromForm)]
pub struct LoginForm {
  /// A rerun token (resolved to an owner via `auth.rerun_tokens`).
  pub token: String,
  /// Where to return after a successful sign-in (validated to a safe local path).
  pub next: Option<String>,
}

/// Processes sign-in (`POST /admin/login`): validates the token against `auth.rerun_tokens`; on
/// success **opens a server-side session** and sets the [`ADMIN_COOKIE`] cookie to its random
/// opaque id (HttpOnly, SameSite=Lax) — the cookie no longer carries the token — then redirects to
/// the validated `next` destination (default `/admin`). A bad token (or a DB hiccup opening the
/// session) returns to the sign-in page flagged, preserving `next`.
#[post("/admin/login", data = "<form>")]
pub fn admin_login(
  form: Form<LoginForm>,
  cookies: &CookieJar<'_>,
  pool: &State<DbPool>,
) -> Redirect {
  let session_id = owner_for_token(&form.token).and_then(|owner| {
    let mut connection = pool.get().ok()?;
    Session::open(&mut connection, &owner, "token").ok()
  });
  match session_id {
    Some(session_id) => {
      cookies.add(
        Cookie::build((ADMIN_COOKIE, session_id))
          .http_only(true)
          .same_site(SameSite::Lax)
          .path("/")
          .build(),
      );
      Redirect::to(safe_next(form.next.as_deref()))
    },
    // Preserve the return destination across a failed attempt.
    None => Redirect::to(sign_in_url(true, form.next.as_deref())),
  }
}

/// Signs out (`POST /admin/logout`): **revokes** the server-side session (so the id is dead even if
/// the cookie lingers), clears the cookie, and returns to the sign-in page.
#[post("/admin/logout")]
pub fn admin_logout(cookies: &CookieJar<'_>, pool: &State<DbPool>) -> Redirect {
  if let Some(session_id) = cookies
    .get(ADMIN_COOKIE)
    .map(|cookie| cookie.value().to_string())
  {
    if let Ok(mut connection) = pool.get() {
      let _ = Session::revoke(&mut connection, &session_id);
    }
  }
  cookies.remove(Cookie::build(ADMIN_COOKIE).path("/").build());
  Redirect::to("/admin/login")
}

/// The route set for the admin web UI.
pub fn routes() -> Vec<Route> { routes![admin_page, admin_login_page, admin_login, admin_logout] }
