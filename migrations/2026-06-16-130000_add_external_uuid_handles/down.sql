-- Drop the external UUIDv7 handles (the UNIQUE constraints drop with their columns).
ALTER TABLE services DROP COLUMN public_id;
ALTER TABLE corpora DROP COLUMN public_id;
