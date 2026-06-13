-- Drop the (name, service_id) uniqueness. (The duplicate rows collapsed by `up.sql` are not
-- restored — they were redundant operational metadata.)
ALTER TABLE worker_metadata DROP CONSTRAINT IF EXISTS worker_metadata_name_service_key;
