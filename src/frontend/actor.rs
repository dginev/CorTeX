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

/// Resolves a rerun token to its owner, mirroring the [`Actor`] guard's lookup. For **form-based**
/// human submissions (a `<form method=post>` token field), where the guard — which only reads the
/// `X-Cortex-Token` header or `?token=` query — can't see a token in the request body.
pub fn owner_for_token(token: &str) -> Option<String> {
  config().auth.rerun_tokens.get(token).cloned()
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
