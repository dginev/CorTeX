// API-docs spike — rocket_okapi side. Annotates the corpora read slice as REAL Rocket routes and
// generates the OpenAPI spec from them, writing it to docs/api-spike/okapi-openapi.json.
//
// Ergonomics to note: the DTOs derive `JsonSchema` (schemars), and each route carries a single
// `#[openapi]` attribute *above its existing Rocket `#[get(...)]`* — the method, path, path params,
// and the response body are all inferred from the actual route signature and return type
// (`Json<Vec<CorpusDto>>`, `Result<Json<...>, Status>`). No restating of the operation.
#![allow(dead_code)] // the DTOs exist only to generate the schema; they are never constructed

#[macro_use]
extern crate rocket;

use rocket::http::Status;
use rocket::serde::json::Json;
use rocket_okapi::okapi::schemars;
use rocket_okapi::settings::OpenApiSettings;
use rocket_okapi::{openapi, openapi_get_routes_spec};
use schemars::JsonSchema;
use serde::Serialize;

/// A corpus as exposed over the API/UI (mirror of `frontend::corpora::CorpusDto`).
#[derive(Serialize, JsonSchema)]
struct CorpusDto {
  /// Human-readable corpus name (its external handle).
  name: String,
  /// Filesystem path to the corpus root.
  path: String,
  /// Human-readable description.
  description: String,
  /// Whether documents are multi-file (complex).
  complex: bool,
}

/// Per-service status counts within a corpus.
#[derive(Serialize, JsonSchema)]
struct ServiceStatusDto {
  name: String,
  version: f32,
  total: i64,
  no_problem: i64,
  warning: i64,
  error: i64,
  fatal: i64,
  invalid: i64,
  todo: i64,
}

/// A corpus with its activated services and status counts.
#[derive(Serialize, JsonSchema)]
struct CorpusDetailDto {
  name: String,
  path: String,
  description: String,
  complex: bool,
  services: Vec<ServiceStatusDto>,
}

/// List all registered corpora.
#[openapi]
#[get("/api/corpora")]
fn api_corpora() -> Json<Vec<CorpusDto>> { Json(vec![]) }

/// Inspect a single corpus: its activated services and per-service status counts.
#[openapi]
#[get("/api/corpora/<name>")]
fn api_corpus(name: &str) -> Result<Json<CorpusDetailDto>, Status> {
  let _ = name;
  Err(Status::NotFound)
}

fn main() {
  let settings = OpenApiSettings::default();
  let (_routes, spec) = openapi_get_routes_spec![settings: api_corpora, api_corpus];
  let json = serde_json::to_string_pretty(&spec).expect("serialize OpenAPI");
  std::fs::create_dir_all("docs/api-spike").expect("mkdir docs/api-spike");
  std::fs::write("docs/api-spike/okapi-openapi.json", &json).expect("write spec");
  println!(
    "rocket_okapi: wrote docs/api-spike/okapi-openapi.json ({} bytes)",
    json.len()
  );
}
