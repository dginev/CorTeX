CREATE TABLE worker_metadata
(
  id SERIAL PRIMARY KEY,
  service_id INTEGER NOT NULL,
  last_dispatched_task_id BIGINT NOT NULL,
  last_returned_task_id BIGINT,
  total_dispatched INTEGER NOT NULL DEFAULT 0,
  total_returned INTEGER NOT NULL DEFAULT 0,
  first_seen TIMESTAMP NOT NULL,
  session_seen TIMESTAMP,
  time_last_dispatch TIMESTAMP NOT NULL,
  time_last_return TIMESTAMP,
  name varchar(200) NOT NULL
);
create unique index worker_id_idx on worker_metadata(name, service_id);
create index worker_service_idx on worker_metadata(service_id);
