-- Performance: `jobs::list_recent` always runs `ORDER BY created_at DESC LIMIT N` — it backs the
-- `/jobs` dashboard, `GET /api/jobs`, the fleet-wide pending-check, the report-refresh + reindex
-- debounces, and (since W-4) the stale-job reaper. The table had only `jobs_status_idx` and
-- `jobs_kind_idx` (neither helps the ordering), so as the table grows one row per import / activation
-- / refresh / reindex, that query degraded to a Seq Scan + in-memory Sort. A `created_at DESC` index
-- turns it into a direct ordered index scan + limit (O(N)), independent of table size.
CREATE INDEX IF NOT EXISTS jobs_created_at_idx ON jobs (created_at DESC);
