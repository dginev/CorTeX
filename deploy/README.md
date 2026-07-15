# CorTeX deployment — public edge + local systemd control

How the home desktop's CorTeX frontend is exposed at **https://corpora.latexml.rs**
and kept healthy under `systemctl`. Two halves:

1. **Local (this desktop):** systemd-managed frontend + reverse-SSH tunnel + health watchdog.
2. **Edge (Vultr VPS `ar5iv-edit-1`, the public anchor for `*.latexml.rs`):** Caddy TLS,
   a shared Anubis bot wall, and a small Host-router.

```
                         ┌─ /healthz, /public/*, /robots.txt, /favicon.ico ─────────┐ (exempt: liveness+assets
 PUBLIC ─https─▶ Caddy ──┤                                                          │  only, never challenged)
              (:443 TLS) │                                                          ▼
                         └─ HTML pages + /api/* + /entry/* ─▶ Anubis (PoW) ─┐  reverse-SSH tunnel
                            (anti bot-crawl: API+archives too) (one shared  │   ┌──────────────────────────┐
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
  (`/corpus/<c>/<s>/<sev>/<cat>/<what>?…`) are a combinatorial bot-crawl DB-load trap, so HTML —
  **plus `/api/*` and `/entry/*`** (owner 2026-06-16: don't let bots crawl the API or the source/
  result archives) — is PoW-walled; **only `/healthz` + page assets bypass** the wall (external agents
  just need an uptime check; the main agentic use is on the unguarded localhost). This closes the
  `KNOWN_ISSUES.md` X-4 un-walled-`/api` gap once the edge Caddyfile is reloaded (`deploy/edge/
  corpora.caddy`).

## Local install (this desktop)

Units live in `deploy/systemd/`. Install:

```bash
sudo install -d -m 755 /etc/cortex
sudo cp deploy/systemd/frontend.env.example /etc/cortex/frontend.env   # then EDIT it (DB url!)
sudo cp deploy/systemd/dispatcher.env.example /etc/cortex/dispatcher.env  # then EDIT it (DB url!)
sudo chmod 600 /etc/cortex/frontend.env /etc/cortex/dispatcher.env
sudo cp deploy/systemd/cortex-*.service deploy/systemd/cortex-*.timer /etc/systemd/system/
sudo cp deploy/systemd/cortex-healthcheck.sh /usr/local/bin/ && sudo chmod 755 /usr/local/bin/cortex-healthcheck.sh
sudo systemctl daemon-reload
```

The dedicated tunnel key (`~/.ssh/cortex_tunnel_ed25519`) maps to a locked-down `cortextunnel`
user on the edge whose `authorized_keys` is `restrict,port-forwarding,permitlisten="127.0.0.1:8000"`
— it can hold *only* that one reverse forward and nothing else.

### Worker host prerequisites (AppArmor + Ghostscript)

The worker fleet rasterizes EPS/PS figures with `gs`, staging in
`TMPDIR=/opt/cortex-scratch`. Ubuntu's stock `gs` AppArmor profile confines
`/usr/bin/gs` to `@{HOME}`/`/tmp`/`/mnt`/`/media` and denies the scratch dir, so
every EPS/PS conversion silently fails (gs aborts with `/undefinedfilename` yet
exits 0). Install the sanctioned local override (idempotent):

```bash
sudo install -d -m 755 /etc/apparmor.d/local
sudo cp deploy/apparmor/local-gs /etc/apparmor.d/local/gs
sudo apparmor_parser -r /etc/apparmor.d/gs
```

Also ensure `ghostscript imagemagick poppler-utils mupdf-tools` are installed
(the converter chain) and that ImageMagick's `policy.xml` permits PS/EPS/PDF.

The **containerized worker** avoids all of the above — it bakes the
gs/ImageMagick setup in and runs under `docker-default`, unaffected by the host
`gs` profile. latexml-oxide publishes it to GHCR, built from its unified
`Dockerfile --target worker`:

```bash
docker run --network host -v /opt/cortex-scratch:/opt/cortex-scratch \
  --hostname="$(hostname)" ghcr.io/dginev/latexml-oxide/cortex-worker <dispatcher-host>
```

Published from latexml-oxide **0.7.4** onward — the unified `Dockerfile` postdates 0.7.3,
which has no `worker` target. Each published release pushes `:<tag>` and `:latest`.

See latexml-oxide's `docker/README.md` for the full turnkey run reference
(`PROFILE`/`WORKERS` env, standalone mode). The systemd units below run the
bare-binary fleet instead (`WORKER_BIN` from a local `maxperf-cortex` build).

### Services

| Unit | Role |
|---|---|
| `cortex-tunnel.service`   | reverse-SSH tunnel desktop→edge (`-R 127.0.0.1:8000`). `Restart=always`. **Enabled.** |
| `cortex-frontend.service` | the Rocket web app. `Restart=always` (crash recovery). Binds `127.0.0.1:8000`. |
| `cortex-dispatcher.service` | the ZeroMQ dispatcher (ventilator `:51695` + sink `:51696`, all interfaces incl. the Tailscale IP — see the worker-network section in `docs/DEPLOYMENT.md`). `Restart=always` — systemd *is* the supervisor for the dispatcher's deliberate fail-fast panic. `EnvironmentFile=/etc/cortex/dispatcher.env`. |
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

## Dispatcher cutover (run conversions under systemd)

The dispatcher is independent of the frontend/tunnel/edge (those serve the read-only public site; the
dispatcher runs the conversion pipeline for the worker fleet). To manage it with systemd:

```bash
# 1) build the release binary (the unit points at target/release/dispatcher)
cd /home/deyan/git/cortex && cargo build --release --bin dispatcher
# 2) set the REAL pipeline database (NOT the static showcase DB cortex_load)
sudoedit /etc/cortex/dispatcher.env
# 3) start it (binds :51695/:51696 on all interfaces, incl. the Tailscale IP 100.x for remote workers)
sudo systemctl enable --now cortex-dispatcher.service
# 4) confirm it bound + accepts a worker connection
ss -ltn | grep -E ':51695|:51696'
journalctl -u cortex-dispatcher -f
#    from a worker box on the LAN/Tailscale:  nc -vz <dispatcher-ip> 51695
```

Workers (the external `pericortex` crate) point at `tcp://<dispatcher-ip>:51695` / `:51696` — the LAN IP
for co-located boxes, the Tailscale `100.x` for remote ones. No edge/tunnel involvement; see the
worker-network section in `docs/DEPLOYMENT.md`.

## Rollback

- **Local:** `sudo systemctl disable --now cortex-frontend cortex-health.timer` and run the
  frontend by hand again. `sudo systemctl stop cortex-tunnel` drops the public path entirely.
  `sudo systemctl disable --now cortex-dispatcher` stops the conversion pipeline (independent of the
  public site; safe to stop/start on its own).
- **Edge:** timestamped backups sit next to the live files —
  `/etc/caddy/Caddyfile.bak.cortex*` and `/opt/ar5iv-editor/deploy/docker-compose.yml.bak.cortex*`.
  Restore + `systemctl reload caddy` / `docker compose up -d`. See `deploy/edge/`.
