# Deploying CorTeX (and the `corpora.latexml.rs` preview)

CorTeX is a self-contained app: a Postgres database, the ZeroMQ dispatcher, and the Rocket frontend
(see [`INSTALL.md`](../INSTALL.md) for the build + `cortex init`). This note covers the **deployment
edge** decisions for the first public preview — things that are intentionally **not framework code**.

## First preview: `https://corpora.latexml.rs`

A read-only public view of the ar5iv dashboards + document previews. The public/read surface is
served openly; the **write / admin** surfaces stay gated (signed-in admins only) and the **agent
API** stays token-gated. The deployment is reverse-proxied (Caddy) over Tailscale to the frontend on
the `cortex` host.

## Worker ↔ dispatcher network

There are **two independent networks** in a CorTeX deployment — don't conflate them:

| Path | Carries | Latency-/throughput-critical? |
|---|---|---|
| **Web / dashboard** | the Rocket frontend HTTP (Caddy → Anubis → reverse-proxy → cortex) | No — reads hit the `report_summary` rollup; proxy hops are fine |
| **Worker ↔ dispatcher** | ZeroMQ: ventilator `:51695` (source out) + sink `:51696` (results in) | **Yes** — streams source/result archives (100 KB chunks, up to the `max_result_bytes` cap) |

The **workers must never traverse the web edge** (Anubis PoW + reverse-SSH is exactly wrong for
high-throughput ZMQ). Give them a **single hop on a private network**. The dispatcher binds
`tcp://*:<port>` (all interfaces), so reachability is purely a *network + firewall* question, not a
code one.

**Co-located workers (same LAN) — the fastest option.** Point workers at `tcp://<dispatcher-lan-ip>:51695`
/ `:51696`. Prefer **wired Ethernet over WiFi**: lower, stable latency, full-duplex throughput, and no
WiFi power-save/roaming dropping idle connections. WiFi *works* (with only a couple of CPU-bound
`latexml` workers the conversion time dominates the network), but wired removes a variable. Either way:
**pin the dispatcher host's IP** (static or a DHCP reservation) so the workers' `tcp://<ip>` config
survives a reconnect.

**Remote workers — Tailscale (WireGuard mesh).** Each node gets a stable `100.x` address that survives
IP changes; kernel WireGuard is fast (UDP) and encrypted. Workers connect to the dispatcher's **Tailscale
IP** on `:51695` / `:51696`. Readiness:

- **Tailscale up** on the dispatcher host (`tailscale ip -4` → the `100.x` to give workers; `tailscale status`
  lists the worker peers).
- **Tailscale ACLs**: the default policy allows all traffic between a tailnet's own nodes, so same-owner
  worker machines reach the ports out of the box. Only a *custom* ACL would need an explicit grant for
  `:51695`/`:51696`.
- **Host firewall**: traffic arriving on `tailscale0` must be accepted. Tailscale's own nftables chain
  already accepts `iifname "tailscale0"`; with a default-`accept` INPUT policy (or `ufw allow in on
  tailscale0`) nothing more is needed. Verify from a worker: `nc -vz <dispatcher-tailscale-ip> 51695`
  (only returns once the dispatcher is actually running and bound).

**Stability knob:** `dispatcher.tcp_keepalive_idle_seconds` (default 120) keeps idle worker connections
alive across NAT/WiFi/overlay idle-timeouts so a worker doesn't silently fall out of the fleet. Task
*correctness* is independent of it (the lease reaper recovers a dead worker's task), so it's safe to
disable (`<= 0`).

**Avoid:** per-worker reverse-SSH tunnels (TCP-over-TCP head-of-line blocking, doesn't scale, chokes on
large multipart archives) and **raw public exposure** of `:51695`/`:51696` — ZeroMQ has no authentication
by default, so keep it on the private LAN/Tailscale overlay (a public bind would require ZMQ CURVE, which
is not configured).

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

## Metrics (Prometheus)

CorTeX exposes operational gauges at **`GET /metrics`** in Prometheus exposition format —
connection-pool saturation (`cortex_pool_in_use` / `cortex_pool_max`), background-job backlog
(`cortex_jobs_active`), active admin sessions, registered corpora/services, and the dispatcher worker
fleet (`cortex_workers_total`, `cortex_workers_in_flight_total`), plus `cortex_db_reachable` and
`cortex_build_info`. It is **token-gated** (the same `auth.rerun_tokens`), so it is not public; keep
it internal regardless. Scrape it by passing the token as a query param:

```yaml
scrape_configs:
  - job_name: cortex
    metrics_path: /metrics
    params:
      token: ["<an admin token>"]   # or set an Authorization-style X-Cortex-Token header
    static_configs:
      - targets: ["cortex:8000"]
```

These are cheap current-state gauges read per scrape (no dispatcher instrumentation, no ZMQ/storage
probe — that is the token-gated `/api/health`'s job; the public `/healthz` is liveness-only). Real-time
event counters (request rates, per-task tallies) are a follow-on that needs hot-path instrumentation.

## Checklist (preview)

1. `cortex init` (migrate + scaffold `cortex.toml`); tune Postgres (`cortex tune-db`).
2. `cortex set-admin-token --generate --owner <you>` → sign in once with the token, enroll a passkey.
3. Set `[webauthn]` to the deploy domain (above); restart the frontend.
4. Put **Anubis** in front of the public origin; allow `/api/*` through for agents.
5. Confirm: public read views load; `/admin` requires sign-in; `/api/*` works with a token.
