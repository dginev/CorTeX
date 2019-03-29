-- up.sql
CREATE TABLE history (
  id SERIAL PRIMARY KEY,
  service_id INTEGER NOT NULL,
  corpus_id INTEGER NOT NULL,
  total INTEGER NOT NULL DEFAULT 0,
  invalid INTEGER NOT NULL DEFAULT 0,
  fatal  INTEGER NOT NULL DEFAULT 0,
  error  INTEGER NOT NULL DEFAULT 0,
  warning  INTEGER NOT NULL DEFAULT 0,
  no_problem  INTEGER NOT NULL DEFAULT 0,
  log_info  INTEGER NOT NULL DEFAULT 0,
  log_warning  INTEGER NOT NULL DEFAULT 0,
  log_error  INTEGER NOT NULL DEFAULT 0,
  log_fatal  INTEGER NOT NULL DEFAULT 0,
  start_time TIMESTAMP NOT NULL DEFAULT NOW(),
  end_time   TIMESTAMP,
  owner varchar(200) NOT NULL,
  description TEXT NOT NULL DEFAULT ''
);
create index history_service_idx on history(service_id);
create index history_corpus_idx on history(corpus_id);
