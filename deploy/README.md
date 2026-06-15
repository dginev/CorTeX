# CorTeX deployment — public edge + local systemd control

How the home desktop's CorTeX frontend is exposed at **https://corpora.latexml.rs**
and kept healthy under `systemctl`. Two halves:

1. **Local (this desktop):** systemd-managed frontend + reverse-SSH tunnel + health watchdog.
2. **Edge (Vultr VPS `ar5iv-edit-1`, the public anchor for `*.latexml.rs`):** Caddy TLS,
   a shared Anubis bot wall, and a small Host-router.

```
                         ┌─ /api/*, /healthz, /public/*, /robots.txt, /favicon.ico ─┐ (exempt: agents/assets,
 PUBLIC ─https─▶ Caddy ──┤                                                          │  never challenged)
              (:443 TLS) │                                                          ▼
                         └─ HTML report pages ─▶ Anubis (PoW) ─┐        reverse-SSH tunnel
                            (anti bot-crawl)     (one shared    │   ┌──────────────────────────┐
                                                  instance)     ▼   ▼                          │
                                              Caddy router :8181 ── corpora ─▶ 127.0.0.1:8000 ──┤
                                               (split by Host)  └── else ────▶ 127.0.0.1:3000   │  (edge loopback)
                                                                              (ar5iv editor)    │
                                                                                                │
   DESKTOP  frontend 127.0.0.1:8000  ◀─────────── ssh -R 127.0.0.1:8000 ───────────────────────┘
            (Rocket, systemd)                      (cortex-tunnel.service)
```

## Why this shape

- **Reverse SSH tunnel, not a public port:** the desktop is residential (dynamic IP, NAT,
  ISP-blocked :80/:443) and the `*.latexml.rs` DNS already points at the Vultr box. The desktop
  dials *out* to the fixed edge, so a changing home IP just triggers a reconnect — nothing to
  re-point. The frontend stays bound to `127.0.0.1` (smallest blast radius).
- **One shared Anubis:** the edge VM is RAM-tight and Anubis is single-`TARGET`, so we reuse the
  editor's instance via a Host-router (`:8181`) instead of running a second. cortex's report URLs
  (`/corpus/<c>/<s>/<sev>/<cat>/<what>?…`) are a combinatorial bot-crawl DB-load trap, so HTML is
  PoW-walled; the agent JSON API + liveness + assets bypass the wall so tools never see a challenge.

## Local install (this desktop)

Units live in `deploy/systemd/`. Install:

```bash
sudo install -d -m 755 /etc/cortex
sudo cp deploy/systemd/frontend.env.example /etc/cortex/frontend.env   # then EDIT it (DB url!)
sudo chmod 600 /etc/cortex/frontend.env
sudo cp deploy/systemd/cortex-*.service deploy/systemd/cortex-*.timer /etc/systemd/system/
sudo cp deploy/systemd/cortex-healthcheck.sh /usr/local/bin/ && sudo chmod 755 /usr/local/bin/cortex-healthcheck.sh
sudo systemctl daemon-reload
```

The dedicated tunnel key (`~/.ssh/cortex_tunnel_ed25519`) maps to a locked-down `cortextunnel`
user on the edge whose `authorized_keys` is `restrict,port-forwarding,permitlisten="127.0.0.1:8000"`
— it can hold *only* that one reverse forward and nothing else.

### Services

| Unit | Role |
|---|---|
| `cortex-tunnel.service`   | reverse-SSH tunnel desktop→edge (`-R 127.0.0.1:8000`). `Restart=always`. **Enabled.** |
| `cortex-frontend.service` | the Rocket web app. `Restart=always` (crash recovery). Binds `127.0.0.1:8000`. |
| `cortex-health.timer`     | every 30 s runs the watchdog: if the service is active but `/healthz` is unreachable or not `ok`, `systemctl restart cortex-frontend` (catches a *hang* a crash-restart can't). |

```bash
systemctl status cortex-frontend cortex-tunnel
journalctl -u cortex-frontend -f        # logs
systemctl restart cortex-frontend       # manual control
```

## Cutover (start managing the frontend with systemd)

The frontend is currently run by hand. To hand it to systemd:

```bash
# 1) build a release binary (the unit points at target/release/frontend)
cd /home/deyan/git/cortex && cargo build --release --bin frontend
# 2) set the REAL database in the env file (placeholder ships pointing at localhost/cortex;
#    the hand-run process points at the load-test DB cortex_load — pick the production one)
sudoedit /etc/cortex/frontend.env
# 3) stop the hand-run frontend, hand :8000 to systemd
pkill -f 'target/debug/frontend'        # or however it was launched
sudo systemctl enable --now cortex-frontend.service cortex-health.timer
# 4) confirm end-to-end
curl -s https://corpora.latexml.rs/healthz | head -c 80
```

Both bind `127.0.0.1:8000`, so the tunnel + edge need no changes across the swap.

## Rollback

- **Local:** `sudo systemctl disable --now cortex-frontend cortex-health.timer` and run the
  frontend by hand again. `sudo systemctl stop cortex-tunnel` drops the public path entirely.
- **Edge:** timestamped backups sit next to the live files —
  `/etc/caddy/Caddyfile.bak.cortex*` and `/opt/ar5iv-editor/deploy/docker-compose.yml.bak.cortex*`.
  Restore + `systemctl reload caddy` / `docker compose up -d`. See `deploy/edge/`.
