-- Drop the passkey (WebAuthn) credential store (credentials first: it references the users table).
drop table webauthn_credentials;
drop table webauthn_users;
