-- Denormalized per-task conversion runtime, written on the finalize path alongside the
-- `Info:cortex:runtime_ms` log row (src/backend/mark.rs::mark_done). It backs the per-service runtime
-- report (`service_runtime_report` in src/frontend/services.rs), which previously re-derived its
-- summary / histogram / slowest-list from a ~2.8M-row scan of `log_infos` JOINed to `tasks` (the
-- service filter lives on `tasks`, not `log_infos`, so every runtime row was fetched and probed) on
-- *every* page view — ~25 s, growing with every run. Here `service_id` is inlined, so:
--   * summary + histogram aggregate index-only over `(service_id, runtime_ms)` (no `tasks` join),
--   * the slowest list is a plain `(service_id, runtime_ms DESC)` index walk over LIMIT+OFFSET rows.
-- The narrow `runtime_ms` column is a validated int (the finalize parser + the backfill regex below
-- guarantee it), so the report's defensive `details ~ '^[0-9]+$'` cast is gone.
--
-- NOTE (production): the table + index are instant (empty), but the backfill scans ~2.8M rows. On the
-- live DB run the whole script out-of-band first (it is fast via the `log_infos_runtime_idx` partial
-- index from the prior migration) and let this migration no-op — `IF NOT EXISTS` + `ON CONFLICT DO
-- NOTHING` make a pre-populated table a no-op. On a fresh DB the backfill selects 0 rows.
create table if not exists task_runtimes (
  task_id    bigint  primary key references tasks(id) on delete cascade,
  service_id integer not null,
  runtime_ms integer not null
);

create index if not exists task_runtimes_service_runtime_idx
  on task_runtimes (service_id, runtime_ms desc);

insert into task_runtimes (task_id, service_id, runtime_ms)
  select li.task_id, t.service_id, (li.details)::int
  from log_infos li join tasks t on t.id = li.task_id
  where li.category = 'cortex' and li.what = 'runtime_ms' and li.details ~ '^[0-9]+$'
  on conflict (task_id) do nothing;
