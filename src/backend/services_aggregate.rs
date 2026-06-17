use super::mark;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, Service};
use diesel::result::Error;
use diesel::sql_types::Integer;
use diesel::IntoSql;
use diesel::*;

pub(crate) fn register_service(
  connection: &mut PgConnection,
  service: &Service,
  corpus_path: &str,
  owner: String,
  description: String,
) -> Result<(), Error> {
  use crate::schema::tasks::dsl::*;
  use crate::schema::{log_errors, log_fatals, log_infos, log_invalids, log_warnings};
  let corpus = Corpus::find_by_path(corpus_path, connection)?;
  let todo_raw = TaskStatus::TODO.raw();
  let service_id_val = service.id;
  let corpus_id_val = corpus.id;

  // Idempotent-NEUTRAL guard (defense-in-depth; the HTTP layer also pre-checks and returns 409
  // without spawning a job). Activation below is destructive — it wipes & re-creates this pair's
  // tasks *and their `log_*` rows* — so registering a service on a corpus where it is **already
  // registered** would silently discard completed results. Refuse with no action. A caller that
  // wants to pick up newly-imported documents uses `extend_service`; one that wants to re-process
  // uses rerun. (Magic-service note: `import` itself is registered per-corpus and re-importing is a
  // different path; this guard is for `>2` real services.)
  let already_registered: i64 = tasks
    .filter(service_id.eq(service_id_val))
    .filter(corpus_id.eq(corpus_id_val))
    .count()
    .get_result(connection)?;
  if already_registered > 0 {
    return Err(Error::QueryBuilderError(
      format!(
        "service {:?} is already registered on corpus {:?} ({already_registered} tasks) — \
         registration is idempotent-neutral; use `extend` to add new documents or `rerun` to \
         re-process",
        service.name, corpus.name
      )
      .into(),
    ));
  }

  // The magic `import` service holds one entry per document; the new service activates over all of
  // them.
  let import_service = Service::find_by_name("import", connection)?;

  // (Re)activation atomically clears this <service, corpus> pair's prior tasks **and their `log_*`
  // rows**, then creates one TODO task per imported entry. The `log_*` tables have no FK to
  // `tasks` (the only FK is `historical_tasks → tasks ON DELETE CASCADE`), so deleting the tasks
  // alone would orphan their log rows on every re-activation — the same hazard closed in
  // `Corpus::destroy`. One transaction so a crash can't leave the service with its tasks deleted
  // but none re-created.
  connection.transaction::<(), Error, _>(|t_connection| {
    let prior_task_ids = || {
      tasks
        .filter(service_id.eq(service_id_val))
        .filter(corpus_id.eq(corpus_id_val))
        .select(id)
    };
    delete(log_infos::table.filter(log_infos::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(log_warnings::table.filter(log_warnings::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(log_errors::table.filter(log_errors::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(log_fatals::table.filter(log_fatals::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(log_invalids::table.filter(log_invalids::task_id.eq_any(prior_task_ids())))
      .execute(t_connection)?;
    delete(tasks)
      .filter(service_id.eq(service_id_val))
      .filter(corpus_id.eq(corpus_id_val))
      .execute(t_connection)?;
    // Create the conversion tasks with ONE server-side `INSERT … SELECT` — no entry list in RAM and
    // no per-document round-trip (KNOWN_ISSUES I-2). The guard above rejected an already-registered
    // pair and import entries are unique, so no row conflict is possible here.
    insert_into(tasks)
      .values(
        tasks
          .filter(service_id.eq(import_service.id))
          .filter(corpus_id.eq(corpus.id))
          .select((
            entry,
            service_id_val.into_sql::<Integer>(),
            corpus_id_val.into_sql::<Integer>(),
            todo_raw.into_sql::<Integer>(),
          )),
      )
      .into_columns((entry, service_id, corpus_id, status))
      .execute(t_connection)?;
    Ok(())
  })?;
  // Finally, register a new run, completing potentially open ones for this pair, attributed to the
  // actor who activated the service (threaded from the UI/API; the CLI passes a `cli-admin`
  // default).
  mark::mark_new_run(connection, &corpus, service, owner, description)
}

pub(crate) fn extend_service(
  connection: &mut PgConnection,
  service: &Service,
  corpus_path: &str,
) -> Result<(), Error> {
  use crate::schema::tasks::dsl::*;
  let corpus = Corpus::find_by_path(corpus_path, connection)?;
  let todo_raw = TaskStatus::TODO.raw();

  let import_service = Service::find_by_name("import", connection)?;
  // Queue a conversion task for every imported document this service doesn't already have one for —
  // only the genuinely-**new** ones (`ON CONFLICT (entry, service_id, corpus_id) DO NOTHING` is the
  // bulk twin of the old per-entry `create_if_new`, so re-running `extend` adds the new documents
  // and leaves existing tasks/results untouched). Done as ONE server-side `INSERT … SELECT`: no
  // import entry list is materialized in RAM and there is no per-document round-trip, so the work
  // is bounded regardless of corpus size (KNOWN_ISSUES I-2 — the old path loaded every import
  // entry into a `Vec` and issued one `create_if_new` per entry, ~1.5M for arxmliv). A single
  // statement is atomic, so no explicit transaction is needed.
  insert_into(tasks)
    .values(
      tasks
        .filter(service_id.eq(import_service.id))
        .filter(corpus_id.eq(corpus.id))
        .select((
          entry,
          service.id.into_sql::<Integer>(),
          corpus.id.into_sql::<Integer>(),
          todo_raw.into_sql::<Integer>(),
        )),
    )
    .into_columns((entry, service_id, corpus_id, status))
    .on_conflict_do_nothing()
    .execute(connection)?;
  Ok(())
}

/// Permanently destroys a service by name — its definition **plus all tasks + `log_*` rows across
/// every corpus**, in one transaction (orphan-free + crash-consistent; this *replaces* the latent
/// `delete_service_by_name`, which deleted only the `services` row and orphaned every dependent
/// `tasks`/`log_*` row — R-6). Refuses the magic `init` (1) and `import` (2) services, which are
/// infrastructure (deleting `import` would wipe a corpus's document registry): returns
/// [`Error::QueryBuilderError`] with a descriptive message rather than touching them. `Err` if the
/// service is unknown.
pub(crate) fn destroy_service_by_name(
  connection: &mut PgConnection,
  name: &str,
) -> Result<usize, Error> {
  let service = Service::find_by_name(name, connection)?;
  // Defense-in-depth: the frontend already rejects these with 403, but the data layer must never
  // let a careless caller wipe init/import. (Magic ids: 1=init, 2=import, >2=real services.)
  if service.id <= 2 {
    return Err(Error::QueryBuilderError(
      format!(
        "refusing to destroy the infrastructure service {:?} (id {})",
        name, service.id
      )
      .into(),
    ));
  }
  service.destroy(connection)
}
