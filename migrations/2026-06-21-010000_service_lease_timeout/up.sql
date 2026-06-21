-- Per-service lease / visibility timeout override (D-17 / HANDOFF R1).
--
-- NULL (the default) means "use the global `dispatcher.lease_timeout_seconds`", so this is a purely
-- additive, backward-compatible column. A non-NULL value lets one dispatcher serve fast and slow
-- worker classes correctly at the same time: latexml-oxide wants a short lease (~240 s, just above
-- its 180 s per-document timeout) for prompt dead-worker recovery, while the legacy Perl LaTeXML
-- worker needs a long lease (~2760 s, above its 45-min budget) to avoid false reaps mid-conversion.
-- The dispatcher captures the effective value at dispatch time, so changing it never affects an
-- already-leased task.
ALTER TABLE services ADD COLUMN lease_timeout_seconds INTEGER;
