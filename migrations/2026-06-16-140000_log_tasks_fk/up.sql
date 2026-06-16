-- Arm 3: referential integrity for the five severity-partitioned log_* tables (the documented
-- orphan footgun). Today they have NO foreign key to `tasks`, so a raw `DELETE FROM tasks` (or a
-- corpus/service delete that removes tasks) orphans their rows — and orphans already accumulate in
-- practice. Add `task_id -> tasks(id) ON DELETE CASCADE` so the database itself guarantees
-- orphan-free deletes: raw deletes become safe, and `Corpus::destroy`/`Service::destroy` keep
-- working (now belt-and-suspenders rather than the sole guard).
--
-- Online-migration discipline for the large tables (log_infos alone is ~273M rows on the production
-- showcase DB): first sweep any pre-existing orphans (so validation can succeed), then
-- `ADD CONSTRAINT ... NOT VALID` (a brief lock that only checks new rows) followed by
-- `VALIDATE CONSTRAINT` (scans existing rows without blocking concurrent reads/writes).

-- 1) Sweep pre-existing orphans (rows whose task is already gone — unviewable dead data).
DELETE FROM log_infos    l WHERE NOT EXISTS (SELECT 1 FROM tasks t WHERE t.id = l.task_id);
DELETE FROM log_warnings l WHERE NOT EXISTS (SELECT 1 FROM tasks t WHERE t.id = l.task_id);
DELETE FROM log_errors   l WHERE NOT EXISTS (SELECT 1 FROM tasks t WHERE t.id = l.task_id);
DELETE FROM log_fatals   l WHERE NOT EXISTS (SELECT 1 FROM tasks t WHERE t.id = l.task_id);
DELETE FROM log_invalids l WHERE NOT EXISTS (SELECT 1 FROM tasks t WHERE t.id = l.task_id);

-- 2) Add the foreign keys NOT VALID (fast), then VALIDATE (concurrent-friendly scan).
ALTER TABLE log_infos    ADD CONSTRAINT log_infos_task_id_fkey    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE NOT VALID;
ALTER TABLE log_infos    VALIDATE CONSTRAINT log_infos_task_id_fkey;
ALTER TABLE log_warnings ADD CONSTRAINT log_warnings_task_id_fkey FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE NOT VALID;
ALTER TABLE log_warnings VALIDATE CONSTRAINT log_warnings_task_id_fkey;
ALTER TABLE log_errors   ADD CONSTRAINT log_errors_task_id_fkey   FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE NOT VALID;
ALTER TABLE log_errors   VALIDATE CONSTRAINT log_errors_task_id_fkey;
ALTER TABLE log_fatals   ADD CONSTRAINT log_fatals_task_id_fkey   FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE NOT VALID;
ALTER TABLE log_fatals   VALIDATE CONSTRAINT log_fatals_task_id_fkey;
ALTER TABLE log_invalids ADD CONSTRAINT log_invalids_task_id_fkey FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE NOT VALID;
ALTER TABLE log_invalids VALIDATE CONSTRAINT log_invalids_task_id_fkey;
