# CorTeX operator manual

How to install, run, and operate CorTeX day-to-day — from a fresh box through managing historical
runs. This is the **admin journey**; for the *why* behind the architecture see
[`docs/PRODUCTIZING_PLAN.md`](docs/PRODUCTIZING_PLAN.md) and the rationalization docs.

> **Every capability is available on three surfaces** (the symmetry contract): the web UI, the agent
> API, and the `cortex` CLI — one controller and one backbone behind each, so all three see the same
> live + historical state. The same handler serves an HTML page to a browser and schema'd JSON to API
> clients; anything you can do in the UI you can script via the [Agent API](#13-agent-api) or the
> [`cortex` CLI](#14-command-line-management-cli). Machine-readable docs live at **`/api/docs`**
> (RapiDoc) and **`/api/openapi.json`**.

## 1. The pieces

CorTeX is three binaries over one Postgres database and a shared `/data` filesystem:

| Binary | Role | Start (from the repo root) |
| --- | --- | --- |
| **`cortex`** | admin CLI — install, diagnose, tokens + scriptable management (reports, runs, rerun, sandbox, delete, export); see [§14](#14-command-line-management-cli) | `cargo run --bin cortex -- <subcommand>` |
| **`frontend`** | Rocket web app + agent API (default `127.0.0.1:8000`) | `cargo run --bin frontend` |
| **`dispatcher`** | leases tasks to workers over ZeroMQ (ventilator `:51695`, sink `:51696`) | `cargo run --bin dispatcher` |

Conversions are performed by **external workers** (the `pericortex` crate) that connect to the
dispatcher — they are separate processes, not started by CorTeX. **Always run the binaries from the
repository root** (templates, `Rocket.toml`, and `config.json` are resolved relative to the CWD).

## 2. Installation

Full, verified steps are in **[`INSTALL.md`](INSTALL.md)**. The short path on a fresh Ubuntu box:

```bash
# 1. Build dependencies (Postgres, ZeroMQ, libsodium)
sudo apt install -y postgresql libpq-dev libzmq3-dev libsodium-dev pkg-config

# 2. Initialize: runs embedded migrations + scaffolds cortex.toml if missing
cargo run --bin cortex -- init

# 3. Create the first admin token (printed once; attributed to an owner in the audit log)
cargo run --bin cortex -- set-admin-token --generate --owner alice

# 4. Verify the installation is healthy
cargo run --bin cortex -- doctor
```

`doctor` reports database reachability, migration currency, whether the magic services are seeded, and
whether an admin token is configured — `=> healthy` or `=> DEGRADED`. Add `--json` for a machine-
readable report (the same data backs the `/health` screen). Put the **database on NVMe, never
`/data`** (the QLC RAID6 array is for document bytes, not an OLTP DB). For server tuning run
`cargo run --bin cortex -- tune-db` (and see [`docs/DB_TUNING.md`](docs/DB_TUNING.md)).

## 3. Configuration

Settings resolve with this precedence (highest last):
**built-in defaults → `cortex.toml` → `CORTEX_`-prefixed env (`CORTEX_DATABASE__URL`) → legacy
`DATABASE_URL` / `.env`**. No recompile is needed to change the database or ports — e.g. point the
frontend at a populated DB with `DATABASE_URL=… cargo run --bin frontend` (see
[`docs/TEST_DRIVE.md`](docs/TEST_DRIVE.md)).

Key sections of `cortex.toml`:

- `[database]` — `url`, `test_url`.
- `[dispatcher]` — `source_port` (51695), `result_port` (51696), `max_in_flight`, queue/retry knobs.
- `[auth]` — `rerun_tokens` (token → owner map; managed via `set-admin-token`, not hand-edited).
- `[webauthn]` — passkey relying-party settings (origin, rp-id), if passkeys are enabled.

## 4. Access & authentication

Two ways to authenticate as an admin; both resolve to a **server-side session** (an opaque cookie):

- **Passkeys (WebAuthn)** — the primary, local, no-external-dependency method. Enroll at
  **`/admin/passkeys`** once signed in; thereafter sign in with your device authenticator. See
  [`docs/archive/WEBAUTHN_DESIGN.md`](docs/archive/WEBAUTHN_DESIGN.md).
- **Admin token** — the bootstrap / break-glass path (and the credential agents use). Created by
  `set-admin-token`; presented as a `?token=…` query param or `Authorization` header. Each token maps
  to an **owner** that is threaded into the audit log as the actor.

Sign in at **`/admin/login`**. A GET screen that needs authorization redirects an anonymous visitor to
`/admin/login?next=<destination>` and returns you there after signing in. Active sessions are listed
at **`/admin/sessions`** (revocable by owner; session ids are never exposed). All mutating actions are
recorded in the **audit log** (`/admin/audit`).

> **Perimeter (deployment):** the public preview is fronted by **Anubis** as a one-time deployment
> measure (bot/abuse mitigation), not a framework feature. See
> [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

## 5. The admin dashboard

**`/admin`** (sign-in gated) is the command center, linking everything below: services, jobs, health,
settings, runs, retention, sessions, audit, passkeys, and "add a corpus". The persistent top nav
(`cortex-admin-nav`) appears on every page.

## 6. Corpus & service lifecycle

**Add a corpus** (register + import) — the "Add a corpus" form on `/admin`, or `POST /api/corpora`.
Import runs as a [background job](#7-running-conversions); poll its handle for progress. Browse a
corpus at **`/corpus/<name>`**; delete it (cascade-clean, orphan-free) from its page or
`DELETE /api/corpora/<name>?confirm=<name>`.

**Services** — the registry is at **`/services`** (twin: `GET /api/services`). Magic services `init`
(1) and `import` (2) are infrastructure; real conversion services are `id > 2`.

| Action | Screen / form | Agent API |
| --- | --- | --- |
| Register a service | `/services` → "Register a service" | `POST /api/services` |
| Activate on a corpus (create tasks) | corpus page | `POST /api/corpora/<c>/services/<s>` |
| Extend (add newly-imported entries) | corpus page | `POST /api/corpora/<c>/extend` |
| Deactivate from a corpus (retire its tasks+logs) | corpus page | `DELETE /api/corpora/<c>/services/<s>?confirm=<s>` |
| **Delete the service** (all corpora, orphan-free) | `/services` → "Delete" | `DELETE /api/services/<s>?confirm=<s>` |
| Worker fleet for a service | `/workers/<service>` | `GET /api/services/<service>/workers` |

`init`/`import` are protected — deletion/deactivation is rejected `403`.

## 7. Running conversions

1. Start the **dispatcher** (`cargo run --bin dispatcher`) — it leases `TODO` tasks, streams sources
   to workers (ventilator), and ingests result archives (sink), persisting each result's status.
2. Start **workers** (the external `pericortex` processes) pointed at this host's ventilator/sink
   ports. They request work, convert, and return result archives.
3. **Activate a service on a corpus** to create the `TODO` tasks for the fleet to chew through.
4. **Rerun** a slice (clear results back to `TODO`) from a report screen or
   `POST /api/reports/<c>/<s>/rerun?<severity>&<category>&<what>&<description>` to reprocess (e.g. after
   a worker upgrade).

Task status is a signed int (`TODO=0`, `NoProblem=-1`, `Warning=-2`, `Error=-3`, `Fatal=-4`,
`Invalid=-5`, `Blocked<-5`, `Queued>0` while leased). A leased task is marked `Queued` durably; a
crash recovers it (`Queued`→`TODO` on dispatcher restart), and a lease unreturned past its
visibility timeout is re-queued (with a bounded retry budget before it dead-letters).

## 8. Background jobs

Long-running admin operations (import, service activation, report refresh, reindex/analyze, exports)
run as **persisted background jobs** so the request returns immediately. Watch them at **`/jobs`**
(twin: `GET /api/jobs`), drill into one at `/jobs/<uuid>` (`GET /api/jobs/<uuid>`). Each job carries
`status`, `health` (`ok`/`failed`/`interrupted`/`pending`/`running`), `progress`, `duration_seconds`,
and `seconds_since_update` (the **heartbeat age** — a climbing value on a `running` job flags a stall).
`GET /api/jobs?active=true` is the fleet-wide **pending check**. A job whose heartbeat goes silent past
the threshold is auto-reaped to `interrupted`; one that dies with the process is cleaned at restart.
See [`docs/archive/JOB_MODEL.md`](docs/archive/JOB_MODEL.md).

## 9. Monitoring & health

- **`/health`** — DB reachability, migrations, seeded services, token readiness (the same data as
  `cortex doctor --json`).
- **`/metrics`** — Prometheus text (token-gated): pool gauges, DB reachability, corpus/service/job/
  session/worker counts, in-flight work, build info. Scrape config in
  [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).
- **`/workers/<service>`** — per-worker dispatch/return tallies + in-flight backlog + liveness age (a
  climbing age or growing backlog flags a stuck worker).

## 10. Reports

A corpus/service report drills down by severity and category: corpus overview → severity → category →
the individual entries and their messages, with a single-entry preview. The agent twin is
`GET /api/reports/<corpus>/<service>/<severity>[/<category>]` returning the same typed DTO.

## 11. Managing historical runs

Every service activation/rerun opens a **run**; per-run tallies live in `historical_runs` and per-task
snapshots in `historical_tasks`.

| View | Screen | Agent API |
| --- | --- | --- |
| All runs, filterable (corpus / service / owner) | **`/admin/runs`** | `GET /api/runs?<corpus>&<service>&<owner>&<limit>` |
| A service's run history | `/runs/<c>/<s>` | `GET /api/runs/<c>/<s>` |
| Current run | — | `GET /api/runs/<c>/<s>/current` |
| **Run-to-run diff** (what changed between two runs) | `/runs/<c>/<s>/diff?<previous>&<current>` | `GET /api/runs/<c>/<s>/diff?…` |
| **Per-task diff** (which entries changed status) | `/runs/<c>/<s>/tasks?…` | `GET /api/runs/<c>/<s>/tasks?…` |
| History chart | `/history/<c>/<s>` | — |

**Retention** — preview and prune old `historical_tasks` snapshots at **`/admin/retention`** (dry-run
count first; confirmed prune by cutoff date, audited). Twin: `GET /api/historical/stats`,
`POST /admin/retention/prune`.

## 12. Maintenance

Run as background jobs (debounced; safe online):

- **Refresh reports** — rebuilds the `report_summary` rollup that backs every report page (also runs
  automatically on run completion; `POST /api/reports/refresh`; see
  [`docs/archive/REPORT_FRESHNESS.md`](docs/archive/REPORT_FRESHNESS.md)).
- **Reindex / analyze** — `REINDEX (CONCURRENTLY)` + `ANALYZE` on the high-churn tables (no exclusive
  lock). See [`docs/DB_TUNING.md`](docs/DB_TUNING.md).

**Export an HTML dataset** — bundle a corpus/service's converted HTML into ZIP archives (the
replacement for the old `bundle-html-dataset*.sh` scripts):

```bash
# one archive per year-month (default); or --group-by severity for one archive per severity
cargo run --bin cortex -- export-dataset arxmliv tex_to_html --out /data/datasets/arxmliv-2024 \
  --group-by month --severity no_problem,warning,error
```

Reads existing result archives off `/data` (no conversion); resumable (an existing archive is
skipped); writes a `<corpus>-manifest.json` provenance sidecar (corpus/service/severities/grouping/
counts/version). Severity keys are the canonical `no_problem` / `warning` / `error` / `fatal` /
`invalid`.

Back up the **Postgres** database (metadata) and the **`/data`** filesystem (document bytes)
separately; delete a corpus only through the app (orphan-free cascade), never a raw `DELETE`.

## 13. Agent API

Tokens carry an owner identity into the audit log. Examples:

```bash
TOKEN=…   # an admin/API token from `set-admin-token`
curl -s localhost:8000/api/jobs?active=true | jq .                 # pending background jobs
curl -s localhost:8000/api/runs?limit=20 | jq .                    # recent runs across the system
curl -s "localhost:8000/api/runs/arxiv/tex_to_html/diff?previous=3&current=4" | jq .
curl -s -X DELETE "localhost:8000/api/services/old_svc?confirm=old_svc&token=$TOKEN"
```

Browse the full, generated contract at **`/api/docs`**.

## 14. Command-line management (CLI)

The `cortex` binary is the **third surface** — a scriptable twin of the web screens and the agent API,
running against the same database (`DATABASE_URL` / config precedence, §3). Install & ops commands
(`init`, `doctor`, `set-admin-token`, `tune-db`, `export-dataset`) are covered above; the management
commands below mirror the web/agent capabilities one-to-one. Add `--json` to any **read** command to
emit the same shape as the corresponding agent DTO (so a script gets identical numbers to the screen).

**Read — the report ladder, scriptable:**

```bash
cortex report   arxmliv tex_to_html             # service overview: valid-task total + per-status counts/shares
cortex runs     arxmliv tex_to_html             # run history: each run's per-severity tallies (live for the open run)
cortex document arxmliv tex_to_html 2105.13573  # per-article forensics: status + every worker-log message
```

**Mutations — consequential, so dry-run by default; pass `--yes` to execute:**

```bash
cortex rerun         arxmliv tex_to_html --severity error            # re-queue a filtered slice (→ TODO) for reconversion
cortex sandbox       arxmliv err-set --service tex_to_html --severity error  # carve a sandbox corpus by a message filter
cortex delete-corpus old-sandbox                                     # orphan-free cascade delete (run tallies survive)
```

Without `--yes`, a mutation prints exactly what it *would* do (the matched scope / blast radius) and
changes nothing — a safe preview. `--owner <name>` attributes the action in the audit log (default
`admin`). These are the same operations as the web forms and `POST /api/…` endpoints, on one shared
backend, so all three surfaces see the same live + historical state.

## 15. Troubleshooting

- **`cortex doctor`** first — it pinpoints DB/migration/seed/token problems.
- **Frontend won't start / wrong DB** — check the CWD (run from repo root) and the config precedence
  (§3); `DATABASE_URL=… cargo run --bin frontend` overrides at runtime.
- **A job is stuck `running`** — check its heartbeat age on `/jobs`; a genuinely hung one is reaped to
  `interrupted` after the timeout, and all in-flight jobs are interrupted on a frontend restart.
- **Tasks not draining** — confirm the dispatcher is up, workers are connected (`/workers/<service>`),
  and the service is activated on the corpus.
- Known limitations and in-flight hardening are tracked in
  [`docs/KNOWN_ISSUES.md`](docs/KNOWN_ISSUES.md).
