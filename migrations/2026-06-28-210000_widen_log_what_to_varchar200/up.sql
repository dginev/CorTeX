-- Widen the log_* `what` column from varchar(50) to varchar(200).
--
-- Motivation: the math-parser diagnostics (`ambiguous_math` / `unparsed_math`) write a structural
-- token-type FOOTPRINT as the `what` value, used as the groupable frequency key for math-syntax
-- research ("most frequent ambiguous / unparsed shapes" -> strategic grammar-coverage work). 50
-- chars (~7 token types) buckets too coarsely; 200 holds ~25 types. The full token stream remains
-- in the message `details` (varchar(2000)), so `what` stays a bounded shape signature, not the dump.
--
-- Cost: increasing a varchar length limit is a CATALOG-ONLY change in PostgreSQL (>= 9.2) -- no
-- table rewrite, no reindex -- so this is instant even on the 500M+ row log_infos, and the
-- (category, what, task_id) btree is untouched. The footprint producer caps values at 200 chars, so
-- the btree per-row size limit (~2704 bytes) is never approached.
--
-- All five severity tables are widened together because CorTeX's log parser (helpers.rs) truncates
-- every severity's `what` to the same bound; widening only one would risk an over-50 `what` on a
-- non-widened table overflowing its column.
ALTER TABLE log_infos    ALTER COLUMN what TYPE varchar(200);
ALTER TABLE log_warnings ALTER COLUMN what TYPE varchar(200);
ALTER TABLE log_errors   ALTER COLUMN what TYPE varchar(200);
ALTER TABLE log_fatals   ALTER COLUMN what TYPE varchar(200);
ALTER TABLE log_invalids ALTER COLUMN what TYPE varchar(200);
