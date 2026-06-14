-- Server-side admin sessions (docs/WEBAUTHN_DESIGN.md). The browser cookie now carries only a random
-- opaque session id; the owner + expiry live here, server-side. This unifies the two human sign-in
-- paths — the admin **token** and a **passkey** both create a session row — so the cookie no longer
-- carries a credential (a forged/stolen cookie is a useless random id unless it matches a live row).
-- Sign-out DELETEs the row (real revocation); expired rows are pruned on the next sign-in.
create table sessions (
  -- the random opaque session id (the cookie value); an unguessable bearer looked up server-side.
  id varchar(64) PRIMARY KEY,
  -- the authenticated identity (the audit-log actor / token owner).
  owner varchar(200) NOT NULL,
  -- how the session was established: 'token' (admin token) or 'passkey' (WebAuthn).
  method varchar(20) NOT NULL DEFAULT 'token',
  created_at TIMESTAMP NOT NULL DEFAULT now(),
  -- absolute expiry (no per-request sliding write, for performance): re-authenticate after this.
  expires_at TIMESTAMP NOT NULL
);
create index sessions_owner_idx on sessions (owner);
create index sessions_expires_at_idx on sessions (expires_at);
