-- Narrow tasks.entry back to varchar(200). NB: this ERRORS if any entry now exceeds 200 chars — the
-- old cap cannot be losslessly re-imposed once long paths exist. Reversible only while all entries
-- still fit (e.g. an immediate rollback before any long entry is stored).
ALTER TABLE tasks ALTER COLUMN entry TYPE varchar(200);
