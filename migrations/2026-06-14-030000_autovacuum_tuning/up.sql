-- Best-practice autovacuum / autoanalyze for CorTeX's high-churn + append-heavy tables.
--
-- Why per-table: PostgreSQL's global defaults (vacuum/analyze scale_factor 0.2/0.1) are far too lazy
-- for tables that grow into the tens/hundreds of millions of rows — they let dead-tuple bloat and
-- stale planner stats accumulate, which is exactly what slows the report queries + the rollup
-- refresh. This bakes the proven production tuning (previously a manual INSTALL.md §8 step) into
-- every install, applies it *consistently* (the live DB missed `log_invalids` and `historical_tasks`),
-- and adds PostgreSQL 13+ **insert-based** autovacuum so the append-only `log_*` tables — which see
-- almost no UPDATE/DELETE and therefore would otherwise never be autovacuumed — still get vacuumed
-- to freeze tuples (avoiding a wraparound emergency-vacuum stall) and to maintain the visibility map
-- (enabling index-only scans + cheaper future vacuums).
--
-- scale_factor 0.0002 + threshold 50 ⇒ vacuum after max(50, 0.02% of the table) dead/inserted rows:
-- aggressive but size-relative, so it stays sane from a 1k-row corpus to a 100M-row arXiv run.

ALTER TABLE tasks SET (
  autovacuum_enabled = true,
  autovacuum_vacuum_scale_factor = 0.0002, autovacuum_vacuum_threshold = 50,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50,
  autovacuum_vacuum_insert_scale_factor = 0.0005, autovacuum_vacuum_insert_threshold = 1000
);
ALTER TABLE log_infos SET (
  autovacuum_enabled = true,
  autovacuum_vacuum_scale_factor = 0.0002, autovacuum_vacuum_threshold = 50,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50,
  autovacuum_vacuum_insert_scale_factor = 0.0005, autovacuum_vacuum_insert_threshold = 1000
);
ALTER TABLE log_warnings SET (
  autovacuum_enabled = true,
  autovacuum_vacuum_scale_factor = 0.0002, autovacuum_vacuum_threshold = 50,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50,
  autovacuum_vacuum_insert_scale_factor = 0.0005, autovacuum_vacuum_insert_threshold = 1000
);
ALTER TABLE log_errors SET (
  autovacuum_enabled = true,
  autovacuum_vacuum_scale_factor = 0.0002, autovacuum_vacuum_threshold = 50,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50,
  autovacuum_vacuum_insert_scale_factor = 0.0005, autovacuum_vacuum_insert_threshold = 1000
);
ALTER TABLE log_fatals SET (
  autovacuum_enabled = true,
  autovacuum_vacuum_scale_factor = 0.0002, autovacuum_vacuum_threshold = 50,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50,
  autovacuum_vacuum_insert_scale_factor = 0.0005, autovacuum_vacuum_insert_threshold = 1000
);
ALTER TABLE log_invalids SET (
  autovacuum_enabled = true,
  autovacuum_vacuum_scale_factor = 0.0002, autovacuum_vacuum_threshold = 50,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50,
  autovacuum_vacuum_insert_scale_factor = 0.0005, autovacuum_vacuum_insert_threshold = 1000
);
ALTER TABLE historical_tasks SET (
  autovacuum_enabled = true,
  autovacuum_vacuum_scale_factor = 0.0002, autovacuum_vacuum_threshold = 50,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50,
  autovacuum_vacuum_insert_scale_factor = 0.0005, autovacuum_vacuum_insert_threshold = 1000
);
