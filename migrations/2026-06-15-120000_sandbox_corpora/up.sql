-- Sandbox corpora (Arm 5): a sandbox is a first-class corpus carved from a PARENT corpus by a
-- message-condition filter. Two nullable columns make a `corpora` row a sandbox:
--   * parent_corpus_id — the corpus it was carved from (NULL for ordinary corpora).
--   * selection        — the filter predicate it was built from: {service, severity, category, what}.
-- The selection IS the provenance ("why these entries": the predicate applied over the parent), so no
-- per-task origin link is needed (owner decision 2026-06-15). No FK on parent_corpus_id — the schema's
-- prevailing convention is integer ids without FKs (only historical_tasks.task_id has one); referential
-- FKs are a separate arm (Arm 3).
ALTER TABLE corpora ADD COLUMN parent_corpus_id INT;
ALTER TABLE corpora ADD COLUMN selection JSONB;
