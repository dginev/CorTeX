//! CORS capabilities for the Rocket frontend
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::Header;
use std::io::Cursor;
/// Rocket solution for Cross-origin resource sharing
pub struct CORS();

#[rocket::async_trait]
impl Fairing for CORS {
  fn info(&self) -> Info {
    Info {
      name: "Add CORS headers to requests",
      kind: Kind::Response,
    }
  }

  async fn on_response<'r>(
    &self,
    request: &'r rocket::Request<'_>,
    response: &mut rocket::Response<'r>,
  ) {
    if request.method() == rocket::http::Method::Options
      || response.content_type() == Some(rocket::http::ContentType::JSON)
    {
      // The JSON surface is deliberately public, read-only data; agents authorize with an explicit
      // `X-Cortex-Token` header (never ambient cookies) and the admin UI is same-origin. So `*` is
      // the correct origin for public reads — but it MUST NOT be paired with
      // `Access-Control-Allow-Credentials: true`: that combination is spec-invalid (browsers reject
      // it) and signals credentialed cross-origin access we never want (a CSRF/data-theft footgun
      // on any cookie-resolvable `/api/*` route). We omit the credentials header entirely.
      response.set_header(Header::new("Access-Control-Allow-Origin", "*"));
      response.set_header(Header::new(
        "Access-Control-Allow-Methods",
        "POST, GET, OPTIONS",
      ));
      response.set_header(Header::new("Access-Control-Allow-Headers", "Content-Type"));
      response.set_header(Header::new(
        "Content-Security-Policy-Report-Only",
        "default-src https:; report-uri /csp-violation-report-endpoint/",
      ));
    }

    if request.method() == rocket::http::Method::Options {
      response.set_header(rocket::http::ContentType::Plain);
      response.set_sized_body(0, Cursor::new(""));
    }
  }
}

#[cfg(test)]
mod tests {
  use super::CORS;
  use rocket::local::blocking::Client;
  use rocket::serde::json::Json;

  #[rocket::get("/json")]
  fn json_route() -> Json<&'static str> { Json("ok") }

  fn client() -> Client {
    let rocket = rocket::build()
      .mount("/", rocket::routes![json_route])
      .attach(CORS());
    Client::tracked(rocket).expect("rocket client")
  }

  #[test]
  fn public_json_gets_wildcard_origin_but_no_credentials() {
    let client = client();
    let resp = client.get("/json").dispatch();
    let headers = resp.headers();
    assert_eq!(headers.get_one("Access-Control-Allow-Origin"), Some("*"));
    // The spec-invalid / unsafe `* + credentials` combination must never be emitted.
    assert_eq!(headers.get_one("Access-Control-Allow-Credentials"), None);
  }
}
