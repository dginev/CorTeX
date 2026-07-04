-- Widen the log_* `category` column from varchar(50) to varchar(100).
--
-- Motivation: the 50-char cap on the message taxonomy's middle key bit us in
-- the loaded_file family (long binding/package identifiers) and truncated
-- categories are useless as groupable keys. 100 chars holds every observed
-- producer with headroom.
--
-- Cost: increasing a varchar length limit is a CATALOG-ONLY change in
-- PostgreSQL (>= 9.2) — no table rewrite, no reindex — so this is instant
-- even on the 500M+ row log_infos, and the (category, what, task_id) btree
-- is untouched.
--
-- All five severity tables are widened together because CorTeX's log parser
-- (helpers.rs) truncates every severity's `category` to the same bound;
-- widening only one would risk an over-50 `category` on a non-widened table
-- overflowing its column.
ALTER TABLE log_infos    ALTER COLUMN category TYPE varchar(100);
ALTER TABLE log_warnings ALTER COLUMN category TYPE varchar(100);
ALTER TABLE log_errors   ALTER COLUMN category TYPE varchar(100);
ALTER TABLE log_fatals   ALTER COLUMN category TYPE varchar(100);
ALTER TABLE log_invalids ALTER COLUMN category TYPE varchar(100);
