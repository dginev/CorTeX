-- Extend the report_summary rollup (Arm 14 #6.2) from ROLLUP(what) to a full
-- ROLLUP(category, what), so a single materialized view serves all three aggregate report grains
-- with the non-summable COUNT(DISTINCT task) computed by Postgres at each grain:
--
--   * what-grain rows     (category_is_total = 0, what_is_total = 0) -> the "what" drill-down report
--   * category-grain rows (category_is_total = 0, what_is_total = 1) -> the "category" report
--   * severity grand total(category_is_total = 1, what_is_total = 1) -> the per-severity totals
--     (distinct logged tasks + total messages) the live report needed a second GROUP BY pass for.
--
-- This lets the read path (src/backend/reports.rs) derive every category/what report number from
-- indexed rollup lookups plus two cheap tasks-table counts -- no per-read log-table aggregation,
-- and no Redis cache. ROLLUP(a, b) yields exactly the three nested grouping sets above (it does NOT
-- emit a (what)-without-category set, unlike CUBE), which is precisely the category->what drill-down.
--
-- severity in {warning,error,fatal,invalid} keys off the task status (worst-message severity); the
-- 'info' branch is the all-messages dimension (log_infos over all completed, non-invalid tasks).
-- Status raw values: NoProblem -1, Warning -2, Error -3, Fatal -4, Invalid -5.
--
-- Freshness: refreshed by the dispatcher on the run-completion path AND at a daily cadence while a
-- run is in flight (a single conversion run can take ~weeks, so an event-only refresh is not enough
-- -- see src/dispatcher/finalize.rs). Plain `REFRESH MATERIALIZED VIEW` for now (brief lock);
-- `REFRESH ... CONCURRENTLY` is the R-4 follow-up (needs a UNIQUE index over the ROLLUP NULLs).
DROP MATERIALIZED VIEW IF EXISTS report_summary;

CREATE MATERIALIZED VIEW report_summary AS
  SELECT t.corpus_id, t.service_id, 'warning'::text AS severity,
         CASE WHEN GROUPING(COALESCE(l.category, '')) = 1 THEN NULL ELSE COALESCE(l.category, '') END AS category,
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END AS what,
         GROUPING(COALESCE(l.category, ''))::int AS category_is_total,
         GROUPING(COALESCE(l.what, ''))::int AS what_is_total,
         COUNT(DISTINCT l.task_id)::bigint AS task_count,
         COUNT(*)::bigint AS message_count
  FROM tasks t JOIN log_warnings l ON l.task_id = t.id
  WHERE t.status = -2
  GROUP BY t.corpus_id, t.service_id, ROLLUP(COALESCE(l.category, ''), COALESCE(l.what, ''))
  UNION ALL
  SELECT t.corpus_id, t.service_id, 'error'::text AS severity,
         CASE WHEN GROUPING(COALESCE(l.category, '')) = 1 THEN NULL ELSE COALESCE(l.category, '') END,
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END,
         GROUPING(COALESCE(l.category, ''))::int,
         GROUPING(COALESCE(l.what, ''))::int,
         COUNT(DISTINCT l.task_id)::bigint,
         COUNT(*)::bigint
  FROM tasks t JOIN log_errors l ON l.task_id = t.id
  WHERE t.status = -3
  GROUP BY t.corpus_id, t.service_id, ROLLUP(COALESCE(l.category, ''), COALESCE(l.what, ''))
  UNION ALL
  SELECT t.corpus_id, t.service_id, 'fatal'::text AS severity,
         CASE WHEN GROUPING(COALESCE(l.category, '')) = 1 THEN NULL ELSE COALESCE(l.category, '') END,
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END,
         GROUPING(COALESCE(l.category, ''))::int,
         GROUPING(COALESCE(l.what, ''))::int,
         COUNT(DISTINCT l.task_id)::bigint,
         COUNT(*)::bigint
  FROM tasks t JOIN log_fatals l ON l.task_id = t.id
  WHERE t.status = -4
  GROUP BY t.corpus_id, t.service_id, ROLLUP(COALESCE(l.category, ''), COALESCE(l.what, ''))
  UNION ALL
  SELECT t.corpus_id, t.service_id, 'invalid'::text AS severity,
         CASE WHEN GROUPING(COALESCE(l.category, '')) = 1 THEN NULL ELSE COALESCE(l.category, '') END,
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END,
         GROUPING(COALESCE(l.category, ''))::int,
         GROUPING(COALESCE(l.what, ''))::int,
         COUNT(DISTINCT l.task_id)::bigint,
         COUNT(*)::bigint
  FROM tasks t JOIN log_invalids l ON l.task_id = t.id
  WHERE t.status = -5
  GROUP BY t.corpus_id, t.service_id, ROLLUP(COALESCE(l.category, ''), COALESCE(l.what, ''))
  UNION ALL
  SELECT t.corpus_id, t.service_id, 'info'::text AS severity,
         CASE WHEN GROUPING(COALESCE(l.category, '')) = 1 THEN NULL ELSE COALESCE(l.category, '') END,
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END,
         GROUPING(COALESCE(l.category, ''))::int,
         GROUPING(COALESCE(l.what, ''))::int,
         COUNT(DISTINCT l.task_id)::bigint,
         COUNT(*)::bigint
  FROM tasks t JOIN log_infos l ON l.task_id = t.id
  WHERE t.status < 0 AND t.status > -5
  GROUP BY t.corpus_id, t.service_id, ROLLUP(COALESCE(l.category, ''), COALESCE(l.what, ''))
WITH DATA;

-- Reads pin (corpus, service, severity) and the grain (the two discriminators), then filter category.
CREATE INDEX report_summary_lookup_idx
  ON report_summary (corpus_id, service_id, severity, category_is_total, what_is_total, category);
