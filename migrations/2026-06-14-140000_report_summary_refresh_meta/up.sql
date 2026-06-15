-- Track when the `report_summary` materialized view was last refreshed, so report pages can show the
-- true freshness of the data (the matview's age), not the per-request render time. Postgres does not
-- record a matview's refresh time, so we keep a single-row meta table updated by
-- `refresh_report_summary`.
CREATE TABLE IF NOT EXISTS report_summary_meta (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  refreshed_at timestamptz NOT NULL DEFAULT now()
);
INSERT INTO report_summary_meta (singleton) VALUES (true) ON CONFLICT (singleton) DO NOTHING;
