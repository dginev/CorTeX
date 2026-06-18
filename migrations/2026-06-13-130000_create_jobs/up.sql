-- Background jobs: one row per long-running administrative operation.
CREATE TABLE jobs (
  id               BIGSERIAL PRIMARY KEY,
  uuid             UUID NOT NULL UNIQUE DEFAULT gen_random_uuid(),
  kind             VARCHAR(50)  NOT NULL,
  status           VARCHAR(20)  NOT NULL DEFAULT 'queued',
  progress_current INTEGER      NOT NULL DEFAULT 0,
  progress_total   INTEGER,
  message          TEXT         NOT NULL DEFAULT '',
  actor            VARCHAR(200) NOT NULL DEFAULT '',
  params           JSONB        NOT NULL DEFAULT '{}',
  result           JSONB,
  created_at       TIMESTAMP    NOT NULL DEFAULT NOW(),
  updated_at       TIMESTAMP    NOT NULL DEFAULT NOW()
);
CREATE INDEX jobs_status_idx ON jobs(status);
CREATE INDEX jobs_kind_idx ON jobs(kind);
