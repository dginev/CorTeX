// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! HTTP error catchers: one consistent, **content-negotiated** error response across the whole
//! surface, instead of Rocket's built-in default page.
//!
//! An agent (a request under `/api`, or one sending `Accept: application/json`) gets a JSON
//! `{ "error", "status" }`; a human gets the themed HTML error page (`templates/error`). The status
//! code is unchanged — only the body shape — so this is the error-path half of the symmetry
//! contract (humans and agents get the same information in their native form).

use rocket::http::Status;
use rocket::request::Request;
use rocket::response::{self, Responder};
use rocket::serde::json::Json;
use rocket::{catch, catchers, Catcher};
use rocket_dyn_templates::{context, Template};
use serde_json::json;

/// A status + message rendered as JSON for agents and themed HTML for humans, the branch chosen
/// from the request (a `/api` path or an `application/json` `Accept` ⇒ JSON).
struct NegotiatedError {
  status: Status,
  message: &'static str,
}

impl<'r> Responder<'r, 'static> for NegotiatedError {
  fn respond_to(self, request: &'r Request<'_>) -> response::Result<'static> {
    let wants_json = request.uri().path().starts_with("/api")
      || request
        .headers()
        .get_one("Accept")
        .is_some_and(|accept| accept.contains("application/json"));
    if wants_json {
      let body = Json(json!({ "error": self.message, "status": self.status.code }));
      (self.status, body).respond_to(request)
    } else {
      let global = json!({
        "title": format!("{} · {}", self.status.code, self.message),
        "description": self.message,
      });
      let page = Template::render(
        "error",
        context! { global, status: self.status.code, message: self.message },
      );
      (self.status, page).respond_to(request)
    }
  }
}

#[catch(400)]
fn bad_request(_request: &Request) -> NegotiatedError {
  NegotiatedError {
    status: Status::BadRequest,
    message: "Bad request",
  }
}

#[catch(401)]
fn unauthorized(_request: &Request) -> NegotiatedError {
  NegotiatedError {
    status: Status::Unauthorized,
    message: "Unauthorized — a valid token is required",
  }
}

#[catch(403)]
fn forbidden(_request: &Request) -> NegotiatedError {
  NegotiatedError {
    status: Status::Forbidden,
    message: "Forbidden — this action is not permitted (e.g. a protected init/import service)",
  }
}

#[catch(404)]
fn not_found(_request: &Request) -> NegotiatedError {
  NegotiatedError {
    status: Status::NotFound,
    message: "Not found",
  }
}

#[catch(409)]
fn conflict(_request: &Request) -> NegotiatedError {
  NegotiatedError {
    status: Status::Conflict,
    message: "Conflict — the resource already exists, or the run is busy (e.g. tasks in progress)",
  }
}

#[catch(422)]
fn unprocessable(_request: &Request) -> NegotiatedError {
  NegotiatedError {
    status: Status::UnprocessableEntity,
    message: "Unprocessable request — check the submitted values (e.g. an unreadable path or an \
              unknown severity/grouping)",
  }
}

#[catch(500)]
fn internal_error(_request: &Request) -> NegotiatedError {
  NegotiatedError {
    status: Status::InternalServerError,
    message: "Internal server error",
  }
}

#[catch(503)]
fn unavailable(_request: &Request) -> NegotiatedError {
  NegotiatedError {
    status: Status::ServiceUnavailable,
    message: "Service unavailable — try again shortly",
  }
}

/// The catcher set to register on the Rocket instance (`server::mount_api_with`).
pub fn catchers() -> Vec<Catcher> {
  catchers![
    bad_request,
    unauthorized,
    forbidden,
    not_found,
    conflict,
    unprocessable,
    internal_error,
    unavailable
  ]
}
