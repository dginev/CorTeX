-- Revert to varchar(50). NOTE: shrinking DOES validate existing rows; rows
-- with >50-char categories must be trimmed first for this to succeed.
ALTER TABLE log_infos    ALTER COLUMN category TYPE varchar(50);
ALTER TABLE log_warnings ALTER COLUMN category TYPE varchar(50);
ALTER TABLE log_errors   ALTER COLUMN category TYPE varchar(50);
ALTER TABLE log_fatals   ALTER COLUMN category TYPE varchar(50);
ALTER TABLE log_invalids ALTER COLUMN category TYPE varchar(50);
