CREATE TABLE log_infos (
  id BIGSERIAL PRIMARY KEY,
  task_id BIGINT NOT NULL,
  category varchar(50),
  what varchar(50),
  details varchar(2000)
);
CREATE TABLE log_warnings (
  id BIGSERIAL PRIMARY KEY,
  task_id BIGINT NOT NULL,
  category varchar(50),
  what varchar(50),
  details varchar(2000)
);
CREATE TABLE log_errors (
  id BIGSERIAL PRIMARY KEY,
  task_id BIGINT NOT NULL,
  category varchar(50),
  what varchar(50),
  details varchar(2000)
);
CREATE TABLE log_fatals (
  id BIGSERIAL PRIMARY KEY,
  task_id BIGINT NOT NULL,
  category varchar(50),
  what varchar(50),
  details varchar(2000)
);
CREATE TABLE log_invalids (
  id BIGSERIAL PRIMARY KEY,
  task_id BIGINT NOT NULL,
  category varchar(50),
  what varchar(50),
  details varchar(2000)
);

create index log_infos_task_id on log_infos(task_id);
create index log_warnings_task_id on log_warnings(task_id);
create index log_errors_task_id on log_errors(task_id);
create unique index log_fatals_task_id on log_fatals(task_id);
create unique index log_invalids_task_id on log_invalids(task_id);

-- Note: to avoid a sequential scan on log fors all the report pages, the following indexes are crucial:
create index log_infos_index on log_infos(category,what,task_id);
create index log_warnings_index on log_warnings(category,what,task_id);
create index log_errors_index on log_errors(category,what,task_id);
create index log_fatals_index on log_fatals(category,what,task_id);
create index log_invalids_index on log_invalids(category,what,task_id);
