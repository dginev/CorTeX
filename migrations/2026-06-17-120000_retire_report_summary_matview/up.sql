-- Retire the global `report_summary` materialized view.
--
-- The matview aggregated all five `log_*` tables across EVERY corpus into one cube. Refreshing it
-- (`REFRESH MATERIALIZED VIEW [CONCURRENTLY] report_summary`) was an O(~99 GB / 345M-row) scan that
-- took ~16 min under fleet load and starved the conversion finalize path, stalling the worker
-- fleet. The category/what drill-down reports never needed the global cube: each pins one
-- (corpus, service, severity) and reads from a per-scope cache (`report_grain_cache`, below) that is
-- (re)populated one slice at a time -- never the all-corpora cube -- so reporting can never again
-- stall conversions.
--
-- `report_summary_meta` (the refresh-time stamp) is intentionally kept: it is now unused by code
-- (`report_summary_refreshed_at` returns None) but harmless (single row), and keeping it avoids a
-- dangling `schema.rs` table declaration.
DROP MATERIALIZED VIEW IF EXISTS report_summary;

-- Replace it with a per-(corpus, service, severity)-SCOPED report cache. Unlike the matview, this is
-- NEVER refreshed globally: each slice is (re)populated on demand (cold-miss on a report view), on
-- that scope's rerun, on run-completion (the dispatcher invalidates only the touched scopes), or on
-- a manual force-refresh. A single slice's regeneration scans one log table for one corpus -- bounded,
-- never the all-corpora cube. The row shape mirrors the retired matview's ROLLUP(category, what)
-- grains, so the readers in src/backend/rollup.rs SELECT from it directly (written via raw SQL, so it
-- has no src/schema.rs declaration).
CREATE TABLE report_grain_cache (
  corpus_id         integer     NOT NULL,
  service_id        integer     NOT NULL,
  severity          text        NOT NULL,
  category          varchar,
  what              varchar,
  category_is_total integer     NOT NULL,
  what_is_total     integer     NOT NULL,
  task_count        bigint      NOT NULL,
  message_count     bigint      NOT NULL,
  computed_at       timestamptz NOT NULL DEFAULT now()
);
-- One row per grain; NULLS NOT DISTINCT so the NULL category/what total rows are unique and
-- ON CONFLICT can target them (mirrors the retired report_summary_uniq_idx).
CREATE UNIQUE INDEX report_grain_cache_uniq
  ON report_grain_cache (corpus_id, service_id, severity, category_is_total, what_is_total, category, what)
  NULLS NOT DISTINCT;
-- Slice lookup / invalidation by (corpus, service[, severity]).
CREATE INDEX report_grain_cache_scope
  ON report_grain_cache (corpus_id, service_id, severity);
