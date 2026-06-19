-- Live "Latest dispatcher activity" stream (frontend): a unified, time-sorted tail of the most
-- recent fatal/error/warning conversion messages needs a shared ordering key across the three
-- severity-partitioned log_* tables, which have independent BIGSERIAL id sequences and (until now)
-- no timestamp. Add `recorded_at` so the feed can `UNION ALL … ORDER BY recorded_at DESC`.
--
-- ONLINE-SAFE on the large production tables: `ADD COLUMN … TIMESTAMPTZ` (nullable, no default) is a
-- catalog-only change — instant, no table rewrite, no lock-out (PG 11+). A separate `SET DEFAULT
-- now()` then auto-stamps NEW rows. The dispatcher's INSERTs list columns explicitly (diesel
-- Insertable), so they omit `recorded_at` and the default fills it — NO dispatcher/code change and NO
-- hot-path cost. Pre-existing rows stay NULL (the feed sorts them last); every new run streams by time.
ALTER TABLE log_fatals   ADD COLUMN recorded_at TIMESTAMPTZ;
ALTER TABLE log_fatals   ALTER COLUMN recorded_at SET DEFAULT now();
ALTER TABLE log_errors   ADD COLUMN recorded_at TIMESTAMPTZ;
ALTER TABLE log_errors   ALTER COLUMN recorded_at SET DEFAULT now();
ALTER TABLE log_warnings ADD COLUMN recorded_at TIMESTAMPTZ;
ALTER TABLE log_warnings ALTER COLUMN recorded_at SET DEFAULT now();
