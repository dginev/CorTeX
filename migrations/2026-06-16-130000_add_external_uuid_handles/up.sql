-- Arm 3 (D8): external UUIDv7 handles for the small external-handle entities.
--
-- `corpora` and `services` gain a stable, externally-referenceable `public_id` that is independent
-- of their (mutable, human-facing) `name`. PostgreSQL 18's built-in `uuidv7()` generates a
-- time-ordered v7 as the column DEFAULT, so every insert from every surface (web / CLI / agent)
-- gets one for free -- no app-side generation, no PG-version skew. Existing rows are backfilled by
-- the same DEFAULT during ADD COLUMN (volatile default -> one distinct value per row).
--
-- Scope is deliberate: only the small external-handle tables get a handle. tasks / log_* (millions
-- of rows, never externally referenced) do NOT. `historical_runs` (its own external run handle)
-- follows in a later migration alongside run-by-handle routing.
--
-- Requires PostgreSQL 18+ for `uuidv7()` (the deployment floor; see INSTALL.md).

ALTER TABLE corpora ADD COLUMN public_id uuid NOT NULL DEFAULT uuidv7();
ALTER TABLE corpora ADD CONSTRAINT corpora_public_id_key UNIQUE (public_id);

ALTER TABLE services ADD COLUMN public_id uuid NOT NULL DEFAULT uuidv7();
ALTER TABLE services ADD CONSTRAINT services_public_id_key UNIQUE (public_id);
