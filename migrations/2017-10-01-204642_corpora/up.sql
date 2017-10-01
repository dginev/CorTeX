CREATE TABLE corpora (
  corpusid SERIAL PRIMARY KEY,
  path varchar(200) NOT NULL,
  name varchar(200) NOT NULL,
  complex boolean NOT NULL
);

create index corpusnameidx on corpora(name);
