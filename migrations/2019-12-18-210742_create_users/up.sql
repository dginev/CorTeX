-- up.sql
CREATE TABLE users (
  id SERIAL PRIMARY KEY,
  display TEXT NOT NULL DEFAULT 'anonymous',
  email varchar(200) NOT NULL,
  admin boolean
);
create index users_email_idx on users(email);

CREATE TABLE user_permissions(
  id SERIAL PRIMARY KEY,
  user_id INTEGER NOT NULL,
  corpus_id INTEGER NOT NULL,
  service_id INTEGER NOT NULL,
  owner boolean,
  runner boolean,
  viewer boolean
);
create index user_permissions_triple_idx on user_permissions(user_id, corpus_id, service_id);