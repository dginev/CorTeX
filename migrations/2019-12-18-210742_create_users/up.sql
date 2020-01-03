-- up.sql
CREATE TABLE users (
  id SERIAL PRIMARY KEY,
  display TEXT NOT NULL DEFAULT 'anonymous',
  email varchar(200) NOT NULL,
  first_seen TIMESTAMP NOT NULL,
  last_seen TIMESTAMP NOT NULL,
  admin boolean
);
create index users_email_idx on users(email);

CREATE TABLE user_permissions(
  id SERIAL PRIMARY KEY,
  user_id INTEGER NOT NULL,
  corpus_id INTEGER,
  service_id INTEGER,
  owner boolean NOT NULL,
  developer boolean NOT NULL,
  viewer boolean NOT NULL
);
create index user_permissions_triple_idx on user_permissions(user_id, corpus_id, service_id);

CREATE TABLE user_actions(
  id SERIAL PRIMARY KEY,
  user_id INTEGER NOT NULL,
  corpus_id INTEGER,
  service_id INTEGER,
  action_counter INTEGER NOT NULL DEFAULT 0,
  last_timestamp TIMESTAMP NOT NULL,
  location TEXT NOT NULL,
  description TEXT NOT NULL
);
