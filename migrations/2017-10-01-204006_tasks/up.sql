-- Your SQL goes here
CREATE TABLE tasks (
  id BIGSERIAL PRIMARY KEY,
  serviceid INTEGER NOT NULL,
  corpusid INTEGER NOT NULL,
  status INTEGER NOT NULL,
  entry varchar(200) NOT NULL,
  UNIQUE (entry, serviceid, corpusid)
);
-- TECHNICAL DEBT: I want to express the status codes via status=$1 arguments, such as &TaskStatus::NoProblem.raw(),
--                 to avoid fragility if/when changing conventions.
create index entryidx on tasks(entry);
create index serviceidx on tasks(serviceid);
create index ok_index on tasks(status,serviceid,corpusid,id,entry) where status=-1;
create index warning_index on tasks(status,serviceid,corpusid,id,entry) where status=-2;
create index error_index on tasks(status,serviceid,corpusid,id,entry) where status=-3;
create index fatal_index on tasks(status,serviceid,corpusid,id,entry) where status=-4;
create index invalid_index on tasks(status,serviceid,corpusid,id,entry) where status=-5;
