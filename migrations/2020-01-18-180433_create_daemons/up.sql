CREATE TABLE daemons (
  id SERIAL PRIMARY KEY,
  pid INTEGER NOT NULL,
  first_seen TIMESTAMP NOT NULL,
  last_seen TIMESTAMP NOT NULL,
  name varchar(200) NOT NULL
);
create unique index daemon_name_idx on daemons(name);
