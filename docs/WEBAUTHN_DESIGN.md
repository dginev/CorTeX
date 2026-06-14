# Passkey (WebAuthn) sign-in for CorTeX

Owner directive (2026-06-14): *"a modern local best practice for authentication without external
reliance — as convenient as OAuth but local"*, and *"first introduce the webauthn-rs approach"*. This
is the AuthN arm settled in [`AAA_DESIGN.md`](AAA_DESIGN.md) §6: **passkeys**, with the CorTeX server
itself as the relying party. No IdP, no per-deployment OAuth app, nothing external in the auth path;
the server stores **only public keys**.

## Why passkeys (recap)

- **Local, no external dependency** — the relying party is this server. Nothing to register anywhere.
- **As convenient as OAuth** — biometric / security-key tap, passwordless.
- **More robust than what we have** — no shared secret at rest to leak/hash/rotate; phishing-resistant.
- Crate: [`webauthn-rs`](https://github.com/kanidm/webauthn-rs) `0.5` (powers Kanidm). Builds here
  against the system `openssl` (present).

## How it layers on what exists (nothing thrown away)

- The **admin token** (`auth.rerun_tokens`, set by `cortex set-admin-token`) stays as the
  **bootstrap / break-glass** credential **and** the **agent/API** credential (machines can't do
  biometrics → they keep `X-Cortex-Token`).
- **Passkeys** become the convenient **day-to-day human** sign-in.
- The **`audit_log` is auth-agnostic** (actor is a string), so it records the actor identically
  whether a session was established by token or by passkey.
- Passkeys never block the token path: a disabled/misconfigured relying party degrades to "token
  only" (logged, never a panic).

## Data model (landed)

Migration `2026-06-14-100000_create_webauthn_credentials` + `models::webauthn`:

- **`webauthn_users`** `(owner PK, handle uuid unique, created_at)` — one WebAuthn user per admin
  `owner` (the same string the audit log + tokens use). `handle` is the stable per-user id WebAuthn
  binds credentials to. `WebauthnUser::ensure(owner)` get-or-creates it (idempotent).
- **`webauthn_credentials`** `(id, owner FK→users ON DELETE CASCADE, label, credential jsonb,
  created_at, last_used)` — the enrolled passkeys (a person may have several: laptop, phone, key).
  `credential` is a serialized `webauthn_rs::prelude::Passkey` (**public** key + signature counter);
  stored as opaque JSON so the persistence layer doesn't depend on the WebAuthn crate.
  `WebauthnCredential::{store, for_owner, update_after_use, touch}`.

## Config (landed)

`config.WebauthnConfig` → `[webauthn]` in `cortex.toml` (persisted by `cortex init`): `enabled`
(default **false**), `rp_id` (registrable domain, host only — `localhost` dev / `corpora.latexml.rs`
deploy), `rp_origin` (full https origin). `frontend::webauthn::build_state` builds the relying-party
instance or returns `None` (logged) when disabled/invalid. Surfaced read-only on the Settings page +
`/api/config`.

## Sessions (the load-bearing piece — **landed**)

Previously `AdminSession` was a cookie that **carried the plaintext token**, re-validated each
request. A passkey user has **no token**, so passkeys forced a real session model. Landed
(migration `2026-06-14-110000_create_sessions` + `models::session`):

> **Server-side session store.** `sessions (id PK = random opaque id, owner, method, created_at,
> expires_at)`; the cookie carries only the random 48-char session id (unguessable bearer, looked up
> server-side — no `secret_key` dependency). `AdminSession::from_request` resolves cookie → session
> row → owner (absolute 7-day expiry; `Session::resolve_owner`). **Both** sign-in paths open a row:
> `Session::open(owner, "token")` today, `"passkey"` next. `/admin/logout` calls `Session::revoke`
> (real revocation — the id is dead even if the cookie lingers). `Session::{revoke_all_for, active,
> prune_expired}` support sign-out-everywhere + an "active sessions" view + housekeeping (pruned on
> sign-in). The audit fairing's actor resolution was split into a sync `actor_carriers` (no lookup,
> on the reactor) + `resolve_carriers` (token via config, cookie via the `sessions` table, run inside
> the existing `spawn_blocking` so the DB lookup stays off the reactor).

Chosen over Rocket *private* cookies (signed+encrypted owner+expiry): no `secret_key` config
dependency, real server-side revocation, and a session list for accounting — a better fit for the
robustness + accounting mandate. Backward-compatible with `/admin/login` (token sign-in → session →
gated screens). `tests/admin_test.rs` additionally asserts the cookie value is **not** the raw token.

## Ceremonies (next increments)

Ceremony state (`PasskeyRegistration` / `PasskeyAuthentication`) lives between the begin/finish
requests in a short-lived **in-memory** store (managed `Mutex<HashMap<Uuid, (state, expiry)>>` keyed
by a random id in a cookie) — no `danger-allow-state-serialisation` feature needed.

- **Enroll** (a signed-in admin registers a passkey — bootstrap is: sign in with the token, then
  enroll): `POST /admin/passkeys/register/begin` → `start_passkey_registration(handle, owner, owner,
  exclude=existing cred ids)` → challenge JSON to the browser; the browser calls
  `navigator.credentials.create()`; `POST …/register/finish` → `finish_passkey_registration` →
  `WebauthnCredential::store`.
- **Sign in** (owner-first, simplest for admins): `/admin/login` offers "Sign in with a passkey" →
  `POST /admin/passkeys/auth/begin?owner=` → `start_passkey_authentication(for_owner)` → challenge;
  `navigator.credentials.get()`; `POST …/auth/finish` → `finish_passkey_authentication` → on success
  create a session (above) + `update_after_use`/`touch` the credential.
- **Manage**: an admin "Your passkeys" list on `/admin` (label, created, last-used, remove).

## Bootstrap & recovery (honest)

- **Bootstrap**: the first admin signs in with the **token**, then enrolls a passkey. The token is
  the break-glass credential thereafter.
- **Lost device**: enroll multiple passkeys, and the admin token remains the fallback. (No email/SMS
  recovery — that would reintroduce external dependency.)

## Browser JS

A small **vanilla** `navigator.credentials` snippet (base64url encode/decode of the challenge +
credential) on the enroll + login pages. Not a framework — consistent with the "light JS only" rule.

## Bot/abuse protection is **not** in the framework — Anubis at the deploy edge

Owner (2026-06-14): the prototype's `captcha_secret` is **removed**; bot/scraper protection for the
public read surface is handled by an **[Anubis](https://github.com/TecharoHQ/anubis) reverse proxy**
in front of the deployment — a **one-time deployment measure for the `corpora.latexml.rs` preview**,
**not** a framework feature (it isn't needed for CorTeX in general). See
[`DEPLOYMENT.md`](DEPLOYMENT.md). CorTeX adds no in-code captcha/bot guard.

## Increment status

- ✅ **Foundation**: `webauthn-rs` dep; `[webauthn]` config; `webauthn_users` + `webauthn_credentials`
  migration + models; `frontend::webauthn::build_state` (relying-party instance) + unit tests; captcha
  removed.
- ✅ **Sessions**: `sessions` table + `models::session`; `AdminSession` now resolves a server-side
  session id (token sign-in opens a session, sign-out revokes it); audit-fairing actor resolution
  split for off-reactor cookie lookup. (See "Sessions" above.)
- ⏭️ **Next**: the enroll + sign-in ceremonies (`start/finish_passkey_*`) + the in-memory ceremony
  store + vanilla `navigator.credentials` JS + the "Your passkeys" management view; passkey sign-in
  opens a `Session::open(owner, "passkey")`.
- ⏭️ **Then** (separately tracked, [`AAA_DESIGN.md`](AAA_DESIGN.md)): switch the human write/confirm
  dialogs from token-entry to the session cookie + redirect anonymous write attempts to `/admin/login`.
