CREATE TABLE services (
  id SERIAL PRIMARY KEY,
  name varchar(200) NOT NULL,
  version real NOT NULL,
  inputformat varchar(20) NOT NULL,
  outputformat varchar(20) NOT NULL,
  inputconverter varchar(200),
  complex boolean NOT NULL,
  UNIQUE(name,version)
);

create index servicenameidx on services(name);

INSERT INTO services (name, version, inputformat,outputformat,complex)
               values('init',0.1, 'tex','tex', true);

INSERT INTO services (name, version, inputformat,outputformat,complex)
               values('import',0.1, 'tex','tex', true);
