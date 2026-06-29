-- Revert the log_* `what` column to varchar(50).
--
-- Decreasing a varchar length limit DOES rewrite the table and FAILS if any existing value exceeds
-- the new limit, so first clamp any longer values (the widened footprints) back to 50 chars. This
-- is lossy for those rows by necessity -- varchar(50) cannot hold a 200-char footprint -- and the
-- clamp is what makes the down migration succeed instead of erroring on the first over-50 row.
UPDATE log_infos    SET what = left(what, 50) WHERE length(what) > 50;
UPDATE log_warnings SET what = left(what, 50) WHERE length(what) > 50;
UPDATE log_errors   SET what = left(what, 50) WHERE length(what) > 50;
UPDATE log_fatals   SET what = left(what, 50) WHERE length(what) > 50;
UPDATE log_invalids SET what = left(what, 50) WHERE length(what) > 50;

ALTER TABLE log_infos    ALTER COLUMN what TYPE varchar(50);
ALTER TABLE log_warnings ALTER COLUMN what TYPE varchar(50);
ALTER TABLE log_errors   ALTER COLUMN what TYPE varchar(50);
ALTER TABLE log_fatals   ALTER COLUMN what TYPE varchar(50);
ALTER TABLE log_invalids ALTER COLUMN what TYPE varchar(50);
