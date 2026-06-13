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

use rocket::http::Status;
use rocket::request::{FromRequest, Outcome, Request};

use crate::config::config;

/// The authenticated initiator of a mutating request, resolved from a rerun token.
pub struct Actor {
  /// The human-readable owner the token maps to (recorded as the `owner` of the resulting action).
  pub owner: String,
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
