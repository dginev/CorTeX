-- Drop the tasks -> corpora/services foreign keys (the orphan-sweep is not reversible — those rows
-- were already dead data).
ALTER TABLE tasks DROP CONSTRAINT tasks_service_id_fkey;
ALTER TABLE tasks DROP CONSTRAINT tasks_corpus_id_fkey;
