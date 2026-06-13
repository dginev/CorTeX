-- Materialized rollup for the expensive category/what drill-down reports (Arm 14 #6).
--
-- Replaces the O(millions of log rows) join+group+sort (~500 ms warm, spills to disk -- the reason
-- the Redis cache exists) with an indexed, sub-millisecond lookup: cheap AND fresh (refreshed on the
-- run-completion path), which lets us drop the hard Redis dependency.
--
-- One row per (corpus_id, service_id, severity, category, what). ROLLUP(what) yields, per
-- severity+category:
--   * one row per `what`            (what_is_total = 0) -> the "what" drill-down report
--   * one category-grain rollup row (what_is_total = 1, what = NULL) whose task_count is
--     COUNT(DISTINCT task) over all whats -> the "category" report. (Distinct-task counts cannot be
--     summed from the per-what rows, so Postgres computes them here.)
--
-- severity ∈ {warning,error,fatal,invalid} keys off the task status (the worst-message severity);
-- the 'info' branch is the all-messages dimension (log_infos over all completed, non-invalid tasks).
-- Status raw values: NoProblem -1, Warning -2, Error -3, Fatal -4, Invalid -5.
--
-- NOTE: refreshed with plain `REFRESH MATERIALIZED VIEW` for now (brief lock during the infrequent
-- run-completion refresh). `REFRESH ... CONCURRENTLY` is a follow-up; it needs a UNIQUE index, which
-- requires disambiguating the ROLLUP NULL `what` from real values (see KNOWN_ISSUES).
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

-- Reads always pin (corpus, service, severity) and the grain (category vs what); index accordingly.
CREATE INDEX report_summary_lookup_idx
  ON report_summary (corpus_id, service_id, severity, what_is_total, category);
