-- Arm 3 (Phase 2b): complete the referential integrity by linking every task to its corpus and its
-- service. Combined with the log_* -> tasks FKs (migration …140000), this makes a raw
-- `DELETE FROM corpora` cascade corpora -> tasks -> log_* entirely in the database, so even a raw
-- delete is orphan-free. `Corpus::destroy` / `Service::destroy` remain the transactional, audited
-- path; these FKs are the structural backstop that the load-bearing "always delete through destroy"
-- caveat asked for.
--
-- Same online-migration discipline as …140000 (tasks is ~5.87M rows on the production showcase DB):
-- sweep any orphans first, then `ADD CONSTRAINT ... NOT VALID` (brief lock, new rows only) followed
-- by `VALIDATE CONSTRAINT` (scans existing rows without blocking concurrent reads/writes).

-- 1) Sweep tasks whose corpus or service no longer exists (dead, unviewable rows). With …140000 in
--    place these cascade to remove the tasks' log_* rows too.
DELETE FROM tasks t WHERE NOT EXISTS (SELECT 1 FROM corpora  c WHERE c.id = t.corpus_id);
DELETE FROM tasks t WHERE NOT EXISTS (SELECT 1 FROM services s WHERE s.id = t.service_id);

-- 2) Add the foreign keys NOT VALID (fast), then VALIDATE (concurrent-friendly scan).
ALTER TABLE tasks ADD CONSTRAINT tasks_corpus_id_fkey  FOREIGN KEY (corpus_id)  REFERENCES corpora(id)  ON DELETE CASCADE NOT VALID;
ALTER TABLE tasks VALIDATE CONSTRAINT tasks_corpus_id_fkey;
ALTER TABLE tasks ADD CONSTRAINT tasks_service_id_fkey FOREIGN KEY (service_id) REFERENCES services(id) ON DELETE CASCADE NOT VALID;
ALTER TABLE tasks VALIDATE CONSTRAINT tasks_service_id_fkey;
