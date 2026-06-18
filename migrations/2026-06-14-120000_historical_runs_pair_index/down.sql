-- Restore the single-column corpus_id index and drop the composite.
CREATE INDEX IF NOT EXISTS historical_runs_corpus_idx ON historical_runs (corpus_id);
DROP INDEX IF EXISTS historical_runs_corpus_service_start_idx;
