-- Revert to the non-unique lookup index (CONCURRENTLY refresh becomes unavailable again — pair this
-- with reverting src/backend/rollup.rs to a plain `REFRESH MATERIALIZED VIEW`).
CREATE INDEX report_summary_lookup_idx
  ON report_summary (corpus_id, service_id, severity, category_is_total, what_is_total, category);

DROP INDEX IF EXISTS report_summary_uniq_idx;
