-- Enforce one worker_metadata row per (name, service_id) so the dispatcher can record dispatch and
-- return events with a race-free upsert (KNOWN_ISSUES D-2). Two pre-existing bugs motivate this:
--   1. record_received did find-then-update and silently dropped the count when the row did not yet
--      exist (the sink's metadata write can outrun the ventilator's insert for the same worker);
--   2. with no uniqueness, concurrent inserts could create duplicate rows, after which find_by_name's
--      get_result (expects exactly one) breaks.
--
-- First collapse any duplicates the pre-upsert insert race may already have created. worker_metadata
-- is operational/observability data (dispatch/return tallies, last-seen times), so keeping the most
-- recent row per worker and dropping the rest is an acceptable one-time reconciliation.
DELETE FROM worker_metadata a
  USING worker_metadata b
  WHERE a.id < b.id AND a.name = b.name AND a.service_id = b.service_id;

ALTER TABLE worker_metadata
  ADD CONSTRAINT worker_metadata_name_service_key UNIQUE (name, service_id);
