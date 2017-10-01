CREATE TABLE logs_fatal (
  messageid BIGSERIAL PRIMARY KEY,
  taskid BIGINT NOT NULL,
  category char(50),
  what char(50),
  details varchar(2000)
);
CREATE TABLE logs_error (
  messageid BIGSERIAL PRIMARY KEY,
  taskid BIGINT NOT NULL,
  category char(50),
  what char(50),
  details varchar(2000)
);
CREATE TABLE logs_warning (
  messageid BIGSERIAL PRIMARY KEY,
  taskid BIGINT NOT NULL,
  category char(50),
  what char(50),
  details varchar(2000)
);
CREATE TABLE logs_invalid (
  messageid BIGSERIAL PRIMARY KEY,
  taskid BIGINT NOT NULL,
  category char(50),
  what char(50),
  details varchar(2000)
);

create index logs_fatal_taskid on logs_fatal(taskid);
create index logs_error_taskid on logs_error(taskid);
create index logs_warning_taskid on logs_warning(taskid);
create index logs_invalid_taskid on logs_invalid(taskid);

-- Note: to avoid a sequential scan on logs for all the report pages, the following indexes are crucial:
create index logs_fatal_index on logs_fatal(category,what,taskid);
create index logs_error_index on logs_error(category,what,taskid);
create index logs_warning_index on logs_warning(category,what,taskid);
create index logs_invalid_index on logs_invalid(category,what,taskid);
