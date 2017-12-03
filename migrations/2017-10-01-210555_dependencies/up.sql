CREATE TABLE dependencies (
  master INTEGER NOT NULL,
  foundation INTEGER NOT NULL,
  PRIMARY KEY (master, foundation)
);
create index masteridx on dependencies(master);
create index foundationidx on dependencies(foundation);
