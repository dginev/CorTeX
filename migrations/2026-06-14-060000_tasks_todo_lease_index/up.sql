-- Partial index for the ventilator's hot task-leasing query (`tasks_aggregate::fetch_tasks`):
--   SELECT * FROM tasks WHERE service_id = $1 AND status = 0  -- TODO
--   LIMIT $queue_size FOR UPDATE
--
-- The table already carries one partial index per *completed* status (`ok_index` … `invalid_index`,
-- WHERE status = -1 … -5) used by the report queries, but **none for TODO (status = 0)** — the
-- status the dispatcher leases. So leasing fell back to the broad `service_idx (service_id)` index;
-- on a mostly-processed corpus, where the leasable TODO rows are sparse among millions of completed
-- tasks for the same service, that scans many non-TODO index entries before a queue's worth of TODO
-- rows is found. This adds the missing sibling — same (status, service_id, corpus_id, id, entry)
-- shape, restricted to TODO — so leasing scans only the leasable rows regardless of how processed
-- the corpus is.
--
-- NOTE (production): a plain CREATE INDEX takes a SHARE lock (blocks writes) while it builds. On the
-- live multi-million-row `tasks` table, build it CONCURRENTLY out-of-band first and let this
-- migration no-op (the `IF NOT EXISTS` makes a pre-built index a no-op):
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS todo_index
--     ON tasks(status, service_id, corpus_id, id, entry) WHERE status = 0;
create index if not exists todo_index
  on tasks(status, service_id, corpus_id, id, entry) where status = 0;
