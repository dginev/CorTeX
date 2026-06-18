#!/usr/bin/env bash
# recover_arxiv_history.sh — ONE-OFF, INSTALLATION-SPECIFIC data backfill.
#
# The corpora.latexml.rs deployment lost the pre-2025 conversion-run history of its
# `arXiv` / `tex_to_html` pair to a catastrophic failure. This script re-inserts a
# SYNTHETIC APPROXIMATION of that history, reconstructed from the published figure
# https://prodg.org/sigmathling22/arxmliv_history_2022.svg — 7 annual arXMLiv builds
# (2016-2022): the per-category stacked-bar pixel heights, scaled so the 2022 build is
# ~2,000,000 documents (consistent with the current corpus size). The category split
# (no_problem / warning / error / fatal) is preserved from the figure.
#
# This is deliberately NOT a Diesel migration: migrations run against every cortex DB
# that migrates, and this backfill is meaningful for exactly one installation. Run it
# by hand, only on the deployment that lost the data:
#
#   DATABASE_URL=postgres://… scripts/recover_arxiv_history.sh
#   # or: scripts/recover_arxiv_history.sh "postgres://…"
#
# Idempotent: re-running inserts nothing if the recovered rows are already present.
# It is also a no-op on any DB lacking an `arXiv` corpus + `tex_to_html` service.
set -euo pipefail

DB="${1:-${DATABASE_URL:-}}"
if [[ -z "$DB" ]]; then
  echo "error: set DATABASE_URL or pass the connection string as \$1" >&2
  exit 2
fi

DESC='reconstructed arXMLiv build — synthetic approximation from arxmliv_history_2022.svg (scripts/recover_arxiv_history.sh)'

psql "$DB" -v ON_ERROR_STOP=1 -v desc="$DESC" <<'SQL'
INSERT INTO historical_runs
  (service_id, corpus_id, total, invalid, fatal, error, warning, no_problem, in_progress,
   start_time, end_time, owner, description)
SELECT s.id, c.id, v.total, 0, v.fatal, v.error, v.warning, v.no_problem, 0,
       v.start_time, v.end_time, 'recovery', :'desc'
FROM (VALUES
  ( 382514,  32787,  120219,   180328,   49180, TIMESTAMP '2016-08-01 00:00:00', TIMESTAMP '2017-08-01 00:00:00'),
  (1174864,  98361,  377049,   579235,  120219, TIMESTAMP '2017-08-01 00:00:00', TIMESTAMP '2018-08-01 00:00:00'),
  (1273224, 109290,  409836,   628415,  125683, TIMESTAMP '2018-08-01 00:00:00', TIMESTAMP '2019-08-01 00:00:00'),
  (1437159,  92896,  426230,   770492,  147541, TIMESTAMP '2019-08-01 00:00:00', TIMESTAMP '2020-08-01 00:00:00'),
  (1573770,  71038,  431694,   907104,  163934, TIMESTAMP '2020-08-01 00:00:00', TIMESTAMP '2021-08-01 00:00:00'),
  (1803278,  71038,  437158,  1060109,  234973, TIMESTAMP '2021-08-01 00:00:00', TIMESTAMP '2022-08-01 00:00:00'),
  (1999999,  49180,  459016,  1153005,  338798, TIMESTAMP '2022-08-01 00:00:00', TIMESTAMP '2022-09-01 00:00:00')
) AS v(total, fatal, error, warning, no_problem, start_time, end_time)
CROSS JOIN corpora  c
CROSS JOIN services s
WHERE c.name = 'arXiv' AND s.name = 'tex_to_html'
  AND NOT EXISTS (
    SELECT 1 FROM historical_runs x
    WHERE x.corpus_id = c.id AND x.service_id = s.id AND x.description = :'desc'
  );
SQL

echo "recover_arxiv_history.sh: done."
