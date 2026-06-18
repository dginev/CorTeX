-- Arm 3 (D8): external UUIDv7 handle for historical_runs — the third (and last) external-handle
-- entity, after corpora/services (migration …130000). Gives every conversion run a stable handle
-- independent of its (corpus, service, start_time) coordinates, so a specific run can be referenced
-- by a single opaque token. PostgreSQL 18's built-in uuidv7() is the column DEFAULT (app-side wiring
-- unnecessary); historical_runs is small (one row per run), so the ADD COLUMN rewrite is cheap.
--
-- Deliberately NOT a foreign-key target change: historical_runs keeps NO FK to corpora/services so
-- its tallies survive a corpus/service delete (the immutable-history rule); this only adds a handle.
ALTER TABLE historical_runs ADD COLUMN public_id uuid NOT NULL DEFAULT uuidv7();
ALTER TABLE historical_runs ADD CONSTRAINT historical_runs_public_id_key UNIQUE (public_id);
