// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Persistence for passkey (**WebAuthn**) sign-in (`docs/WEBAUTHN_DESIGN.md`): the per-owner
//! WebAuthn user handle and the enrolled public-key credentials. **Only public keys are stored** —
//! the `credential` column holds a serialized `webauthn_rs::prelude::Passkey` (public key +
//! counter), kept as opaque JSON here so the persistence layer does not depend on the WebAuthn
//! crate.

use chrono::NaiveDateTime;
use diesel::prelude::*;
use diesel::result::Error;
use serde_json::Value;
use uuid::Uuid;

use crate::schema::{webauthn_credentials, webauthn_users};

/// A WebAuthn user: the stable handle credentials are bound to, one per admin `owner`.
#[derive(Queryable, Identifiable, Debug, Clone)]
#[diesel(table_name = webauthn_users, primary_key(owner))]
pub struct WebauthnUser {
  /// The human identity (the audit-log actor / token owner).
  pub owner: String,
  /// The stable per-user handle WebAuthn binds credentials to.
  pub handle: Uuid,
  /// When the user was first enrolled.
  pub created_at: NaiveDateTime,
}

#[derive(Insertable)]
#[diesel(table_name = webauthn_users)]
struct NewWebauthnUser<'a> {
  owner: &'a str,
  handle: Uuid,
}

impl WebauthnUser {
  /// Returns the owner's stable WebAuthn handle, creating the user with a fresh random handle on
  /// first enrollment. Idempotent and concurrency-safe (`ON CONFLICT DO NOTHING`, then re-read).
  pub fn ensure(connection: &mut PgConnection, owner: &str) -> Result<Uuid, Error> {
    use crate::schema::webauthn_users::dsl;
    if let Some(handle) = dsl::webauthn_users
      .filter(dsl::owner.eq(owner))
      .select(dsl::handle)
      .first::<Uuid>(connection)
      .optional()?
    {
      return Ok(handle);
    }
    diesel::insert_into(webauthn_users::table)
      .values(NewWebauthnUser {
        owner,
        handle: Uuid::new_v4(),
      })
      .on_conflict(dsl::owner)
      .do_nothing()
      .execute(connection)?;
    // Re-read so a concurrent insert's handle (whichever won) is the one returned.
    dsl::webauthn_users
      .filter(dsl::owner.eq(owner))
      .select(dsl::handle)
      .first::<Uuid>(connection)
  }
}

/// An enrolled passkey row. `credential` is the serialized `Passkey` (public key + signature
/// counter + metadata) — public data only.
#[derive(Queryable, Identifiable, Debug, Clone)]
#[diesel(table_name = webauthn_credentials)]
pub struct WebauthnCredential {
  /// Auto-incremented id.
  pub id: i64,
  /// The owner this passkey authenticates as.
  pub owner: String,
  /// A human label for the authenticator (e.g. "MacBook Touch ID").
  pub label: String,
  /// The serialized `webauthn_rs::prelude::Passkey` (opaque public JSON here).
  pub credential: Value,
  /// When the passkey was enrolled.
  pub created_at: NaiveDateTime,
  /// Last successful authentication with this passkey (`None` until first used).
  pub last_used: Option<NaiveDateTime>,
}

#[derive(Insertable)]
#[diesel(table_name = webauthn_credentials)]
struct NewWebauthnCredential<'a> {
  owner: &'a str,
  label: &'a str,
  credential: &'a Value,
}

impl WebauthnCredential {
  /// Stores a newly-enrolled passkey for `owner`.
  pub fn store(
    connection: &mut PgConnection,
    owner: &str,
    label: &str,
    credential: &Value,
  ) -> Result<(), Error> {
    diesel::insert_into(webauthn_credentials::table)
      .values(NewWebauthnCredential {
        owner,
        label,
        credential,
      })
      .execute(connection)
      .map(|_| ())
  }

  /// All enrolled passkeys for `owner` (oldest first) — the set an authentication ceremony allows.
  pub fn for_owner(connection: &mut PgConnection, owner: &str) -> Result<Vec<Self>, Error> {
    use crate::schema::webauthn_credentials::dsl;
    dsl::webauthn_credentials
      .filter(dsl::owner.eq(owner))
      .order(dsl::id)
      .get_results(connection)
  }

  /// Replaces a credential's serialized state (e.g. the signature counter after a login) and stamps
  /// `last_used`. Called when WebAuthn reports the authenticator's counter advanced.
  pub fn update_after_use(
    connection: &mut PgConnection,
    id: i64,
    credential: &Value,
  ) -> Result<(), Error> {
    use crate::schema::webauthn_credentials::dsl;
    diesel::update(dsl::webauthn_credentials.filter(dsl::id.eq(id)))
      .set((
        dsl::credential.eq(credential),
        dsl::last_used.eq(chrono::Utc::now().naive_utc()),
      ))
      .execute(connection)
      .map(|_| ())
  }

  /// Stamps `last_used = now()` without changing the stored credential (login where the counter did
  /// not need an update).
  pub fn touch(connection: &mut PgConnection, id: i64) -> Result<(), Error> {
    use crate::schema::webauthn_credentials::dsl;
    diesel::update(dsl::webauthn_credentials.filter(dsl::id.eq(id)))
      .set(dsl::last_used.eq(chrono::Utc::now().naive_utc()))
      .execute(connection)
      .map(|_| ())
  }
}
