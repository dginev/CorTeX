//! Frontend logic responsible for managing ReCaptcha guards
use futures::{Future, Stream};
use hyper::header::{HeaderValue, CONTENT_LENGTH, CONTENT_TYPE};
use hyper::Client;
use hyper::{Body, Method, Request};
use hyper_tls::HttpsConnector;
use rocket::Data;
use serde::Deserialize;
use std::io::Read;
use std::str;
use tokio_core::reactor::Core;

const TOKEN_LIMIT: u64 = 512;
/// Safely cast a rocket Data input into a String (is this still needed?)
pub fn safe_data_to_string(data: Data) -> Result<String, std::io::Error> {
  let mut stream = data.open().take(TOKEN_LIMIT);
  let mut string = String::with_capacity((TOKEN_LIMIT / 2) as usize);
  stream.read_to_string(&mut string)?; // do we need str::from_utf8(token_bytes)
  Ok(string)
}

#[derive(Deserialize)]
struct IsSuccess {
  success: bool,
}

/// validate a recaptcha response against the captcha secret
pub fn check_captcha(g_recaptcha_response: &str, captcha_secret: &str) -> bool {
  let mut core = match Core::new() {
    Ok(c) => c,
    _ => return false,
  };
  let https = HttpsConnector::new(4).expect("TLS initialization failed");
  let client = Client::builder().build::<_, hyper::Body>(https);

  let mut verified = false;
  let url_with_query = "https://www.google.com/recaptcha/api/siteverify?secret=".to_string()
    + captcha_secret
    + "&response="
    + g_recaptcha_response;
  let json_str = format!(
    "{{\"secret\":\"{:?}\",\"response\":\"{:?}\"}}",
    captcha_secret, g_recaptcha_response
  );
  let req_url = match url_with_query.parse() {
    Ok(parsed) => parsed,
    _ => return false,
  };
  let json_len = json_str.len();
  let mut req = Request::new(Body::from(json_str));
  *req.method_mut() = Method::POST;
  *req.uri_mut() = req_url;
  req.headers_mut().insert(
    CONTENT_TYPE,
    HeaderValue::from_static("application/javascript"),
  );
  req
    .headers_mut()
    .insert(CONTENT_LENGTH, HeaderValue::from(json_len));

  let post = client
    .request(req)
    .and_then(|res| res.into_body().concat2());
  let posted = match core.run(post) {
    Ok(posted_data) => match str::from_utf8(&posted_data) {
      Ok(posted_str) => posted_str.to_string(),
      Err(e) => {
        println!("err: {}", e);
        return false;
      },
    },
    Err(e) => {
      println!("err: {}", e);
      return false;
    },
  };
  let json_decoded: Result<IsSuccess, _> = serde_json::from_str(&posted);
  if let Ok(response_json) = json_decoded {
    if response_json.success {
      verified = true;
    }
  }
  verified
}
