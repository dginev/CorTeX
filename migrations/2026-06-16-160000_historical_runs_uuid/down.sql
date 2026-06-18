-- Drop the external UUIDv7 handle (the UNIQUE constraint drops with the column).
ALTER TABLE historical_runs DROP COLUMN public_id;
