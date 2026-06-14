# Deploying CorTeX (and the `corpora.latexml.rs` preview)

CorTeX is a self-contained app: a Postgres database, the ZeroMQ dispatcher, and the Rocket frontend
(see [`INSTALL.md`](../INSTALL.md) for the build + `cortex init`). This note covers the **deployment
edge** decisions for the first public preview — things that are intentionally **not framework code**.

## First preview: `https://corpora.latexml.rs`

A read-only public view of the ar5iv dashboards + document previews. The public/read surface is
served openly; the **write / admin** surfaces stay gated (signed-in admins only) and the **agent
API** stays token-gated. The deployment is reverse-proxied (Caddy) over Tailscale to the frontend on
the `cortex` host.

## Bot / abuse protection: **Anubis at the edge, not in the framework**

Owner decision (2026-06-14): the prototype's `captcha_secret` read-only guard is **removed from the
codebase**. CorTeX does **not** ship a captcha or bot-challenge feature — it isn't needed for CorTeX
in general, and baking it into the framework would be the wrong layer.

Instead, **guard the entire preview deployment behind [Anubis](https://github.com/TecharoHQ/anubis)**
— a lightweight proof-of-work reverse proxy that shields the public surface from scraper/AI-bot load
— as a **one-time deployment measure** for `corpora.latexml.rs`. It sits in front of the reverse
proxy; CorTeX is unaware of it.

- Place Anubis in front of the public origin (ahead of / within the Caddy chain) per its docs.
- Scope it to the **public read** routes; it must not challenge the **agent API** (`/api/*`, used by
  machines that can't solve a PoW challenge) — keep those reachable directly (e.g. internal/Tailscale
  only, or an Anubis allow-rule for `/api`).
- It is **per-deployment ops config**, versioned with the deployment (not this repo).

## Human sign-in: passkeys (WebAuthn)

The admin UI uses passkeys for human sign-in (see [`WEBAUTHN_DESIGN.md`](WEBAUTHN_DESIGN.md)). For the
preview, set in `cortex.toml`:

```toml
[webauthn]
enabled = true
rp_id = "corpora.latexml.rs"
rp_origin = "https://corpora.latexml.rs"
```

WebAuthn requires HTTPS in production (satisfied by the Caddy TLS termination). The **admin token**
(`cortex set-admin-token`) remains the bootstrap / break-glass + agent credential.

## Checklist (preview)

1. `cortex init` (migrate + scaffold `cortex.toml`); tune Postgres (`cortex tune-db`).
2. `cortex set-admin-token --generate --owner <you>` → sign in once with the token, enroll a passkey.
3. Set `[webauthn]` to the deploy domain (above); restart the frontend.
4. Put **Anubis** in front of the public origin; allow `/api/*` through for agents.
5. Confirm: public read views load; `/admin` requires sign-in; `/api/*` works with a token.
