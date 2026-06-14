-- Accounting pillar (AAA — docs/AAA_DESIGN.md): a persistent, queryable record of every admin action
-- with the **actor** who took it, so "who did what, when" is observable in the admin UI. This is the
-- piece the owner asked for ("observability of actions taken"). It is **auth-agnostic** — `actor` is
-- just whatever the auth layer resolved (today the admin token's `owner`), so the table survives any
-- future auth upgrade (per-admin tokens, local accounts, …) without a schema change.
create table audit_log (
  id BIGSERIAL PRIMARY KEY,
  -- the identity that acted: the signed-in admin / the API token's owner ("" if somehow unresolved).
  actor varchar(200) NOT NULL DEFAULT '',
  -- what was done, a stable verb, e.g. 'rerun', 'import_corpus', 'deactivate_service', 'reindex'.
  action varchar(100) NOT NULL,
  -- the resource acted on, e.g. 'corpus' or 'corpus/service' (free-form, may be empty).
  target varchar(512) NOT NULL DEFAULT '',
  -- the result, e.g. an HTTP status code or 'ok'/'denied' (free-form, may be empty).
  outcome varchar(48) NOT NULL DEFAULT '',
  -- optional extra context (a short params summary); kept compact, never secrets.
  details text NOT NULL DEFAULT '',
  -- when it happened (server clock; same convention as the `jobs` table's timestamps).
  at TIMESTAMP NOT NULL DEFAULT now()
);
-- The admin-UI view is "most recent first", optionally filtered to one actor.
create index audit_log_at_idx on audit_log (at desc);
create index audit_log_actor_idx on audit_log (actor);
