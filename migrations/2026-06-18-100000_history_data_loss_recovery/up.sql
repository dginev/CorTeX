-- Recover the arXMLiv (arXiv / tex_to_html) historical run data lost to a catastrophic
-- failure. Values are a SYNTHETIC APPROXIMATION reconstructed from the published figure
-- https://prodg.org/sigmathling22/arxmliv_history_2022.svg (7 annual builds, 2016-2022):
-- per-category pixel heights of the stacked bars, scaled so the 2022 build ~= 2,000,000
-- documents (consistent with the current corpus size). Idempotent across environments:
-- inserts only where the arXiv corpus + tex_to_html service exist (no-op elsewhere, e.g. CI).
INSERT INTO historical_runs
  (service_id, corpus_id, total, invalid, fatal, error, warning, no_problem, in_progress,
   start_time, end_time, owner, description)
SELECT s.id, c.id, v.total, 0, v.fatal, v.error, v.warning, v.no_problem, 0,
       v.start_time, v.end_time, 'recovery', 'reconstructed arXMLiv build — synthetic approximation from arxmliv_history_2022.svg (history_data_loss_recovery)'
FROM (VALUES
  (382514, 32787, 120219, 180328, 49180, TIMESTAMP '2016-08-01 00:00:00', TIMESTAMP '2017-08-01 00:00:00'),
  (1174864, 98361, 377049, 579235, 120219, TIMESTAMP '2017-08-01 00:00:00', TIMESTAMP '2018-08-01 00:00:00'),
  (1273224, 109290, 409836, 628415, 125683, TIMESTAMP '2018-08-01 00:00:00', TIMESTAMP '2019-08-01 00:00:00'),
  (1437159, 92896, 426230, 770492, 147541, TIMESTAMP '2019-08-01 00:00:00', TIMESTAMP '2020-08-01 00:00:00'),
  (1573770, 71038, 431694, 907104, 163934, TIMESTAMP '2020-08-01 00:00:00', TIMESTAMP '2021-08-01 00:00:00'),
  (1803278, 71038, 437158, 1060109, 234973, TIMESTAMP '2021-08-01 00:00:00', TIMESTAMP '2022-08-01 00:00:00'),
  (1999999, 49180, 459016, 1153005, 338798, TIMESTAMP '2022-08-01 00:00:00', TIMESTAMP '2022-09-01 00:00:00')
) AS v(total, fatal, error, warning, no_problem, start_time, end_time)
CROSS JOIN corpora  c
CROSS JOIN services s
WHERE c.name = 'arXiv' AND s.name = 'tex_to_html';
