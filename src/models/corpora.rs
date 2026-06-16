#![allow(clippy::extra_unused_lifetimes)]
use diesel::result::Error;
use diesel::*;
use serde::Serialize;
use std::collections::HashMap;

use crate::concerns::CortexInsertable;
use crate::schema::corpora;
use crate::schema::services;
use crate::schema::tasks;

use super::services::Service;

// Corpora

#[derive(Identifiable, Queryable, AsChangeset, Clone, Debug, Serialize)]
#[diesel(table_name = corpora)]
/// A minimal description of a document collection. Defined by a name, path and simple/complex file
/// system setup.
pub struct Corpus {
  /// auto-incremented postgres id
  pub id: i32,
  /// file system path to corpus root
  /// (a corpus is held in a single top-level directory)
  pub path: String,
  /// a human-readable name for this corpus
  pub name: String,
  /// are we using multiple files to represent a document entry?
  /// (if unsure, always use "true")
  pub complex: bool,
  /// a human-readable description of the corpus, maybe allow markdown here?
  pub description: String,
  /// For a **sandbox** corpus: the parent corpus it was carved from (else `None`). See
  /// [`crate::backend::sandbox`].
  pub parent_corpus_id: Option<i32>,
  /// For a **sandbox** corpus: the filter predicate it was built from — the JSON
  /// `{service, severity, category, what}` selection over the parent. This IS the sandbox's
  /// provenance ("why these entries"), so no per-task origin link is kept.
  pub selection: Option<serde_json::Value>,
  /// Stable external handle (UUIDv7, DB-generated), independent of the mutable `name` — for
  /// public/API references that must survive a rename. Immutable once assigned (Arm 3 / D8).
  pub public_id: uuid::Uuid,
}

impl Corpus {
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_name(name_query: &str, connection: &mut PgConnection) -> Result<Self, Error> {
    use crate::schema::corpora::name;
    corpora::table.filter(name.eq(name_query)).first(connection)
  }
  /// Find a corpus by its primary key (used to resolve a sandbox's parent).
  pub fn find_by_id(id_query: i32, connection: &mut PgConnection) -> Result<Self, Error> {
    corpora::table.find(id_query).first(connection)
  }
  /// The id under which this corpus's **result archives** are name-scoped, or `None` for an
  /// ordinary corpus. A sandbox (`parent_corpus_id.is_some()`) scopes its outputs by its own id
  /// so a rerun can't clobber the parent's archives — see [`crate::helpers::result_archive_path`]
  /// (F-6).
  pub fn sandbox_id(&self) -> Option<i32> { self.parent_corpus_id.map(|_| self.id) }
  /// Total number of tasks registered under this corpus (across all services) — the blast radius a
  /// [`Corpus::destroy`] would remove. Used to preview a destructive delete before committing.
  pub fn task_count(&self, connection: &mut PgConnection) -> Result<i64, Error> {
    tasks::table
      .filter(tasks::corpus_id.eq(self.id))
      .count()
      .get_result(connection)
  }
  /// ORM-like until diesel.rs introduces finders for more fields
  pub fn find_by_path(path_query: &str, connection: &mut PgConnection) -> Result<Self, Error> {
    use crate::schema::corpora::path;
    corpora::table.filter(path.eq(path_query)).first(connection)
  }
  /// Returns all registered corpora, ordered by name.
  pub fn all(connection: &mut PgConnection) -> Result<Vec<Self>, Error> {
    corpora::table
      .order(corpora::name.asc())
      .get_results(connection)
  }

  /// Document count per corpus id, for **every** corpus in **one** query (no N+1) — the number of
  /// `import`-service (id 2) tasks, which is one per ingested document. Used to show each corpus's
  /// scale on the overview/landing without a per-corpus count. A corpus with no import tasks (none
  /// ingested yet) is simply absent from the map (treat as 0).
  pub fn document_counts(connection: &mut PgConnection) -> HashMap<i32, i64> {
    use crate::schema::tasks::dsl::{corpus_id, service_id, tasks};
    use diesel::dsl::sql;
    use diesel::sql_types::BigInt;
    // The magic `import` service id is 2 (1=init, 2=import). Raw `count(*)` mirrors
    // `progress_report` and sidesteps Diesel's aggregate/group-by type check.
    tasks
      .select((corpus_id, sql::<BigInt>("count(*)")))
      .filter(service_id.eq(2))
      .group_by(corpus_id)
      .load::<(i32, i64)>(connection)
      .unwrap_or_default()
      .into_iter()
      .collect()
  }
  /// Return a hash representation of the corpus, usually for frontend reports
  pub fn to_hash(&self) -> HashMap<String, String> {
    let mut hm = HashMap::new();
    hm.insert("name".to_string(), self.name.clone());
    hm.insert("path".to_string(), self.path.clone());
    hm.insert("description".to_string(), self.description.clone());
    hm
  }

  /// Return a vector of services currently activated on this corpus
  pub fn select_services(&self, connection: &mut PgConnection) -> Result<Vec<Service>, Error> {
    use crate::schema::tasks::dsl::{corpus_id, service_id};
    let corpus_service_ids_query = tasks::table
      .select(service_id)
      .distinct()
      .filter(corpus_id.eq(self.id));
    let services_query = services::table.filter(services::id.eq_any(corpus_service_ids_query));
    let services: Vec<Service> = services_query.get_results(connection)?;
    Ok(services)
  }

  /// Deletes a corpus and **all** its dependent rows — the `log_*` messages, the tasks, and the
  /// corpus registration — consuming the object. Runs in a single transaction so a crash mid-delete
  /// can't leave a half-deleted corpus (crash-consistency, `docs/DESIGN_PRINCIPLES.md`).
  ///
  /// The `log_*` tables have **no** foreign key to `tasks` (the only FK is
  /// `historical_tasks.task_id → tasks ON DELETE CASCADE`), so their rows must be deleted
  /// explicitly **before** the tasks or they orphan — this is why deletion lives in one complete
  /// primitive rather than a bare `DELETE FROM corpora` (the CLAUDE.md "deleting a corpus orphans
  /// log_* rows" hazard, now closed at the source so every caller is safe).
  pub fn destroy(self, connection: &mut PgConnection) -> Result<usize, Error> {
    use crate::schema::{log_errors, log_fatals, log_infos, log_invalids, log_warnings};
    let corpus_id = self.id;
    let corpus_path = self.path;
    connection.transaction(|t_connection| {
      // The task ids of this corpus, rebuilt per delete (the subquery is consumed by `eq_any`).
      let task_ids = || {
        tasks::table
          .filter(tasks::corpus_id.eq(corpus_id))
          .select(tasks::id)
      };
      delete(log_infos::table.filter(log_infos::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      delete(log_warnings::table.filter(log_warnings::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      delete(log_errors::table.filter(log_errors::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      delete(log_fatals::table.filter(log_fatals::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      delete(log_invalids::table.filter(log_invalids::task_id.eq_any(task_ids())))
        .execute(t_connection)?;
      // all tasks for entries of this corpus (cascades to historical_tasks via its FK)
      delete(tasks::table)
        .filter(tasks::corpus_id.eq(corpus_id))
        .execute(t_connection)?;
      // the init task of this corpus
      delete(tasks::table)
        .filter(tasks::entry.eq(corpus_path))
        .filter(tasks::service_id.eq(1))
        .execute(t_connection)?;
      // the corpus registration
      delete(corpora::table)
        .filter(corpora::id.eq(corpus_id))
        .execute(t_connection)
    })
  }
}

/// Insertable `Corpus` struct
#[derive(Insertable)]
#[diesel(table_name = corpora)]
pub struct NewCorpus {
  /// file system path to corpus root
  /// (a corpus is held in a single top-level directory)
  pub path: String,
  /// a human-readable name for this corpus
  pub name: String,
  /// are we using multiple files to represent a document entry?
  /// (if unsure, always use "true")
  pub complex: bool,
  /// frontend-facing description of the corpus, maybe allow markdown here?
  pub description: String,
}
impl Default for NewCorpus {
  fn default() -> Self {
    NewCorpus {
      name: "mock corpus".to_string(),
      path: ".".to_string(),
      complex: true,
      description: String::new(),
    }
  }
}
impl CortexInsertable for NewCorpus {
  fn create(&self, connection: &mut PgConnection) -> Result<usize, Error> {
    insert_into(corpora::table).values(self).execute(connection)
  }
}

/// Insertable for a **sandbox** corpus — a `NewCorpus` plus the parent link and the filter
/// predicate that defines it. Kept separate from [`NewCorpus`] so ordinary corpus creation (the
/// import path, and dozens of test fixtures) stays a 4-field literal; the two sandbox columns are
/// nullable, so a plain `NewCorpus` insert simply leaves them `NULL`.
#[derive(Insertable)]
#[diesel(table_name = corpora)]
pub struct NewSandboxCorpus {
  /// file system path to corpus root — the sandbox references the **parent's** path in place
  /// (sources are not copied; owner decision 2026-06-15).
  pub path: String,
  /// a human-readable name for this sandbox
  pub name: String,
  /// inherited from the parent (same on-disk topology)
  pub complex: bool,
  /// frontend-facing description (auto-generated from the selection)
  pub description: String,
  /// the parent corpus this sandbox was carved from
  pub parent_corpus_id: Option<i32>,
  /// the filter predicate (`{service, severity, category, what}`) — the sandbox's provenance
  pub selection: Option<serde_json::Value>,
}
impl CortexInsertable for NewSandboxCorpus {
  fn create(&self, connection: &mut PgConnection) -> Result<usize, Error> {
    insert_into(corpora::table).values(self).execute(connection)
  }
}
