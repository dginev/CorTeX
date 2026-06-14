// API-docs spike — utoipa side. Annotates the corpora read slice the SAME way the real routes are
// shaped, generates the OpenAPI spec, and writes it to docs/api-spike/utoipa-openapi.json.
//
// Ergonomics to note: the DTOs derive `ToSchema`, but each operation is re-declared in a
// `#[utoipa::path(...)]` macro on a *separate* (here empty) fn — method, path, params and response
// types are restated, independent of the actual Rocket route. utoipa is framework-agnostic, so it
// never sees the Rocket `#[get(...)]` attribute or the handler's real return type.
#![allow(dead_code)] // the DTOs/fns exist only to generate the schema; they are never constructed

use serde::Serialize;
use utoipa::{OpenApi, ToSchema};

/// A corpus as exposed over the API/UI (mirror of `frontend::corpora::CorpusDto`).
#[derive(Serialize, ToSchema)]
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
#[derive(Serialize, ToSchema)]
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
#[derive(Serialize, ToSchema)]
struct CorpusDetailDto {
  name: String,
  path: String,
  description: String,
  complex: bool,
  services: Vec<ServiceStatusDto>,
}

/// List all registered corpora.
#[utoipa::path(
  get,
  path = "/api/corpora",
  responses((status = 200, description = "All registered corpora", body = [CorpusDto]))
)]
fn api_corpora() {}

/// Inspect a single corpus: its activated services and per-service status counts.
#[utoipa::path(
  get,
  path = "/api/corpora/{name}",
  params(("name" = String, Path, description = "Corpus name (external handle)")),
  responses(
    (status = 200, description = "Corpus detail", body = CorpusDetailDto),
    (status = 404, description = "Unknown corpus"),
  )
)]
fn api_corpus() {}

#[derive(OpenApi)]
#[openapi(
  paths(api_corpora, api_corpus),
  components(schemas(CorpusDto, CorpusDetailDto, ServiceStatusDto))
)]
struct ApiDoc;

fn main() {
  let spec = ApiDoc::openapi()
    .to_pretty_json()
    .expect("serialize OpenAPI");
  std::fs::create_dir_all("docs/api-spike").expect("mkdir docs/api-spike");
  std::fs::write("docs/api-spike/utoipa-openapi.json", &spec).expect("write spec");
  println!(
    "utoipa: wrote docs/api-spike/utoipa-openapi.json ({} bytes)",
    spec.len()
  );
}
