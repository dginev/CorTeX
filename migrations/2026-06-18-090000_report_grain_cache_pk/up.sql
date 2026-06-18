-- Give the report_grain_cache a surrogate primary key so Diesel can manage it (be in src/schema.rs
-- and serve the typed-DSL readers in src/backend/rollup.rs), instead of being excluded from schema
-- generation. The table's natural identity is the grain tuple (corpus_id, service_id, severity,
-- category_is_total, what_is_total, category, what) — but category/what are nullable (NULL on the
-- ROLLUP total rows), and a PRIMARY KEY cannot contain nullable columns, so a surrogate `id` is the
-- only way to key it. The existing UNIQUE INDEX (report_grain_cache_uniq, NULLS NOT DISTINCT) still
-- backs populate_scope's ON CONFLICT upsert; `id` is identity-only and never referenced by queries.
ALTER TABLE report_grain_cache ADD COLUMN id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY;
