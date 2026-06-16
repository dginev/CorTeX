// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Passkey (**WebAuthn**) sign-in — the relying-party instance built from config
//! (`docs/archive/WEBAUTHN_DESIGN.md`). This is the **foundation**: the configured
//! [`webauthn_rs::prelude::Webauthn`] relying party as Rocket managed state. The
//! registration/authentication ceremonies and the sign-in UI build on this in the following
//! increments.
//!
//! The relying party is the CorTeX server itself — no external IdP, no per-deployment app
//! registration. Passkeys are the convenient day-to-day human sign-in; the admin token
//! (`frontend::actor`) remains the bootstrap / break-glass / agent credential. Passkeys never block
//! the token path: a disabled or misconfigured relying party degrades to `None` (logged), and
//! sign-in still works via the token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use rocket::http::{Cookie, CookieJar, SameSite, Status};
use rocket::response::Redirect;
use rocket::serde::json::Json;
use rocket::{Route, State};
use rocket_dyn_templates::{context, Template};
use serde::Serialize;
use webauthn_rs::prelude::*;

use crate::backend::DbPool;
use crate::config::WebauthnConfig;
use crate::frontend::actor::{
  require_admin, require_admin_to, AdminReject, AdminSession, ReturnTo, ADMIN_COOKIE,
};
use crate::models::{Session, WebauthnCredential, WebauthnUser};

/// The cookie carrying the in-flight ceremony id between a `…/begin` and its `…/finish` (scoped to
/// the passkey paths, HttpOnly, SameSite=Strict — it is never a credential, just a lookup key).
const CEREMONY_COOKIE: &str = "cortex_ceremony";
/// How long an in-flight ceremony is valid (a user taps their authenticator within seconds).
const CEREMONY_TTL: Duration = Duration::from_secs(300);

/// The configured WebAuthn relying-party instance, shared as Rocket managed state. Present only
/// when passkeys are **enabled** and the relying party built successfully; absent ⇒ token sign-in
/// only.
pub struct WebauthnState {
  /// The relying-party instance (`Arc` so the ceremony handlers cheaply share one instance).
  pub webauthn: Arc<Webauthn>,
}

/// Builds the relying-party [`Webauthn`] from config, or returns `None` (logged, never panics) when
/// passkeys are disabled or the `rp_id`/`rp_origin` are invalid — token sign-in keeps working
/// either way (graceful degradation, the robustness mandate).
pub fn build_state(config: &WebauthnConfig) -> Option<WebauthnState> {
  if !config.enabled {
    return None;
  }
  let origin = match Url::parse(&config.rp_origin) {
    Ok(origin) => origin,
    Err(error) => {
      tracing::error!(rp_origin = %config.rp_origin, %error, "webauthn: invalid rp_origin (passkeys disabled)");
      return None;
    },
  };
  match WebauthnBuilder::new(&config.rp_id, &origin)
    .map(|builder| builder.rp_name("CorTeX"))
    .and_then(|builder| builder.build())
  {
    Ok(webauthn) => Some(WebauthnState {
      webauthn: Arc::new(webauthn),
    }),
    Err(error) => {
      tracing::error!(rp_id = %config.rp_id, rp_origin = %config.rp_origin, %error, "webauthn: cannot build relying party (passkeys disabled)");
      None
    },
  }
}

/// In-flight WebAuthn ceremony state, kept **server-side** between the begin and finish requests
/// (never serialized to the client) and keyed by a random id in the [`CEREMONY_COOKIE`].
pub enum Ceremony {
  /// A passkey **enrollment** in progress (the owner is known from the signed-in session).
  Register(PasskeyRegistration),
  /// A passkey **sign-in** in progress; carries the claimed `owner` (verified by the assertion at
  /// finish) so a successful authentication opens a session for the right identity.
  Authenticate {
    /// The owner the sign-in is for (its enrolled passkeys seed the challenge).
    owner: String,
    /// The in-progress authentication state, paired to the issued challenge.
    state: PasskeyAuthentication,
  },
}

/// A short-lived, process-local store of in-flight ceremonies. Ceremonies live seconds, so an
/// in-memory map (pruned on insert, capacity-bounded by the TTL) is sufficient and needs no
/// `danger-allow-state-serialisation`. Mutex poisoning is recovered from, never panicked on (this
/// is a request path — `docs/DESIGN_PRINCIPLES.md`).
pub struct CeremonyStore {
  inner: Mutex<HashMap<String, (Ceremony, Instant)>>,
}

impl CeremonyStore {
  /// An empty store (managed by Rocket).
  pub fn new() -> Self {
    CeremonyStore {
      inner: Mutex::new(HashMap::new()),
    }
  }

  /// Stores a ceremony, pruning expired entries, and returns its random id (for the cookie).
  pub fn put(&self, ceremony: Ceremony) -> String {
    let id: String = thread_rng()
      .sample_iter(&Alphanumeric)
      .take(32)
      .map(char::from)
      .collect();
    let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();
    map.retain(|_, (_, expiry)| *expiry > now);
    map.insert(id.clone(), (ceremony, now + CEREMONY_TTL));
    id
  }

  /// Removes and returns a ceremony if present and unexpired (single-use).
  pub fn take(&self, id: &str) -> Option<Ceremony> {
    let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
    match map.remove(id) {
      Some((ceremony, expiry)) if expiry > Instant::now() => Some(ceremony),
      _ => None,
    }
  }
}

impl Default for CeremonyStore {
  fn default() -> Self { Self::new() }
}

/// Checks out the relying party, mapping "passkeys disabled" to `503` (the caller's UI offers the
/// token sign-in instead).
fn relying_party(webauthn: &State<Option<WebauthnState>>) -> Result<&Webauthn, Status> {
  webauthn
    .inner()
    .as_ref()
    .map(|state| state.webauthn.as_ref())
    .ok_or(Status::ServiceUnavailable)
}

/// **Enroll, step 1** (`POST /admin/passkeys/register/begin`): a signed-in admin starts registering
/// a new passkey for their own identity. Returns the WebAuthn `CreationChallengeResponse` JSON for
/// `navigator.credentials.create()` and stashes the ceremony state server-side (cookie-keyed).
/// `401` if not signed in, `503` if passkeys are disabled. Already-enrolled credentials are
/// excluded so the same authenticator isn't registered twice.
#[post("/admin/passkeys/register/begin")]
pub fn register_begin(
  session: AdminSession,
  webauthn: &State<Option<WebauthnState>>,
  store: &State<CeremonyStore>,
  cookies: &CookieJar<'_>,
  pool: &State<DbPool>,
) -> Result<Json<CreationChallengeResponse>, Status> {
  let webauthn = relying_party(webauthn)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let handle = WebauthnUser::ensure(&mut connection, &session.owner)
    .map_err(|_| Status::InternalServerError)?;
  let exclude: Vec<CredentialID> = WebauthnCredential::for_owner(&mut connection, &session.owner)
    .unwrap_or_default()
    .iter()
    .filter_map(|row| serde_json::from_value::<Passkey>(row.credential.clone()).ok())
    .map(|passkey| passkey.cred_id().clone())
    .collect();
  let (challenge, state) = webauthn
    .start_passkey_registration(handle, &session.owner, &session.owner, Some(exclude))
    .map_err(|error| {
      tracing::error!(%error, "webauthn: start_passkey_registration failed");
      Status::InternalServerError
    })?;
  let ceremony_id = store.put(Ceremony::Register(state));
  cookies.add(
    Cookie::build((CEREMONY_COOKIE, ceremony_id))
      .http_only(true)
      .same_site(SameSite::Strict)
      .path("/admin/passkeys")
      .build(),
  );
  Ok(Json(challenge))
}

/// **Enroll, step 2** (`POST /admin/passkeys/register/finish?label=`): finishes registration with
/// the authenticator's response, persisting the new passkey (public key only) under an optional
/// `label`. `400` if the ceremony cookie/state is missing or the attestation doesn't verify.
#[post("/admin/passkeys/register/finish?<label>", data = "<credential>")]
pub fn register_finish(
  session: AdminSession,
  label: Option<String>,
  credential: Json<RegisterPublicKeyCredential>,
  webauthn: &State<Option<WebauthnState>>,
  store: &State<CeremonyStore>,
  cookies: &CookieJar<'_>,
  pool: &State<DbPool>,
) -> Result<Status, Status> {
  let webauthn = relying_party(webauthn)?;
  let ceremony_id = cookies
    .get(CEREMONY_COOKIE)
    .map(|cookie| cookie.value().to_string())
    .ok_or(Status::BadRequest)?;
  cookies.remove(
    Cookie::build(CEREMONY_COOKIE)
      .path("/admin/passkeys")
      .build(),
  );
  let state = match store.take(&ceremony_id) {
    Some(Ceremony::Register(state)) => state,
    _ => return Err(Status::BadRequest),
  };
  let passkey = webauthn
    .finish_passkey_registration(&credential, &state)
    .map_err(|_| Status::BadRequest)?;
  let value = serde_json::to_value(&passkey).map_err(|_| Status::InternalServerError)?;
  let label = label.unwrap_or_default();
  let label = if label.trim().is_empty() {
    "passkey"
  } else {
    label.trim()
  };
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  WebauthnCredential::store(&mut connection, &session.owner, label, &value)
    .map_err(|_| Status::InternalServerError)?;
  Ok(Status::Created)
}

/// **Sign in, step 1** (`POST /admin/passkeys/auth/begin?owner=`): begins a passkey authentication
/// for the named owner, seeded by that owner's enrolled passkeys. Returns the WebAuthn
/// `RequestChallengeResponse` for `navigator.credentials.get()` and stashes the ceremony state.
/// `404` if the owner has no enrolled passkeys (admin identities are not treated as secret — the
/// public deployment sits behind an Anubis proxy and the admin set is small), `503` if disabled.
#[post("/admin/passkeys/auth/begin?<owner>")]
pub fn auth_begin(
  owner: String,
  webauthn: &State<Option<WebauthnState>>,
  store: &State<CeremonyStore>,
  cookies: &CookieJar<'_>,
  pool: &State<DbPool>,
) -> Result<Json<RequestChallengeResponse>, Status> {
  let webauthn = relying_party(webauthn)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let passkeys: Vec<Passkey> = WebauthnCredential::for_owner(&mut connection, &owner)
    .unwrap_or_default()
    .iter()
    .filter_map(|row| serde_json::from_value::<Passkey>(row.credential.clone()).ok())
    .collect();
  if passkeys.is_empty() {
    return Err(Status::NotFound);
  }
  let (challenge, state) = webauthn
    .start_passkey_authentication(&passkeys)
    .map_err(|error| {
      tracing::error!(%error, "webauthn: start_passkey_authentication failed");
      Status::InternalServerError
    })?;
  let ceremony_id = store.put(Ceremony::Authenticate { owner, state });
  cookies.add(
    Cookie::build((CEREMONY_COOKIE, ceremony_id))
      .http_only(true)
      .same_site(SameSite::Strict)
      .path("/admin/passkeys")
      .build(),
  );
  Ok(Json(challenge))
}

/// **Sign in, step 2** (`POST /admin/passkeys/auth/finish`): completes the assertion. On success
/// **opens a `passkey` session** (the unified session model), sets the [`ADMIN_COOKIE`], advances
/// the matching credential's signature counter (clone detection), and returns `200` — the browser
/// then navigates to `/admin`. `400` on a missing/expired ceremony, `401` if the assertion doesn't
/// verify.
#[post("/admin/passkeys/auth/finish", data = "<credential>")]
pub fn auth_finish(
  credential: Json<PublicKeyCredential>,
  webauthn: &State<Option<WebauthnState>>,
  store: &State<CeremonyStore>,
  cookies: &CookieJar<'_>,
  pool: &State<DbPool>,
) -> Result<Status, Status> {
  let webauthn = relying_party(webauthn)?;
  let ceremony_id = cookies
    .get(CEREMONY_COOKIE)
    .map(|cookie| cookie.value().to_string())
    .ok_or(Status::BadRequest)?;
  cookies.remove(
    Cookie::build(CEREMONY_COOKIE)
      .path("/admin/passkeys")
      .build(),
  );
  let (owner, state) = match store.take(&ceremony_id) {
    Some(Ceremony::Authenticate { owner, state }) => (owner, state),
    _ => return Err(Status::BadRequest),
  };
  let result = webauthn
    .finish_passkey_authentication(&credential, &state)
    .map_err(|_| Status::Unauthorized)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  // Advance the matching credential's signature counter (best-effort: failure to persist the
  // counter must not fail an otherwise-valid sign-in).
  if let Ok(rows) = WebauthnCredential::for_owner(&mut connection, &owner) {
    for row in rows {
      if let Ok(mut passkey) = serde_json::from_value::<Passkey>(row.credential.clone()) {
        match passkey.update_credential(&result) {
          Some(true) => {
            if let Ok(value) = serde_json::to_value(&passkey) {
              let _ = WebauthnCredential::update_after_use(&mut connection, row.id, &value);
            }
          },
          Some(false) => {
            let _ = WebauthnCredential::touch(&mut connection, row.id);
          },
          None => {},
        }
      }
    }
  }
  let session_id =
    Session::open(&mut connection, &owner, "passkey").map_err(|_| Status::InternalServerError)?;
  cookies.add(
    Cookie::build((ADMIN_COOKIE, session_id))
      .http_only(true)
      .same_site(SameSite::Lax)
      .path("/")
      .build(),
  );
  Ok(Status::Ok)
}

/// A passkey as shown on the management page.
#[derive(Debug, Serialize)]
pub struct PasskeyDto {
  /// Row id (for the remove action).
  pub id: i64,
  /// The human label.
  pub label: String,
  /// When it was enrolled (formatted).
  pub created_at: String,
  /// When it was last used to sign in, or "never".
  pub last_used: String,
}

/// The "Your passkeys" management screen (`GET /admin/passkeys`): the signed-in admin's enrolled
/// passkeys, with enroll + remove actions. Signed-in admins only (unauthenticated → sign-in page).
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[get("/admin/passkeys")]
pub fn passkeys_page(
  session: Option<AdminSession>,
  return_to: ReturnTo,
  webauthn: &State<Option<WebauthnState>>,
  pool: &State<DbPool>,
) -> Result<Template, AdminReject> {
  let session = require_admin_to(session, &return_to)?;
  let passkeys: Vec<PasskeyDto> = pool
    .get()
    .ok()
    .and_then(|mut connection| WebauthnCredential::for_owner(&mut connection, &session.owner).ok())
    .unwrap_or_default()
    .into_iter()
    .map(|row| PasskeyDto {
      id: row.id,
      label: row.label,
      created_at: crate::frontend::helpers::iso_utc(row.created_at),
      last_used: row
        .last_used
        .map(crate::frontend::helpers::iso_utc)
        .unwrap_or_else(|| "never".to_string()),
    })
    .collect();
  let global = serde_json::json!({
    "title": "Your passkeys",
    "description": "Manage the passkeys that sign you in to CorTeX",
  });
  Ok(Template::render(
    "passkeys",
    context! { global, owner: session.owner, enabled: webauthn.inner().is_some(), passkeys },
  ))
}

/// Removes one of the signed-in admin's passkeys (`POST /admin/passkeys/<id>/delete`); the `owner`
/// filter means a session can only remove its own. Redirects back to the management page.
#[allow(clippy::result_large_err)] // AdminReject carries a Redirect; see actor::AdminReject.
#[post("/admin/passkeys/<id>/delete")]
pub fn passkey_delete(
  id: i64,
  session: Option<AdminSession>,
  pool: &State<DbPool>,
) -> Result<Redirect, AdminReject> {
  let session = require_admin(session)?;
  let mut connection = pool.get().map_err(|_| Status::ServiceUnavailable)?;
  let _ = WebauthnCredential::delete(&mut connection, id, &session.owner);
  Ok(Redirect::to("/admin/passkeys"))
}

/// The passkey routes (the management page + the enrollment ceremony).
pub fn routes() -> Vec<Route> {
  routes![
    passkeys_page,
    register_begin,
    register_finish,
    passkey_delete,
    auth_begin,
    auth_finish,
  ]
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn disabled_config_yields_no_state() {
    assert!(
      build_state(&WebauthnConfig::default()).is_none(),
      "the default config is disabled, so no relying party is built"
    );
  }

  #[test]
  fn localhost_config_builds_a_relying_party() {
    let config = WebauthnConfig {
      enabled: true,
      rp_id: "localhost".to_string(),
      rp_origin: "http://localhost:8000".to_string(),
    };
    assert!(
      build_state(&config).is_some(),
      "a valid localhost relying party builds"
    );
  }

  #[test]
  fn invalid_origin_degrades_to_none_not_panic() {
    let config = WebauthnConfig {
      enabled: true,
      rp_id: "localhost".to_string(),
      rp_origin: "not a url".to_string(),
    };
    assert!(
      build_state(&config).is_none(),
      "an invalid origin disables passkeys gracefully (token path keeps working), never panics"
    );
  }
}
