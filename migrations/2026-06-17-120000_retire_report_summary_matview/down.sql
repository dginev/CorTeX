-- Recreate the `report_summary` materialized view (full ROLLUP(category, what) over the five
-- severity branches) and its CONCURRENTLY-capable unique index, as they stood before this migration
-- (mirrors 2026-06-13-150000_report_summary_grand_total + 2026-06-14-040000_report_summary_concurrent_refresh).
-- NB: nothing reads this view after the retirement (the readers in src/backend/rollup.rs read the
-- per-scope `report_grain_cache`); this `down` exists only so the migration is reversible.
DROP TABLE IF EXISTS report_grain_cache;

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

CREATE UNIQUE INDEX report_summary_uniq_idx
  ON report_summary (corpus_id, service_id, severity, category_is_total, what_is_total, category, what)
  NULLS NOT DISTINCT;
