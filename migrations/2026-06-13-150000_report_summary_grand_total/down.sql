-- Revert report_summary to the ROLLUP(what)-only form (Arm 14 #6.1): one row per (corpus, service,
-- severity, category) plus a category-grain rollup row, without the severity grand-total set or the
-- category_is_total discriminator.
DROP MATERIALIZED VIEW IF EXISTS report_summary;

CREATE MATERIALIZED VIEW report_summary AS
  SELECT t.corpus_id, t.service_id, 'warning'::text AS severity,
         COALESCE(l.category, '') AS category,
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END AS what,
         GROUPING(COALESCE(l.what, ''))::int AS what_is_total,
         COUNT(DISTINCT l.task_id)::bigint AS task_count,
         COUNT(*)::bigint AS message_count
  FROM tasks t JOIN log_warnings l ON l.task_id = t.id
  WHERE t.status = -2
  GROUP BY t.corpus_id, t.service_id, COALESCE(l.category, ''), ROLLUP(COALESCE(l.what, ''))
  UNION ALL
  SELECT t.corpus_id, t.service_id, 'error'::text AS severity,
         COALESCE(l.category, ''),
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END,
         GROUPING(COALESCE(l.what, ''))::int,
         COUNT(DISTINCT l.task_id)::bigint,
         COUNT(*)::bigint
  FROM tasks t JOIN log_errors l ON l.task_id = t.id
  WHERE t.status = -3
  GROUP BY t.corpus_id, t.service_id, COALESCE(l.category, ''), ROLLUP(COALESCE(l.what, ''))
  UNION ALL
  SELECT t.corpus_id, t.service_id, 'fatal'::text AS severity,
         COALESCE(l.category, ''),
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END,
         GROUPING(COALESCE(l.what, ''))::int,
         COUNT(DISTINCT l.task_id)::bigint,
         COUNT(*)::bigint
  FROM tasks t JOIN log_fatals l ON l.task_id = t.id
  WHERE t.status = -4
  GROUP BY t.corpus_id, t.service_id, COALESCE(l.category, ''), ROLLUP(COALESCE(l.what, ''))
  UNION ALL
  SELECT t.corpus_id, t.service_id, 'invalid'::text AS severity,
         COALESCE(l.category, ''),
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END,
         GROUPING(COALESCE(l.what, ''))::int,
         COUNT(DISTINCT l.task_id)::bigint,
         COUNT(*)::bigint
  FROM tasks t JOIN log_invalids l ON l.task_id = t.id
  WHERE t.status = -5
  GROUP BY t.corpus_id, t.service_id, COALESCE(l.category, ''), ROLLUP(COALESCE(l.what, ''))
  UNION ALL
  SELECT t.corpus_id, t.service_id, 'info'::text AS severity,
         COALESCE(l.category, ''),
         CASE WHEN GROUPING(COALESCE(l.what, '')) = 1 THEN NULL ELSE COALESCE(l.what, '') END,
         GROUPING(COALESCE(l.what, ''))::int,
         COUNT(DISTINCT l.task_id)::bigint,
         COUNT(*)::bigint
  FROM tasks t JOIN log_infos l ON l.task_id = t.id
  WHERE t.status < 0 AND t.status > -5
  GROUP BY t.corpus_id, t.service_id, COALESCE(l.category, ''), ROLLUP(COALESCE(l.what, ''))
WITH DATA;

CREATE INDEX report_summary_lookup_idx
  ON report_summary (corpus_id, service_id, severity, what_is_total, category);
