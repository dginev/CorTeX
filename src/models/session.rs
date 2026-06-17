// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Server-side admin sessions (`docs/archive/WEBAUTHN_DESIGN.md`). The browser cookie carries only
//! a random opaque session id; this table holds the owner + absolute expiry. **Both** human sign-in
//! paths — the admin **token** and a **passkey** — open a session, so the model is unified and the
//! cookie no longer carries a credential. Sign-out deletes the row (real revocation).

use chrono::{Duration, NaiveDateTime, Utc};
use diesel::prelude::*;
use diesel::result::Error;
use rand::distributions::Alphanumeric;
use rand::{Rng, thread_rng};

use crate::schema::sessions;

/// How long a session is valid from creation. **Absolute** expiry — no per-request sliding write,
/// so an authenticated request costs one indexed lookup and zero writes (performance).
pub const SESSION_TTL_DAYS: i64 = 7;

/// A server-side admin session.
#[derive(Queryable, Identifiable, Debug, Clone)]
#[diesel(table_name = sessions)]
pub struct Session {
  /// The opaque session id (the cookie value).
  pub id: String,
  /// The authenticated identity (the audit-log actor / token owner).
  pub owner: String,
  /// How the session was established: `token` or `passkey`.
  pub method: String,
  /// When the session was opened.
  pub created_at: NaiveDateTime,
  /// Absolute expiry; the session resolves only while `now < expires_at`.
  pub expires_at: NaiveDateTime,
}

#[derive(Insertable)]
#[diesel(table_name = sessions)]
struct NewSession<'a> {
  id: &'a str,
  owner: &'a str,
  method: &'a str,
  expires_at: NaiveDateTime,
}

impl Session {
  /// Opens a session for `owner` established via `method` (`token` | `passkey`) and returns the
  /// opaque session id to place in the cookie. Prunes expired rows first (best-effort
  /// housekeeping).
  pub fn open(connection: &mut PgConnection, owner: &str, method: &str) -> Result<String, Error> {
    let _ = Self::prune_expired(connection);
    // 48 url-safe chars (~285 bits) — an unguessable bearer; the security rests on this randomness.
    let id: String = thread_rng()
      .sample_iter(&Alphanumeric)
      .take(48)
      .map(char::from)
      .collect();
    let expires_at = (Utc::now() + Duration::days(SESSION_TTL_DAYS)).naive_utc();
    diesel::insert_into(sessions::table)
      .values(NewSession {
        id: &id,
        owner,
        method,
        expires_at,
      })
      .execute(connection)?;
    Ok(id)
  }

  /// Resolves a session id to its (unexpired) owner, or `None` if unknown/expired.
  pub fn resolve_owner(connection: &mut PgConnection, id: &str) -> Option<String> {
    use crate::schema::sessions::dsl;
    dsl::sessions
      .filter(dsl::id.eq(id))
      .filter(dsl::expires_at.gt(Utc::now().naive_utc()))
      .select(dsl::owner)
      .first::<String>(connection)
      .optional()
      .ok()
      .flatten()
  }

  /// Revokes (deletes) a single session — sign-out.
  pub fn revoke(connection: &mut PgConnection, id: &str) -> Result<(), Error> {
    use crate::schema::sessions::dsl;
    diesel::delete(dsl::sessions.filter(dsl::id.eq(id)))
      .execute(connection)
      .map(|_| ())
  }

  /// Revokes every session for an owner (sign-out-everywhere / a revoked token).
  pub fn revoke_all_for(connection: &mut PgConnection, owner: &str) -> Result<usize, Error> {
    use crate::schema::sessions::dsl;
    diesel::delete(dsl::sessions.filter(dsl::owner.eq(owner))).execute(connection)
  }

  /// Lists the live (unexpired) sessions, most-recent first — the admin "active sessions" view.
  pub fn active(connection: &mut PgConnection) -> Result<Vec<Self>, Error> {
    use crate::schema::sessions::dsl;
    dsl::sessions
      .filter(dsl::expires_at.gt(Utc::now().naive_utc()))
      .order(dsl::created_at.desc())
      .get_results(connection)
  }

  /// Deletes expired sessions (housekeeping); returns the number removed.
  pub fn prune_expired(connection: &mut PgConnection) -> Result<usize, Error> {
    use crate::schema::sessions::dsl;
    diesel::delete(dsl::sessions.filter(dsl::expires_at.le(Utc::now().naive_utc())))
      .execute(connection)
  }
}
