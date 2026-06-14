// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! The **accounting** pillar (AAA — `docs/AAA_DESIGN.md`): a Rocket fairing that records every
//! mutating admin request to the `audit_log`, so "who did what, when, to what, with what outcome"
//! is observable. Centralizing it in one fairing (rather than a call in each write handler) means
//! no endpoint can forget to log and new endpoints are audited automatically — drift-proof, in the
//! spirit of the symmetry contract.

use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::Method;
use rocket::{Request, Response};

use crate::backend::DbPool;
use crate::frontend::actor::resolve_actor;
use crate::models::NewAuditEntry;

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
    let actor = resolve_actor(request).unwrap_or_default();
    let outcome = response.status().code.to_string();
    let pool = pool.clone();
    rocket::tokio::task::spawn_blocking(move || {
      let entry = NewAuditEntry::new(actor, action, target).outcome(outcome);
      match pool.get() {
        Ok(mut connection) => {
          if let Err(error) = entry.record(&mut connection) {
            eprintln!("-- audit: failed to record {entry:?}: {error}");
          }
        },
        Err(error) => eprintln!("-- audit: pool exhausted, dropped {entry:?}: {error}"),
      }
    });
  }
}
