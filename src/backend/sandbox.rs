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
use diesel::sql_types::Text;
use diesel::*;
use serde::{Deserialize, Serialize};

use crate::concerns::CortexInsertable;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, NewSandboxCorpus};

/// The filter that defines a sandbox: a slice of the parent corpus addressed by independent,
/// **intersected** task-status and message (`severity`/`category`/`what`) dimensions (Model C),
/// plus optional entry/limit narrowing. Serialized verbatim into the sandbox corpus's `selection`
/// JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxSelection {
  /// the service whose conversion results are being filtered
  pub service_id: i32,
  /// optional **task-status** filter (`no_problem` | `warning` | `error` | `fatal` | `invalid`) —
  /// the task's overall outcome. Intersected with the message filter below.
  #[serde(default)]
  pub status: Option<String>,
  /// optional **message-severity** filter (`info` | `warning` | `error` | `fatal` | `invalid`) —
  /// the carve keeps tasks that *emitted* a message of this severity (any task status).
  /// `category`/`what` narrow within it.
  #[serde(default)]
  pub message_severity: Option<String>,
  /// optional message-category narrowing (needs `message_severity`)
  #[serde(default)]
  pub category: Option<String>,
  /// optional `what` narrowing within the category (needs `category`)
  #[serde(default)]
  pub what: Option<String>,
  /// optional substring the parent `entry` path must contain, matched as `entry LIKE '%…%'` (e.g.
  /// `2506.` carves one arXiv month). `None`/empty = no narrowing.
  #[serde(default)]
  pub entry: Option<String>,
  /// optional hard cap on how many entries the carve captures — the first `n` by `entry` order
  /// (deterministic). `None`/non-positive = no cap.
  #[serde(default)]
  pub max_entries: Option<i64>,
  /// **Legacy** (pre-Model-C) single overloaded severity — kept only so selections stored before
  /// the status/message split still render their provenance. Never set by new carves.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub severity: Option<String>,
}

/// The message-severity → `log_*` table map (includes `info`, which has no [`TaskStatus`]). The
/// only thing the carve interpolates into SQL, and it comes from this fixed map — never user input.
fn message_log_table(message_severity: &str) -> Option<&'static str> {
  match message_severity {
    "info" => Some("log_infos"),
    "warning" => Some("log_warnings"),
    "error" => Some("log_errors"),
    "fatal" => Some("log_fatals"),
    "invalid" => Some("log_invalids"),
    _ => None,
  }
}

/// A validated, resolved sandbox filter: the task-status raw int to match (if any) and the message
/// `log_*` table to look in (if any). The carve uses both; the pre-flight just checks for `Err`.
pub struct ResolvedFilter {
  /// raw task-status int to match `t.status` against, or `None` for "any status"
  pub status_raw: Option<i32>,
  /// the `log_*` table a message filter looks in, or `None` for "no message filter"
  pub message_table: Option<&'static str>,
}

impl SandboxSelection {
  /// A compact, human-readable summary of the carve filter — e.g.
  /// `status=no_problem, message=error/missing_file, entry~2506., limit=100`. The single source of
  /// truth for both the sandbox corpus's stored `description` and the provenance surfaced on its
  /// corpus page / detail API, so the two never drift. Falls back to the legacy `severity` for
  /// selections stored before the status/message split.
  pub fn filter_summary(&self) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(status) = &self.status {
      parts.push(format!("status={status}"));
    }
    if let Some(message_severity) = &self.message_severity {
      let mut message = format!("message={message_severity}");
      if let Some(category) = &self.category {
        message.push_str(&format!("/{category}"));
      }
      if let Some(what) = &self.what {
        message.push_str(&format!("/{what}"));
      }
      parts.push(message);
    }
    // Legacy stored selections only had the overloaded `severity` (+ its category/what).
    if parts.is_empty()
      && let Some(severity) = &self.severity
    {
      parts.push(format!("severity={severity}"));
      if let Some(category) = &self.category {
        parts.push(format!("category={category}"));
      }
      if let Some(what) = &self.what {
        parts.push(format!("what={what}"));
      }
    }
    if let Some(entry) = self
      .entry
      .as_deref()
      .map(str::trim)
      .filter(|e| !e.is_empty())
    {
      parts.push(format!("entry~{entry}"));
    }
    if let Some(n) = self.max_entries.filter(|n| *n > 0) {
      parts.push(format!("limit={n}"));
    }
    parts.join(", ")
  }

  /// Validates the **intersecting** task-status and message filters and resolves them for the
  /// carve, or returns a human-readable reason. They are independent dimensions (Model C):
  /// `status` matches the task's outcome; `message_severity` (+ `category`/`what`) matches a log
  /// message the task emitted, **at any task status** — so `info`+category collects every task
  /// that emitted that info message. `category` needs a `message_severity`; `what` needs a
  /// `category`; at least one of the two dimensions must be present. The **pre-flight twin** of
  /// the carve (`start_sandbox` calls it for an immediate `422`).
  pub fn validate(&self) -> Result<ResolvedFilter, String> {
    let status_raw = match self.status.as_deref() {
      Some(key) => Some(
        TaskStatus::from_key(key)
          .ok_or_else(|| {
            format!("unknown task status '{key}' (use no_problem, warning, error, fatal, invalid)")
          })?
          .raw(),
      ),
      None => None,
    };
    let message_table = match self.message_severity.as_deref() {
      Some(key) => Some(message_log_table(key).ok_or_else(|| {
        format!("unknown message severity '{key}' (use info, warning, error, fatal, invalid)")
      })?),
      None => None,
    };
    if message_table.is_none() && (self.category.is_some() || self.what.is_some()) {
      return Err("category/what need a message_severity to filter on".to_string());
    }
    if self.what.is_some() && self.category.is_none() {
      return Err("what needs a category".to_string());
    }
    if status_raw.is_none() && message_table.is_none() {
      return Err(
        "a sandbox needs at least a task-status or a message-severity filter".to_string(),
      );
    }
    Ok(ResolvedFilter {
      status_raw,
      message_table,
    })
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
  // Validation (the intersecting status + message filters) lives in `validate`, so the carve and
  // the `start_sandbox` pre-flight reject the identical set.
  let ResolvedFilter {
    status_raw,
    message_table,
  } = selection
    .validate()
    .map_err(|message| Error::QueryBuilderError(message.into()))?;
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

    // Server-side carve as ONE `INSERT … SELECT` (no rows cross into the app). The two filters are
    // INTERSECTED: an optional `t.status = <status>` and, independently, an optional EXISTS over
    // the message `log_*` table — so a message filter matches a task **at any status**.
    // Validated ints (ids, status, limit) are inlined (no injection surface); the only user
    // strings (category / what / entry) are bound; the only interpolated identifier is the
    // fixed-map `log_*` table name.
    let sandbox_id = sandbox.id;
    let status_clause = match status_raw {
      Some(status) => format!(" AND t.status = {status}"),
      None => String::new(),
    };
    let base = format!(
      "INSERT INTO tasks (service_id, corpus_id, status, entry) \
       SELECT {service_id}, {sandbox_id}, {todo}, t.entry FROM tasks t \
       WHERE t.corpus_id = {parent_id} AND t.service_id = {service_id}{status_clause}"
    );
    let entry_count = match (
      message_table,
      selection.category.as_deref(),
      selection.what.as_deref(),
    ) {
      // No message filter (status-only / entry-only): a plain status + entry scan.
      (None, _, _) => sql_query(format!("{base} AND t.entry LIKE $1{limit_clause}"))
        .bind::<Text, _>(&entry_pattern)
        .execute(t_connection)?,
      // Message severity, no category: tasks that emitted ANY message of that severity.
      (Some(table), None, _) => sql_query(format!(
        "{base} AND EXISTS (SELECT 1 FROM {table} l WHERE l.task_id = t.id) \
         AND t.entry LIKE $1{limit_clause}"
      ))
      .bind::<Text, _>(&entry_pattern)
      .execute(t_connection)?,
      // Message severity + category.
      (Some(table), Some(category), None) => sql_query(format!(
        "{base} AND EXISTS (SELECT 1 FROM {table} l WHERE l.task_id = t.id AND l.category = $1) \
         AND t.entry LIKE $2{limit_clause}"
      ))
      .bind::<Text, _>(category)
      .bind::<Text, _>(&entry_pattern)
      .execute(t_connection)?,
      // Message severity + category + what.
      (Some(table), Some(category), Some(what)) => sql_query(format!(
        "{base} AND EXISTS (SELECT 1 FROM {table} l WHERE l.task_id = t.id AND l.category = $1 \
         AND l.what = $2) AND t.entry LIKE $3{limit_clause}"
      ))
      .bind::<Text, _>(category)
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

#[cfg(test)]
mod tests {
  use super::*;

  fn sel(
    status: Option<&str>,
    message_severity: Option<&str>,
    category: Option<&str>,
    what: Option<&str>,
  ) -> SandboxSelection {
    SandboxSelection {
      service_id: 1,
      status: status.map(String::from),
      message_severity: message_severity.map(String::from),
      category: category.map(String::from),
      what: what.map(String::from),
      entry: None,
      max_entries: None,
      severity: None,
    }
  }

  #[test]
  fn model_c_validation_is_status_and_message_intersected() {
    // The F-7 fix: `info`+category is now VALID (info is a real message severity, `log_infos`;
    // `conversion` is a real info category — unlike `missing_file`, which is a *warning* category).
    assert!(
      sel(None, Some("info"), Some("conversion"), None)
        .validate()
        .is_ok()
    );
    // status + message intersect; status-only; message-only (any status) — all valid.
    assert!(
      sel(
        Some("no_problem"),
        Some("warning"),
        Some("missing_file"),
        None
      )
      .validate()
      .is_ok()
    );
    assert!(sel(Some("warning"), None, None, None).validate().is_ok());
    assert!(sel(None, Some("error"), None, None).validate().is_ok());

    // category needs a message_severity; what needs a category.
    assert!(
      sel(Some("no_problem"), None, Some("x"), None)
        .validate()
        .is_err()
    );
    assert!(
      sel(None, Some("warning"), None, Some("x"))
        .validate()
        .is_err()
    );
    // `info` is a message severity, NOT a task status — rejected in the status slot.
    assert!(sel(Some("info"), None, None, None).validate().is_err());
    // an empty selection (no dimension) is rejected.
    assert!(sel(None, None, None, None).validate().is_err());
  }

  #[test]
  fn legacy_selection_still_renders_its_provenance() {
    // A pre-Model-C stored selection (only `severity`) must still summarise for the provenance UI.
    let mut legacy = sel(None, None, Some("missing_file"), None);
    legacy.severity = Some("warning".to_string());
    let summary = legacy.filter_summary();
    assert!(summary.contains("severity=warning"), "got {summary:?}");
    assert!(summary.contains("category=missing_file"), "got {summary:?}");
  }
}
