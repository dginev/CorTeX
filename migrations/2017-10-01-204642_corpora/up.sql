CREATE TABLE corpora (
  id SERIAL PRIMARY KEY,
  path varchar(200) NOT NULL,
  name varchar(200) NOT NULL,
  complex boolean NOT NULL
);

create unique index corpusnameidx on corpora(name);
