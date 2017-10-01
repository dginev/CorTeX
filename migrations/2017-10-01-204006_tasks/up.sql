-- Your SQL goes here
CREATE TABLE tasks (
  taskid BIGSERIAL PRIMARY KEY,
  serviceid INTEGER NOT NULL,
  corpusid INTEGER NOT NULL,
  entry char(200) NOT NULL,
  status INTEGER NOT NULL,
  UNIQUE (entry, serviceid, corpusid)
);
