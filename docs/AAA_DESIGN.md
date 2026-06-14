# AAA for CorTeX — current state, 2026 target, and the decisions needed

Owner directive (2026-06-14): *"we want to productize the system, so we should have a modern 2026
best practice for AAA — authentication, authorization and accounting."* This supersedes the
lightweight token scheme the admin sign-in (`/admin`, commit `41d3834`) was built on; that sign-in is
a **working stopgap** to evolve, not the end state.

## 1. Where we are today (honest)

There is **one** credential type doing **two** jobs:

- `config().auth.rerun_tokens` — a `HashMap<token → owner>` of **plaintext** "password-like tokens"
  (`config.rs::AuthConfig`), set by **hand-editing** `cortex.toml` `[auth]` (or the legacy
  `config.json`). **There is no generation command, no hashing, no expiry, no rotation.**
- The same token gates **both**: (a) the **agent/machine** API (`Actor` guard → `X-Cortex-Token`
  header / `?token=`), and (b) now the **human** admin web sign-in (the `AdminSession` cookie, which
  just carries the plaintext token and re-validates it).

Gaps vs. "AAA best practice":
- **AuthN:** plaintext secrets at rest; no MFA; no password hashing; human + machine creds conflated;
  no account lifecycle; the session cookie carries the secret itself.
- **AuthZ:** all-or-nothing — any valid token can do *every* write. No roles, no least privilege.
- **Accounting:** partial — an `owner` actor is threaded into writes (and `historical_runs.owner`),
  and `tracing` events exist, but there is **no audit log** (who did what, when, to what, outcome).

`cortex init --set-admin-token <token>` (proposed) would *automate writing a plaintext token* — a
convenience over hand-editing, but it does not move us toward AAA (still plaintext, still dual-use,
still no roles/audit). Worth a small stopgap, but not the productized answer.

## 2. The 2026 target (the shape best practice points to)

Separate **humans** from **machines**, and split the three pillars:

### Authentication (who you are)
- **Humans:** either **OIDC/SSO** (delegate to the org's IdP — Keycloak / Auth0 / Entra / Google /
  GitHub; the IdP owns passwords + MFA + lockout), and/or **local accounts** (a `users` table;
  **argon2id**-hashed passwords; optional **WebAuthn passkeys / TOTP** MFA). **Sessions** become
  signed+encrypted cookies (Rocket *private* cookies, needs a configured `secret_key`) carrying a
  session id — **not** the secret itself; with idle/absolute expiry + sign-out revocation.
- **Machines/agents:** **API keys** that are **hashed at rest** (store only a hash; show the key
  once), **scoped** and **revocable**, with an id + owner + created/last-used. This replaces the
  dual-use plaintext token for the agent API (the OpenAPI `CortexToken` scheme stays, but keys become
  proper hashed credentials).

### Authorization (what you may do) — **stays uniform (owner: 2026-06-14)**
- The owner has confirmed: *"all write actions of the platform are gated by a single uniform admin
  token at the moment (for simplicity), but we may still want to know the signed-in user for the sake
  of observability of actions taken."* So **no RBAC** — keep the single uniform "admin" gate: any
  authenticated admin may do every write. Identity is wanted for **accounting**, *not* to restrict
  *what* a person can do. (If per-action roles are ever wanted, RBAC is an additive later arm — out
  of scope now.)

### Accounting (what happened) — **the actual motivation**
- The point of AuthN here is to **attribute every action to a real, verified identity** (the
  signed-in user), not just "someone with the token". Record that identity as the `owner`/actor on
  every write (it already threads into `historical_runs.owner`), and add an **`audit_log`** table —
  `(actor, action, target, params-summary, outcome, ip, at)` — for every admin write + sign-in/out,
  queryable in the admin UI. Backed by structured `tracing`/`metrics` (Arm 8).

## 3. Refined model (per owner clarifications, 2026-06-14)

Two principals, split cleanly:

- **Humans** sign in to establish a **verified identity** (for accounting). Authorization is uniform
  — any admin who can sign in may do every write. Owner-floated AuthN options: **GitHub OAuth**, a
  **JWT/OIDC** IdP, or local accounts. "Who is an admin" is a small **allowlist** (e.g. permitted
  GitHub logins, or an org/team) — that's the only authorization knob.
- **Machines/agents** keep a **non-interactive credential** (the existing `X-Cortex-Token` API
  token — agents can't do an interactive OAuth dance). Best-practice hardening: hash these at rest +
  make them revocable; the action's actor is the token's owner.

So: **human sign-in = identity (+ uniform admin); agent token = identity (+ uniform admin); every
action attributed to whichever; one `audit_log`.**

### The one remaining fork (asked now)
**Human AuthN method** — GitHub OAuth vs generic OIDC/JWT vs local accounts vs a combination. Drives
the whole build (an OAuth app + callback flow vs a `users` table + password hashing vs validating a
3rd-party JWT). Everything else (uniform authz, identity attribution, the `audit_log`) is settled.

### Stopgap (independent, optional)
A quick `cortex set-admin-token [--generate|<token>]` that writes a token to `cortex.toml [auth]`
(optionally a printed random one) so installs aren't hand-editing — useful for the **agent token**
regardless of the human-AuthN choice; plaintext until agent-key hashing lands.

## 4. Recommendation

- **GitHub OAuth for humans** (the owner's own first suggestion, and the lowest-friction *verified*
  identity for a dev-facing tool): sign in with GitHub → the GitHub login is the actor; admins = a
  configured allowlist of logins (or a GitHub org/team). No passwords for us to store, MFA handled by
  GitHub. Needs a GitHub OAuth App (owner provides `client_id`/`client_secret` via config) + a
  `secret_key` for the signed session cookie. `rocket_oauth2` handles the flow.
- **Keep the API token for agents** (hardened to hashed-at-rest as a follow-on).
- **Add `audit_log`** + attribute every action — the accounting the owner actually wants.
- *JWT/OIDC is the same shape if you'd rather be IdP-agnostic than GitHub-specific; local accounts if
  you want zero external dependency.*

## 5. Conclusion (2026-06-14, after the owner ruled out external auth)

GitHub OAuth was chosen, then **ruled out**: it makes each deployment register its own GitHub OAuth
App (`client_id`/`client_secret`) — exactly the external, per-deploy dependency the owner rejects.
Generic OIDC/JWT has the same shape (needs an IdP). And **self-contained *verified* identity
inherently needs a per-user secret** (passwords → local accounts), which is heavier than "lightweight
+ single uniform admin token for simplicity". So the pragmatic answer satisfying *every* stated
constraint (self-contained, lightweight, no external app, single uniform admin gate, but know the
actor) **reuses what already exists**:

- **AuthN / identity:** keep the existing **`token → owner`** scheme — give each admin their **own**
  token mapped to their name; the `/admin` cookie session (already built) then identifies the actor.
  No new auth, no external dependency, no passwords. (The shared single token is the *simplest* setup;
  per-admin tokens are the *identifiable* setup — same mechanism, the operator's choice.)
- **AuthZ:** **uniform** — any valid admin token/session may do every write. Unchanged.
- **Accounting (the genuinely missing pillar, the owner's actual ask — "observability of actions
  taken"):** a new **`audit_log`** table recording every admin action with its **actor** (the token's
  owner), its action/target/outcome and time, **viewable in the admin UI**. This is **auth-agnostic**
  (the actor is just a string), so it's forward-compatible if the auth model is ever upgraded
  (local accounts / OIDC) later.
- **Agents/machines:** keep the `X-Cortex-Token` API token. Optional hardening later: hash tokens at
  rest.

**So the build is the Accounting pillar (`audit_log`) on top of the existing token→owner identity —
no new auth dependency, nothing external.** That is what the following increments implement.
