-- Partial covering index for the per-service conversion-runtime report (`service_runtime_report` in
-- src/frontend/services.rs): its three scans select every `Info:cortex:runtime_ms` row for a service
-- (summary stats, histogram, slowest list). The general `log_infos_index (category, what, task_id)`
-- matches the category/what prefix but does NOT carry `details`, so each of the ~2.8M runtime rows
-- triggered a heap fetch (~12 GB random I/O per query, ~9 s each, x3 per page load). This partial
-- index holds only the runtime rows with `details` inlined, so all three scans go index-only and the
-- planner gets a real row estimate (it previously misestimated the runtime rows as ~59, actual 2.8M).
--
-- NOTE (production): a plain CREATE INDEX takes a SHARE lock (blocks writes) while it builds. On the
-- live ~530M-row `log_infos`, build it CONCURRENTLY out-of-band first and let this migration no-op
-- (the `IF NOT EXISTS` makes a pre-built index a no-op):
--   CREATE INDEX CONCURRENTLY IF NOT EXISTS log_infos_runtime_idx
--     ON log_infos (task_id) INCLUDE (details) WHERE category = 'cortex' AND what = 'runtime_ms';
create index if not exists log_infos_runtime_idx
  on log_infos (task_id) include (details) where category = 'cortex' and what = 'runtime_ms';
