// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Sandbox corpora (Arm 5) — carve a working subset out of a parent corpus by a **message-condition
//! filter**, as a first-class corpus an agent (or human) can iterate conversion campaigns on.
//!
//! A sandbox is an ordinary `corpora` row with two extra columns set: `parent_corpus_id` (the
//! corpus it was carved from) and `selection` (the filter predicate). The selection IS the
//! provenance — the predicate applied over the parent — so no per-task origin link is kept (owner
//! decision 2026-06-15). Sources are referenced **in place** (the sandbox shares the parent's entry
//! paths; nothing is copied) and the carved set is a **one-time snapshot** evaluated at creation.
//! See `docs/archive/SANDBOX_CORPORA.md`.

use diesel::result::Error;
use diesel::sql_types::{Integer, Text};
use diesel::*;
use serde::{Deserialize, Serialize};

use crate::concerns::CortexInsertable;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, NewSandboxCorpus};

/// The message-condition that defines a sandbox: a slice of the parent corpus's reports, addressed
/// by the same `(service, severity, category, what)` dimensions the reports use. Serialized
/// verbatim into the sandbox corpus's `selection` JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxSelection {
  /// the service whose conversion results are being filtered
  pub service_id: i32,
  /// severity level key (`no_problem` | `warning` | `error` | `fatal` | `invalid`)
  pub severity: String,
  /// optional message-category narrowing (e.g. `missing_file`)
  pub category: Option<String>,
  /// optional `what` narrowing within the category
  pub what: Option<String>,
  /// optional substring the parent `entry` path must contain, matched as `entry LIKE '%…%'` (e.g.
  /// `2506.` carves one arXiv month). `None`/empty = no narrowing.
  #[serde(default)]
  pub entry: Option<String>,
  /// optional hard cap on how many entries the carve captures — the first `n` by `entry` order
  /// (deterministic). `None`/non-positive = no cap.
  #[serde(default)]
  pub max_entries: Option<i64>,
}

impl SandboxSelection {
  /// A compact, human-readable summary of the carve filter — e.g.
  /// `severity=warning, category=missing_file, entry~2506., limit=100`. The single source of truth
  /// for both the sandbox corpus's stored `description` and the provenance surfaced on its corpus
  /// page / detail API, so the two never drift.
  pub fn filter_summary(&self) -> String {
    let mut summary = format!("severity={}", self.severity);
    if let Some(category) = &self.category {
      summary.push_str(&format!(", category={category}"));
    }
    if let Some(what) = &self.what {
      summary.push_str(&format!(", what={what}"));
    }
    if let Some(entry) = self
      .entry
      .as_deref()
      .map(str::trim)
      .filter(|e| !e.is_empty())
    {
      summary.push_str(&format!(", entry~{entry}"));
    }
    if let Some(n) = self.max_entries.filter(|n| *n > 0) {
      summary.push_str(&format!(", limit={n}"));
    }
    summary
  }
}

/// The result of carving a sandbox: the new corpus plus how many entries it captured.
pub struct SandboxOutcome {
  /// the freshly-created sandbox corpus
  pub sandbox: Corpus,
  /// number of parent entries that matched the selection (= number of `TODO` tasks created)
  pub entry_count: usize,
}

/// Carves a **sandbox corpus** from `parent` using `selection`, **entirely server-side**: it
/// inserts the sandbox `corpora` row, then a single `INSERT INTO tasks (...) SELECT ... FROM tasks
/// ...` materializes a `TODO` task per matched parent entry **without ever loading the entries into
/// the application**. A 100k-entry carve therefore costs no client RAM and no per-row bind
/// parameters (it sidesteps the 65535-parameter cap a client-side batch insert would hit); the
/// whole carve is one transaction, so it is atomic (no half-built sandbox). Because the matching
/// `SELECT` over a large parent can take minutes to an hour, this is meant to run as a **background
/// job** (`corpus_sandbox`).
///
/// A severity-only selection reads `tasks` directly; a `category`/`what` narrowing joins the
/// severity's `log_*` table (the table name comes from the fixed [`TaskStatus::to_table`] map, so
/// it is never user-controlled; ids/`category`/`what` are bound parameters). `SELECT DISTINCT`
/// collapses a parent task carrying several matching messages to a single carved entry — which also
/// satisfies the `tasks` `UNIQUE(entry, service_id, corpus_id)` constraint.
///
/// **Output-isolation note:** the sandbox is its own `corpus_id` (own tasks, runs, reports).
/// Running a *conversion* on it would, today, write result archives to the shared
/// `<entry-dir>/<service>.zip` path it inherits from the parent — so isolating a sandbox's **rerun
/// outputs** needs a follow-up (a sink output-path change), tracked in
/// `docs/archive/SANDBOX_CORPORA.md` + `docs/KNOWN_ISSUES.md`.
pub fn create_sandbox(
  connection: &mut PgConnection,
  parent: &Corpus,
  name: &str,
  selection: &SandboxSelection,
) -> Result<SandboxOutcome, Error> {
  let status = match TaskStatus::from_key(&selection.severity) {
    Some(severity) => severity.raw(),
    None => {
      return Err(Error::QueryBuilderError(
        format!("unknown severity '{}'", selection.severity).into(),
      ))
    },
  };
  let log_table = TaskStatus::from_raw(status).to_table();
  let selection_json = serde_json::to_value(selection).ok();

  // Optional entry-substring narrowing: `2506.` → `LIKE '%2506.%'`. Always bound (default `%` =
  // match every entry) so the three SQL branches share one extra bind slot. Trimmed; blank = no
  // narrowing.
  let entry_pattern = selection
    .entry
    .as_deref()
    .map(str::trim)
    .filter(|e| !e.is_empty())
    .map_or_else(|| "%".to_string(), |e| format!("%{e}%"));

  // Optional deterministic size cap: the first `n` entries by `entry` order. `n` is a validated
  // i64, so it is safe to inline (no bind needed; an integer has no injection surface).
  // Non-positive caps are ignored. Appended after the WHERE clause of each branch.
  let limit_clause = match selection.max_entries {
    Some(n) if n > 0 => format!(" ORDER BY t.entry LIMIT {n}"),
    _ => String::new(),
  };

  let description = format!(
    "Sandbox of '{}' (filter: {})",
    parent.name,
    selection.filter_summary()
  );

  let new_sandbox = NewSandboxCorpus {
    path: parent.path.clone(),
    name: name.to_string(),
    complex: parent.complex,
    description,
    parent_corpus_id: Some(parent.id),
    selection: selection_json,
  };

  let todo = TaskStatus::TODO.raw();
  let service_id = selection.service_id;
  let parent_id = parent.id;

  connection.transaction(|t_connection| {
    new_sandbox.create(t_connection)?;
    let sandbox = Corpus::find_by_name(name, t_connection)?;

    // Server-side carve: stream the matching parent entries straight into new sandbox tasks. The
    // affected-row count is the number of entries captured (no rows cross into the application).
    let entry_count = match (selection.category.as_deref(), selection.what.as_deref()) {
      (None, None) => sql_query(format!(
        "INSERT INTO tasks (service_id, corpus_id, status, entry) \
         SELECT $1, $2, $3, t.entry FROM tasks t \
         WHERE t.corpus_id = $4 AND t.service_id = $5 AND t.status = $6 \
         AND t.entry LIKE $7{limit_clause}"
      ))
      .bind::<Integer, _>(service_id)
      .bind::<Integer, _>(sandbox.id)
      .bind::<Integer, _>(todo)
      .bind::<Integer, _>(parent_id)
      .bind::<Integer, _>(service_id)
      .bind::<Integer, _>(status)
      .bind::<Text, _>(&entry_pattern)
      .execute(t_connection)?,
      (Some(category), None) => sql_query(format!(
        "INSERT INTO tasks (service_id, corpus_id, status, entry) \
         SELECT DISTINCT $1, $2, $3, t.entry FROM tasks t JOIN {log_table} l ON l.task_id = t.id \
         WHERE t.corpus_id = $4 AND t.service_id = $5 AND t.status = $6 AND l.category = $7 \
         AND t.entry LIKE $8{limit_clause}"
      ))
      .bind::<Integer, _>(service_id)
      .bind::<Integer, _>(sandbox.id)
      .bind::<Integer, _>(todo)
      .bind::<Integer, _>(parent_id)
      .bind::<Integer, _>(service_id)
      .bind::<Integer, _>(status)
      .bind::<Text, _>(category)
      .bind::<Text, _>(&entry_pattern)
      .execute(t_connection)?,
      (category, Some(what)) => sql_query(format!(
        "INSERT INTO tasks (service_id, corpus_id, status, entry) \
         SELECT DISTINCT $1, $2, $3, t.entry FROM tasks t JOIN {log_table} l ON l.task_id = t.id \
         WHERE t.corpus_id = $4 AND t.service_id = $5 AND t.status = $6 AND l.category = $7 \
         AND l.what = $8 AND t.entry LIKE $9{limit_clause}"
      ))
      .bind::<Integer, _>(service_id)
      .bind::<Integer, _>(sandbox.id)
      .bind::<Integer, _>(todo)
      .bind::<Integer, _>(parent_id)
      .bind::<Integer, _>(service_id)
      .bind::<Integer, _>(status)
      .bind::<Text, _>(category.unwrap_or(""))
      .bind::<Text, _>(what)
      .bind::<Text, _>(&entry_pattern)
      .execute(t_connection)?,
    };

    Ok(SandboxOutcome {
      sandbox,
      entry_count,
    })
  })
}
