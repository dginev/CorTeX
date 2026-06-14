-- R-4: let `report_summary` refresh with `REFRESH MATERIALIZED VIEW CONCURRENTLY`.
--
-- The rollup is rebuilt on the dispatcher's run-completion (drain) path and at a daily cadence
-- (`src/dispatcher/finalize.rs`). A plain `REFRESH MATERIALIZED VIEW` takes an ACCESS EXCLUSIVE lock
-- on the view for the entire rebuild, which on the production-scale corpus (5.87M tasks, 273M
-- `log_infos` + 61M `log_warnings` + 10M `log_errors` rows) measured at **~2 min 13 s** — during which
-- every report read blocks. `REFRESH ... CONCURRENTLY` avoids that lock (readers see the old rollup
-- until the new one is ready); measured at the same ~2 min 14 s, so it is ~free for the writer and a
-- large availability win for readers.
--
-- CONCURRENTLY requires a UNIQUE index that identifies every row. The `ROLLUP(category, what)` grain
-- emits NULL `category`/`what` for the per-category subtotal and the per-severity grand-total rows, so
-- the index is declared `NULLS NOT DISTINCT` (PostgreSQL 15+) to treat those NULLs as equal and
-- actually enforce uniqueness across the three grouping sets (otherwise NULLs are distinct and the
-- total rows would not be uniquely keyed, which CONCURRENTLY rejects).
--
-- The previous `report_summary_lookup_idx` (corpus_id, service_id, severity, category_is_total,
-- what_is_total, category) is a strict prefix of this unique index, so it is now redundant for the
-- read path (`src/backend/rollup.rs` lookups) and is dropped.
CREATE UNIQUE INDEX report_summary_uniq_idx
  ON report_summary (corpus_id, service_id, severity, category_is_total, what_is_total, category, what)
  NULLS NOT DISTINCT;

DROP INDEX IF EXISTS report_summary_lookup_idx;
