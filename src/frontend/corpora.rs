// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Corpus-management capability: list/inspect/import/delete corpora as screens + API.
//!
//! Follows the symmetry contract — one shared [`CorpusDto`] renders as JSON for agents and (later)
//! as HTML for humans. Handlers live here; the app is assembled in [`crate::frontend::server`].
//! This is the first capability drained out of the binary's legacy routes; more land per increment.

use rocket::serde::json::Json;
use rocket::{Route, State};
use serde::Serialize;

use crate::backend::DbPool;
use crate::models::Corpus;

/// A corpus as exposed over the API/UI. `name` is the stable external handle used by every route.
#[derive(Debug, Serialize)]
pub struct CorpusDto {
  /// Human-readable corpus name (its external handle).
  pub name: String,
  /// Filesystem path to the corpus root.
  pub path: String,
  /// Human-readable description.
  pub description: String,
  /// Whether documents are multi-file (complex) rather than a single TeX file.
  pub complex: bool,
}

impl From<Corpus> for CorpusDto {
  fn from(corpus: Corpus) -> Self {
    CorpusDto {
      name: corpus.name,
      path: corpus.path,
      description: corpus.description,
      complex: corpus.complex,
    }
  }
}

/// Lists all registered corpora (the agent twin of the overview screen).
#[get("/api/corpora")]
pub fn api_corpora(pool: &State<DbPool>) -> Json<Vec<CorpusDto>> {
  let corpora = match pool.get() {
    Ok(mut connection) => Corpus::all(&mut connection).unwrap_or_default(),
    Err(_) => Vec::new(),
  };
  Json(corpora.into_iter().map(CorpusDto::from).collect())
}

/// The route set for the corpus-management capability.
pub fn routes() -> Vec<Route> { routes![api_corpora] }
