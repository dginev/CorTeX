-- Drop the log_* -> tasks foreign keys (the orphan-sweep is not reversible — those rows were
-- already dead data, so there is nothing meaningful to restore).
ALTER TABLE log_infos    DROP CONSTRAINT log_infos_task_id_fkey;
ALTER TABLE log_warnings DROP CONSTRAINT log_warnings_task_id_fkey;
ALTER TABLE log_errors   DROP CONSTRAINT log_errors_task_id_fkey;
ALTER TABLE log_fatals   DROP CONSTRAINT log_fatals_task_id_fkey;
ALTER TABLE log_invalids DROP CONSTRAINT log_invalids_task_id_fkey;
