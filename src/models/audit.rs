// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! The **accounting** pillar (AAA — `docs/AAA_DESIGN.md`): a persistent record of admin actions and
//! who took them ("observability of actions taken"). Auth-agnostic — `actor` is whatever the auth
//! layer resolved (today an admin token's `owner`), so this survives any future auth upgrade.

use chrono::NaiveDateTime;
use diesel::prelude::*;
use diesel::result::Error;

use crate::schema::audit_log;

/// A recorded admin action (the read model for the audit view). Not `Serialize` directly — like the
/// other timestamped models, the JSON/API surface uses a DTO with a formatted-string `at` (chrono's
/// serde feature is intentionally off crate-wide).
#[derive(Queryable, Identifiable, Debug, Clone)]
#[diesel(table_name = audit_log)]
pub struct AuditEntry {
  /// Auto-incremented id.
  pub id: i64,
  /// The identity that acted (the signed-in admin / API token owner; empty if unresolved).
  pub actor: String,
  /// What was done — a stable verb (`rerun`, `import_corpus`, `deactivate_service`, …).
  pub action: String,
  /// The resource acted on (e.g. `corpus` or `corpus/service`); may be empty.
  pub target: String,
  /// The result (an HTTP status or `ok`/`denied`); may be empty.
  pub outcome: String,
  /// Optional short context (a params summary); never secrets.
  pub details: String,
  /// When it happened (server clock).
  pub at: NaiveDateTime,
}

/// An admin action to record.
#[derive(Insertable, Debug)]
#[diesel(table_name = audit_log)]
pub struct NewAuditEntry {
  /// The acting identity (the token's `owner` / signed-in admin).
  pub actor: String,
  /// The action verb.
  pub action: String,
  /// The resource acted on.
  pub target: String,
  /// The outcome.
  pub outcome: String,
  /// Optional short context.
  pub details: String,
}

impl NewAuditEntry {
  /// A minimal entry: actor + action + target, no extra outcome/details.
  pub fn new(
    actor: impl Into<String>,
    action: impl Into<String>,
    target: impl Into<String>,
  ) -> Self {
    NewAuditEntry {
      actor: actor.into(),
      action: action.into(),
      target: target.into(),
      outcome: String::new(),
      details: String::new(),
    }
  }

  /// Sets the outcome (e.g. `ok`, `denied`, an HTTP status), builder-style.
  pub fn outcome(mut self, outcome: impl Into<String>) -> Self {
    self.outcome = outcome.into();
    self
  }

  /// Records the action. **Best-effort**: the caller should ignore an error (a failed audit write
  /// must never fail the action it describes — accounting is observability, not a gate).
  pub fn record(&self, connection: &mut PgConnection) -> Result<usize, Error> {
    diesel::insert_into(audit_log::table)
      .values(self)
      .execute(connection)
  }
}

impl AuditEntry {
  /// Lists recent audit entries, most-recent first, capped at `limit`.
  pub fn recent(connection: &mut PgConnection, limit: i64) -> Result<Vec<Self>, Error> {
    audit_log::table
      .order(audit_log::at.desc())
      .limit(limit)
      .get_results(connection)
  }
}
