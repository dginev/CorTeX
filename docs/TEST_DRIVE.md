# Test-driving CorTeX in a browser

You can click around the full CorTeX frontend against **real data** without a rebuild. The database
URL is now runtime config (figment — `docs/PRODUCTIZING_PLAN.md` Arm 1), so you just point the
frontend at any populated database when you launch it.

## One command

From the **repository root** (the frontend resolves `templates/`, `public/`, `Rocket.toml`,
`config.json` relative to the CWD):

```bash
# point at a populated DB; everything else uses defaults
DATABASE_URL="postgres://cortex:cortex@localhost/<your-db>" \
  ROCKET_PORT=8044 ROCKET_ADDRESS=127.0.0.1 \
  cargo run --release --bin frontend
# then open http://127.0.0.1:8044/
```

`DATABASE_URL` (legacy var, also read from `.env`) has the highest precedence; equivalently
`CORTEX_DATABASE__URL=…`. No recompile is needed to switch databases.

> Booting against a **restored production dump** (e.g. a `cortex_load` DB) gives the richest tour:
> multiple corpora, a real `tex_to_html` conversion service with results across every severity, tens
> of millions of log rows, and populated historical runs.

## What to explore (human screens)

| URL | Screen |
|---|---|
| `/` | Corpora overview (all registered corpora) |
| `/corpus/<corpus>` | Services registered for a corpus |
| `/corpus/<corpus>/<service>` | Service report — severity breakdown (No-problem/Warning/Error/Fatal/Invalid) |
| `/corpus/<corpus>/<service>/<severity>` | Drill into one severity — the `what`/category rollup |
| `/corpus/<corpus>/<service>/<severity>/<category>` | Categories → individual messages |
| `/corpus/<corpus>/<service>/<severity>/<category>/<what>` | The task list for one message kind |
| `/services` | Service registry |
| `/jobs` | Background-jobs dashboard (status, duration, health) |

Reports are served from the `report_summary` materialized-view rollup, so even severity reports over
tens of millions of `log_*` rows render in well under a second.

## Agent-API parity

CorTeX is **agent-first**: every human screen has a JSON twin, and every human *write action* has a
token-gated agent endpoint — the symmetry contract's north star ("every human screen action has a
1:1 documented agent API"). The **canonical, never-drifting list** is the live discovery index — it
is introspected from the running route table, so (unlike a hand-kept list) it can't go stale:

```bash
curl -s http://127.0.0.1:8044/api | jq .      # every /api endpoint: method, path, handler name
```

### Read (no secrets)

```bash
curl -s http://127.0.0.1:8044/api/corpora | jq .                       # corpora overview twin
curl -s http://127.0.0.1:8044/api/services | jq .                      # service registry twin
curl -s http://127.0.0.1:8044/api/services/<service>/workers | jq .    # worker fleet (+ liveness age)
curl -s 'http://127.0.0.1:8044/api/reports/<corpus>/<service>/<severity>?offset=0&page_size=50' | jq .
curl -s http://127.0.0.1:8044/api/runs/<corpus>/<service> | jq .       # historical runs
curl -s 'http://127.0.0.1:8044/api/runs/<corpus>/<service>/diff?previous=<ISO>&current=<ISO>' | jq .
curl -s http://127.0.0.1:8044/api/jobs | jq .                          # background jobs (+ health, heartbeat age)
curl -s http://127.0.0.1:8044/healthz | jq .                           # PUBLIC liveness: {status, database.reachable}
curl -s -H 'X-Cortex-Token: <token>' http://127.0.0.1:8044/api/health | jq .  # token-gated detail: migrations/pool/dispatcher/storage
curl -s http://127.0.0.1:8044/api/config | jq .                        # masked effective config
```

### Write & manage (token-gated)

The full admin lifecycle — *install a corpus → register & activate a service → monitor → rerun →
maintain → manage runs* — has agent twins. Each write carries a rerun token via
`-H 'X-Cortex-Token: <token>'` (or `?token=<token>`); a missing/bad token is `401`. Tokens come from
`config.json` `auth.rerun_tokens` (token → owner, threaded as the action's actor). Long-running
writes return `202` + a job handle — poll `GET /api/jobs/<uuid>` (or watch `/jobs`) for progress,
health, and heartbeat age.

```bash
T='X-Cortex-Token: <token>'
# import a corpus (202 + job)
curl -s -H "$T" -X POST -H 'Content-Type: application/json' \
  -d '{"name":"demo","path":"/data/demo","complex":false,"description":""}' \
  http://127.0.0.1:8044/api/corpora
curl -s -H "$T" -X POST http://127.0.0.1:8044/api/corpora/demo/extend          # re-scan for new entries
# register a service (201), then activate it on the corpus (202 + job)
curl -s -H "$T" -X POST -H 'Content-Type: application/json' \
  -d '{"name":"tex_to_html","version":0.1,"inputformat":"tex","outputformat":"html","complex":true}' \
  http://127.0.0.1:8044/api/services
curl -s -H "$T" -X POST http://127.0.0.1:8044/api/corpora/demo/services/tex_to_html
# retire a service from a corpus (deletes that pair's tasks + logs; confirm echoes the service)
curl -s -H "$T" -X DELETE 'http://127.0.0.1:8044/api/corpora/demo/services/tex_to_html?confirm=tex_to_html'
# manage runs: rerun a slice, then rebuild the report rollup
curl -s -H "$T" -X POST 'http://127.0.0.1:8044/api/reports/demo/tex_to_html/rerun?severity=error'
curl -s -H "$T" -X POST http://127.0.0.1:8044/api/reports/refresh
# DB-health maintenance (online; run ANALYZE after a bulk import/rerun)
curl -s -H "$T" -X POST http://127.0.0.1:8044/api/maintenance/reindex
curl -s -H "$T" -X POST http://127.0.0.1:8044/api/maintenance/analyze
# delete a corpus (confirmation echoes the name)
curl -s -H "$T" -X DELETE 'http://127.0.0.1:8044/api/corpora/demo?confirm=demo'
```

> **Note (symmetry mechanism):** read parity is currently via *parallel* `/api/*` routes, not
> content-negotiation on the human URL — `GET /corpus/…` with `Accept: application/json` still returns
> HTML. The CLAUDE.md symmetry contract prefers one content-negotiated controller per screen so the
> HTML and JSON views can't drift; converging the parallel routes onto that shape is open follow-up
> (`docs/OPEN_QUESTIONS.md` #5).

## Notes

- Read-only browsing needs no secrets. The token-gated write actions (rerun, save-snapshot) read
  tokens from the **single** JSON token file — `config.json` in the CWD, or wherever
  `CORTEX_AUTH_FILE` points (there is no `cortex.toml [auth]` layering).
- Run the **dispatcher** + a **worker** as well (see `INSTALL.md` §7) to watch tasks actually move
  through TODO → result; the frontend alone is a read/report view of whatever the DB already holds.
