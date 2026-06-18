-- Revert the per-table autovacuum tuning to the PostgreSQL global defaults.
ALTER TABLE tasks RESET (
  autovacuum_vacuum_scale_factor, autovacuum_vacuum_threshold,
  autovacuum_analyze_scale_factor, autovacuum_analyze_threshold,
  autovacuum_vacuum_insert_scale_factor, autovacuum_vacuum_insert_threshold
);
ALTER TABLE log_infos RESET (
  autovacuum_vacuum_scale_factor, autovacuum_vacuum_threshold,
  autovacuum_analyze_scale_factor, autovacuum_analyze_threshold,
  autovacuum_vacuum_insert_scale_factor, autovacuum_vacuum_insert_threshold
);
ALTER TABLE log_warnings RESET (
  autovacuum_vacuum_scale_factor, autovacuum_vacuum_threshold,
  autovacuum_analyze_scale_factor, autovacuum_analyze_threshold,
  autovacuum_vacuum_insert_scale_factor, autovacuum_vacuum_insert_threshold
);
ALTER TABLE log_errors RESET (
  autovacuum_vacuum_scale_factor, autovacuum_vacuum_threshold,
  autovacuum_analyze_scale_factor, autovacuum_analyze_threshold,
  autovacuum_vacuum_insert_scale_factor, autovacuum_vacuum_insert_threshold
);
ALTER TABLE log_fatals RESET (
  autovacuum_vacuum_scale_factor, autovacuum_vacuum_threshold,
  autovacuum_analyze_scale_factor, autovacuum_analyze_threshold,
  autovacuum_vacuum_insert_scale_factor, autovacuum_vacuum_insert_threshold
);
ALTER TABLE log_invalids RESET (
  autovacuum_vacuum_scale_factor, autovacuum_vacuum_threshold,
  autovacuum_analyze_scale_factor, autovacuum_analyze_threshold,
  autovacuum_vacuum_insert_scale_factor, autovacuum_vacuum_insert_threshold
);
ALTER TABLE historical_tasks RESET (
  autovacuum_vacuum_scale_factor, autovacuum_vacuum_threshold,
  autovacuum_analyze_scale_factor, autovacuum_analyze_threshold,
  autovacuum_vacuum_insert_scale_factor, autovacuum_vacuum_insert_threshold
);
