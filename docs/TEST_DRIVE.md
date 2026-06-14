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

Every human screen has a JSON twin under `/api/…` (13 handlers: `api_corpora`, `api_corpus`,
`api_services`, `api_service_workers`, `api_runs`, `api_run_current`, `api_run_diff`,
`api_run_task_diffs`, `api_category_report`, `api_what_report`, `api_jobs`, `api_job`, `api_config`).
For example:

```bash
curl -s -H 'Accept: application/json' http://127.0.0.1:8044/api/corpora | jq .
curl -s http://127.0.0.1:8044/api/jobs | jq .
```

> **Note (symmetry mechanism):** parity is currently via *parallel* `/api/*` routes, not
> content-negotiation on the human URL — `GET /corpus/…` with `Accept: application/json` still returns
> HTML. The CLAUDE.md symmetry contract prefers one content-negotiated controller per screen so the
> HTML and JSON views can't drift; converging the parallel routes onto that shape is open follow-up.

## Notes

- Read-only browsing needs no secrets. The token-gated write actions (rerun, save-snapshot) read
  `config.json` / `CORTEX_AUTH__…`.
- Run the **dispatcher** + a **worker** as well (see `INSTALL.md` §7) to watch tasks actually move
  through TODO → result; the frontend alone is a read/report view of whatever the DB already holds.
