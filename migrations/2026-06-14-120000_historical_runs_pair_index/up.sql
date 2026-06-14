-- Performance: the per-(corpus, service) run queries on the public hot path only had a single-column
-- corpus_id index. `HistoricalRun::find_by` (`WHERE corpus_id=? AND service_id=? ORDER BY start_time
-- DESC` — every runs / history / diff page) and `find_current` (`... AND end_time IS NULL` — every
-- report page) therefore index-scanned corpus_id, then filtered service_id + sorted start_time in
-- memory. A composite `(corpus_id, service_id, start_time)` keys both equality filters AND the
-- ordering, turning that into a direct ordered index scan (no in-memory sort), and also serves the
-- system-wide overview's corpus+service-filtered reads.
--
-- The old `historical_runs_corpus_idx (corpus_id)` is a strict prefix of this composite, so it is now
-- redundant for corpus_id lookups and is dropped — one fewer index to maintain on every run write
-- (rationalization). `historical_runs_service_idx (service_id)` is kept (service_id is not a prefix
-- of the composite, so it still serves any service_id-only filter).
CREATE INDEX IF NOT EXISTS historical_runs_corpus_service_start_idx
  ON historical_runs (corpus_id, service_id, start_time);
DROP INDEX IF EXISTS historical_runs_corpus_idx;
