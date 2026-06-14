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
use crate::frontend::actor::{owner_for_token, AdminSession, ADMIN_COOKIE};
use crate::models::Corpus;

/// The admin dashboard (`GET /admin`): the consolidated home for admin actions. **Signed-in admins
/// only** — an unauthenticated browser is redirected to the sign-in page (`Err(Redirect)`).
// `Redirect` (Rocket's URI responder) is a chunky type, so the `Err` variant trips
// `result_large_err` — irrelevant for a one-shot request handler; the page-or-redirect `Result` is
// the idiomatic shape.
#[allow(clippy::result_large_err)]
#[get("/admin")]
pub fn admin_page(
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Template, Redirect> {
  let session = session.ok_or_else(|| Redirect::to("/admin/login"))?;
  // A small corpus count for context (best-effort; never blocks the dashboard).
  let corpus_count = pool
    .get()
    .ok()
    .and_then(|mut connection| Corpus::all(&mut connection).ok())
    .map_or(0, |corpora| corpora.len());
  let global = serde_json::json!({
    "title": "Admin",
    "description": "CorTeX administration dashboard",
  });
  Ok(Template::render(
    "admin",
    context! { global, owner: session.owner, corpus_count },
  ))
}

/// The sign-in page (`GET /admin/login`): a form to enter a rerun token. `?bad=true` flags a failed
/// previous attempt.
#[get("/admin/login?<bad>")]
pub fn admin_login_page(bad: Option<bool>) -> Template {
  let global = serde_json::json!({
    "title": "Admin sign-in",
    "description": "Sign in to the CorTeX admin dashboard",
  });
  Template::render(
    "admin-login",
    context! { global, bad: bad.unwrap_or(false) },
  )
}

/// The sign-in form field.
#[derive(FromForm)]
pub struct LoginForm {
  /// A rerun token (resolved to an owner via `auth.rerun_tokens`).
  pub token: String,
}

/// Processes sign-in (`POST /admin/login`): validates the token against `auth.rerun_tokens`; on
/// success sets the [`ADMIN_COOKIE`] session cookie (HttpOnly, SameSite=Lax) and redirects to
/// `/admin`, else back to the sign-in page flagged as a bad attempt.
#[post("/admin/login", data = "<form>")]
pub fn admin_login(form: Form<LoginForm>, cookies: &CookieJar<'_>) -> Redirect {
  match owner_for_token(&form.token) {
    Some(_owner) => {
      cookies.add(
        Cookie::build((ADMIN_COOKIE, form.token.clone()))
          .http_only(true)
          .same_site(SameSite::Lax)
          .path("/")
          .build(),
      );
      Redirect::to("/admin")
    },
    None => Redirect::to("/admin/login?bad=true"),
  }
}

/// Signs out (`POST /admin/logout`): removes the session cookie and returns to the sign-in page.
#[post("/admin/logout")]
pub fn admin_logout(cookies: &CookieJar<'_>) -> Redirect {
  cookies.remove(Cookie::build(ADMIN_COOKIE).path("/").build());
  Redirect::to("/admin/login")
}

/// The route set for the admin web UI.
pub fn routes() -> Vec<Route> { routes![admin_page, admin_login_page, admin_login, admin_logout] }
