-- Passkey (WebAuthn) sign-in (docs/archive/WEBAUTHN_DESIGN.md). The relying party is the CorTeX server
-- itself — no external IdP. These tables hold the per-owner WebAuthn user handle and the enrolled
-- public-key credentials (passkeys). **Only PUBLIC keys are stored** — there is no secret here to
-- leak, hash or rotate (strictly more robust than the plaintext token posture).

-- One WebAuthn "user" per admin identity (the same `owner` string the audit log + tokens use).
create table webauthn_users (
  -- the human identity (matches the audit-log actor / token owner).
  owner varchar(200) PRIMARY KEY,
  -- the stable per-user handle WebAuthn binds credentials to (random uuid, never reused).
  handle uuid NOT NULL UNIQUE,
  created_at TIMESTAMP NOT NULL DEFAULT now()
);

-- The enrolled passkeys for each owner (a person may register several — phone, laptop, security key).
create table webauthn_credentials (
  id BIGSERIAL PRIMARY KEY,
  -- the owner this passkey authenticates as; cascades so removing a user removes their credentials.
  owner varchar(200) NOT NULL REFERENCES webauthn_users(owner) ON DELETE CASCADE,
  -- a human label for the authenticator (e.g. 'MacBook Touch ID', 'YubiKey 5').
  label varchar(200) NOT NULL DEFAULT '',
  -- the serialized webauthn-rs `Passkey` (public key + signature counter + metadata). PUBLIC only.
  credential JSONB NOT NULL,
  created_at TIMESTAMP NOT NULL DEFAULT now(),
  -- last successful authentication with this credential (so the admin can spot stale keys).
  last_used TIMESTAMP
);
create index webauthn_credentials_owner_idx on webauthn_credentials (owner);
