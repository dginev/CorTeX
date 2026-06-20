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

## Tutorial — your first run, end to end

Take one corpus through one service, from a blank database to a saved historical run (~15 min on a
local box). The walkthrough is the **web** path; the CLI/agent twin of each step is noted in
parentheses. Run every binary **from the repo root**.

### Initialize a local install (once)

```bash
# 0. Create an EMPTY Postgres database and point cortex at it — the one bit of config you set.
#    cortex reads DATABASE_URL with highest precedence (from the working-tree .env or the shell);
#    `init` migrates THIS database, so it must exist first. Full role/password setup → INSTALL.md.
createdb cortex                                                  # local peer auth; or: sudo -u postgres createdb -O "$USER" cortex
echo "DATABASE_URL=postgres:///cortex" >> .env                  # or postgres://USER:PASSWORD@localhost/cortex

cargo run --bin cortex -- init                                   # DB: embedded migrations + scaffold cortex.toml (put the DB on NVMe, not /data)
cargo run --bin cortex -- set-admin-token --generate --owner you # Auth: mint the first admin token (printed once → recorded in the audit log)
cargo run --bin cortex -- doctor                                 # verify  => healthy
cargo run --bin frontend                                         # the web app you sign into (default 127.0.0.1:8000)
```

`cargo run --bin cortex -- <cmd>` builds-and-runs from source (always current); add `--release` for a
faster binary on the long-running `frontend`/`dispatcher`. Put `--` *before* any flags (e.g.
`cargo run --bin cortex -- set-admin-token --generate …`) so cargo passes them to the program, not
itself. A real deployment adds a reverse proxy / Anubis + systemd units — see §2 and
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md); none of that is needed locally.

**Once `set-admin-token` succeeds, the entire rest of this walkthrough is doable in the browser** —
creating the corpus, defining the service, registering it, watching the run fill in, and saving the
snapshot are all admin screens (and the agent API mirrors each — noted in parens below). The only
pieces that stay on the command line are the **dispatcher + worker** in step 5 (the conversion engine,
which CorTeX does not start for you).

### Walk the run

1. **Sign in.** Open `http://127.0.0.1:8000/admin/login` and paste the token. You land on **`/admin`**,
   the command center. *(Agents/CLI carry the token as `X-Cortex-Token` / config instead.)*
2. **Create a corpus.** `/admin` → **Add a corpus** (`/corpora/new`): give it a name and the documents'
   path under `/data`. Import runs as a background job — watch it on **`/jobs`**.
   *(`cortex import <name> <path>`.)*
3. **Create a service.** `/admin` → **Add a service** (`/services/new`): a name + input/output formats,
   e.g. `tex_to_html`, tex → html.
   *(`cortex create-service tex_to_html --inputformat tex --outputformat html`.)*
4. **Register the service on the corpus.** **`/corpus/<name>`** → **Register a service** → pick it →
   **Register**. This queues one `TODO` task per document. *(`cortex activate <name> tex_to_html`.)*
5. **Start the dispatcher + a worker.** In two more terminals:
   ```bash
   cargo run --bin dispatcher                  # leases TODO tasks; ventilator :51695 / sink :51696
   cargo run --example tex_to_html_worker       # the sample worker — needs latexmlc on PATH
   ```
   The worker connects to the dispatcher and starts pulling tasks. Watch the fleet at
   **`/workers/tex_to_html`** (dispatched / returned tallies + liveness).
6. **Watch the run complete.** The dispatcher leases `TODO` → streams each source to the worker →
   ingests the result archive → records a status. The report **`/corpus/<name>/tex_to_html`** fills in
   (TODO drains into no_problem / warning / error) and **`/runs/<name>/tex_to_html`** shows the live
   run. It's done when TODO reaches 0.
7. **Save a snapshot.** On the report page click **Save snapshot** (enabled once nothing is in
   progress) — it freezes the per-task statuses into history. The run now appears on **History**
   (`/history/<name>/tex_to_html`, plotted) and in **`/admin/runs`**.
   *(`cortex snapshot <name> tex_to_html`.)*
8. **Shut down gracefully.** `Ctrl-C` the worker, then the dispatcher. A task left leased mid-flight is
   recovered (`Queued` → `TODO`) on the next dispatcher start, so nothing is lost either way.

That's the whole loop: **install → corpus → service → register → run → snapshot → history.** Scale it by
starting more workers (step 5); reprocess a filtered slice with **Rerun** and quantify the effect with
the run-to-run diff ([§11](#11-managing-historical-runs)).

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

# 2. Create an EMPTY database and point cortex at it (init migrates THIS db, so it must exist first)
createdb cortex                                              # local peer auth; or: sudo -u postgres createdb -O "$USER" cortex
echo "DATABASE_URL=postgres:///cortex" >> .env               # or postgres://USER:PASSWORD@localhost/cortex

# 3. Initialize: runs embedded migrations + scaffolds cortex.toml if missing
cargo run --bin cortex -- init

# 4. Create the first admin token (printed once; attributed to an owner in the audit log)
cargo run --bin cortex -- set-admin-token --generate --owner alice

# 5. Verify the installation is healthy
cargo run --bin cortex -- doctor

# 6. Import your first corpus (registers it + creates one import task per document)
cargo run --bin cortex -- import arxmliv /data/arxmliv        # add --complex for multi-file documents
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
  `set-admin-token`; presented as an **`X-Cortex-Token: <token>`** header **or** a `?token=…` query
  param (these are the only two the guard reads — *not* `Authorization`/`Bearer`). Each token maps
  to an **owner** that is threaded into the audit log as the actor. **Revoke** with
  `cortex revoke-token <token>` (or `--owner <name>` to revoke every token a person holds, e.g. when
  they leave); a revoked token stops working immediately (the guard resolves `rerun_tokens` live).

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

1. Start the **dispatcher** — it leases `TODO` tasks, streams sources to workers (ventilator), and
   ingests result archives (sink), persisting each result's status. In production it runs as the
   supervised systemd service **`cortex-dispatcher`** (`sudo systemctl start cortex-dispatcher`;
   one-time setup: point its DB at production in `/etc/cortex/dispatcher.env` and `enable` it — see
   deploy/README.md "Dispatcher cutover"). For an ad-hoc/foreground run use **`scripts/run_dispatcher.sh`**
   (builds the release binary once, runs it with an explicit `DATABASE_URL`, prints the redacted
   target) rather than a bare `cargo run --release --bin dispatcher`, which recompiles every launch
   and crash-loops if `DATABASE_URL` is unset.
2. Start **workers** (the external `pericortex` processes) pointed at this host's ventilator/sink
   ports. They request work, convert, and return result archives. For the production TeX→HTML
   fleet, see [The latexml-oxide worker fleet](#the-latexml-oxide-worker-fleet) below.
3. **Activate a service on a corpus** to create the `TODO` tasks for the fleet to chew through.
4. **Rerun** a slice (clear results back to `TODO`) from a report screen or
   `POST /api/reports/<c>/<s>/rerun?<severity>&<category>&<what>&<description>` to reprocess (e.g. after
   a worker upgrade).

Task status is a signed int (`TODO=0`, `NoProblem=-1`, `Warning=-2`, `Error=-3`, `Fatal=-4`,
`Invalid=-5`, `Blocked<-5`, `Queued>0` while leased). A leased task is marked `Queued` durably; a
crash recovers it (`Queued`→`TODO` on dispatcher restart), and a lease unreturned past its
visibility timeout is re-queued (with a bounded retry budget before it dead-letters).

**Frontend + dispatcher are one deployment.** They're independent over the shared DB (no start
ordering between them), but a frontend with **active tasks** (an open run / a `TODO` backlog) needs a
live dispatcher to drain them — keep both up whenever work is queued. To **upgrade**, rebuild both and
replace the running pair in one step with **`scripts/deploy.sh`** (build both → online migrate →
restart `cortex-frontend` + `cortex-dispatcher` → verify both). `systemctl restart` stops the old
dispatcher before the new binds, so the ZMQ ports hand over cleanly and any in-flight conversions are
re-leased by the reaper (no loss). Use `--frontend-only` for a UI-only change and `--no-migrate` for a
code/template/CSS-only deploy. For **lifecycle** (not upgrade) the two units are grouped under
**`cortex.target`**: `sudo systemctl {start,stop,restart} cortex.target` cycles both and
`systemctl list-dependencies cortex.target` shows the deployment — each unit is still independently
enabled, so they also boot on their own.

### The latexml-oxide worker fleet

The production TeX→HTML worker is latexml-oxide's `cortex_worker` binary (in-process Rust engine,
per-paper panic/OOM/timeout isolation). Workers are **separate processes, not started by CorTeX** —
run them on the worker host(s), pointed at the dispatcher's ports. The robust model is **one
conversion per process** with a per-process RAM cap, so a runaway paper kills only its own worker
(the dispatcher's lease reaper recovers the task) and the worker is respawned.

Run the whole fleet with the **canonical launcher** — `sudo systemctl enable --now cortex-worker`
(its `ExecStart` is `scripts/run_worker.sh`, which sources `/etc/cortex/worker.env`; per-host
template: `deploy/systemd/worker.env.example`). It pins `--workers` (= **physical** cores + 1/8 — the
harness's own CPU default sizes to *logical* cores and over-commits) plus the service/endpoints in a
version-controlled env file and **refuses to start on a missing parameter**, so a fleet never comes up
with the wrong settings by accident. The underlying command it runs (`--harness` spawns and respawns
single-conversion children; the validated memory defaults need no flags):

```bash
cortex_worker --harness \
  --service oxidized-tex-to-html \             # MUST match the registered service name exactly
  --source-address tcp://DISPATCHER_HOST:51695 \
  --sink-address   tcp://DISPATCHER_HOST:51696 \
  --profile ar5iv
# Optional overrides:
#   --workers N               # default: CPU-derived (a deliberate over-commit)
#   --child-mem-limit-mb 5632  # per-child RLIMIT_AS cap — this IS the default (~4 GB RSS); do NOT
#                              # raise to 8192 (a 72-worker sweep at 8192 once spiked to 207 GB -> OOM)
#   --mem-pressure-floor-mb N  # fleet governor floor (default: max(cap, 10% RAM); 0 disables)
```

- **Service name must match exactly.** The ventilator leases tasks for this name and the sink
  re-validates each returned result against the task's service id — a mismatch **silently discards**
  the result. The CorTeX preview registers this service as `oxidized-tex-to-html` (hyphenated).
- **Deliberate over-commit.** The worker count defaults to CPU-derived, *not* sized to
  `workers × cap` — most papers use a fraction of the per-child cap, so sizing to the cap would idle
  the box. Two limits keep it safe: the **per-child cap** below contains a *single* runaway job, and
  the **fleet governor** contains the *aggregate*.
- **Per-child cap.** `--child-mem-limit-mb` (default 5632) enforces a per-child `setrlimit(RLIMIT_AS)`;
  a breach surfaces as a clean `Fatal:oom` + `Status:conversion:3` (exit 137), then a respawn. This
  bounds *address space* (VSZ), not RSS — with the worker's mimalloc allocator the true resident kill
  point sits ~1–1.5 GiB below the number, so the 5632 cap trips at a ~4 GB RSS ceiling (lowered from
  8192 after a 72-worker sweep at 8192 OOMed the box at 207 GB aggregate).
- **Fleet memory-pressure governor.** While system `MemAvailable` drops below `--mem-pressure-floor-mb`
  (auto = `max(cap, 10% RAM)`), the harness SIGTERMs its **largest-RSS** child (task re-leased) and
  pauses respawns until memory recovers — a deliberate, attributable shed in place of a random kernel
  OOM-kill, surviving a rare cluster of concurrently-heavy jobs. For a hard backstop, also run the
  harness inside a memory-limited container or `systemd-run --scope -p MemoryMax=…`; the cgroup,
  governor, and per-child `RLIMIT_AS` compose.
- **Per-paper exit codes** (all recorded as `Status:conversion:3`, task re-leased + worker
  respawned): `124` wall-clock timeout, `137` memory ceiling / alloc failure, `70` repeated panics →
  clean restart.
- **Avoid `--pool-size N`** for production: N conversion threads in one process share one RAM
  ceiling and false-positive the memory guards on every in-flight paper.

Watch the fleet at **`/workers/oxidized-tex-to-html`** (per-worker dispatch/return tallies, in-flight
backlog, liveness age). Full design and tuning: latexml-oxide
`docs/CORTEX_WORKER_HARNESS.md`; the supervision mechanism: pericortex `docs/HARNESS.md`.

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
**`what`** → the individual affected documents and their messages, with a single-entry preview. The
agent mirrors the whole ladder, each rung a typed, paginated DTO:
`GET /api/reports/<corpus>/<service>` (overview) → `…/<severity>` (categories) → `…/<severity>/<category>`
(whats) → `…/<severity>/<category>/<what>` (**the entry list — which documents have this issue**), then
`GET /api/corpus/<corpus>/<service>/document/<name>` for one document's full forensics. So an agent can
go from a macro count straight to the affected papers and into each one, same as a human clicking through.

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

The **agent twin** runs the same export as a background job — `POST
/api/corpora/<corpus>/services/<service>/export-dataset` (token-gated) with a JSON body
`{ "out": "/data/datasets/…", "group_by": "month"|"severity", "severities": ["no_problem", …] }`
(`group_by`/`severities` optional; default `month` + `no_problem,warning,error`). It returns `202` +
a `dataset_export` job handle to poll at `GET /api/jobs/<uuid>` (the manifest is the job result).
`404` for an unknown corpus/service, `422` for a bad `group_by`/severity:

```bash
curl -s -X POST -H "X-Cortex-Token: $TOKEN" -H 'content-type: application/json' \
  localhost:8000/api/corpora/arxmliv/services/tex_to_html/export-dataset \
  -d '{"out":"/data/datasets/arxmliv-2024","group_by":"month","severities":["no_problem","warning","error"]}' | jq .
```

The **web twin** is the **Export dataset** action in a service report's admin row (`/corpus/<c>/<s>`),
or its screen directly at **`/export/<corpus>/<service>`** — the same fields (output path, grouping,
severities), redirecting to the job's live-progress page. So export is available on all three surfaces.

Back up the **Postgres** database (metadata) and the **`/data`** filesystem (document bytes)
separately; delete a corpus only through the app (orphan-free cascade), never a raw `DELETE`.

## 13. Agent API

Every human screen has a 1:1 JSON twin under `/api`. **Reads are open; mutations need a token**
(`X-Cortex-Token: $TOKEN` header or `?token=$TOKEN`, carrying an owner identity into the audit log).
Enumerate the surface at **`/api`**, browse the generated typed contract at **`/api/docs`** (raw spec
at `/api/openapi.json`).

```bash
TOKEN=…   # an admin/API token from `set-admin-token`
curl -s localhost:8000/api | jq .                                  # capability index (every endpoint)
curl -s -H "X-Cortex-Token: $TOKEN" localhost:8000/api/status | jq .  # system snapshot (the dashboard's data: backlog, fleet, jobs, last run)
curl -s localhost:8000/api/jobs?active=true | jq .                 # pending background jobs
curl -s localhost:8000/api/runs?limit=20 | jq .                    # recent runs across the system
```

**Workflow A — forensic drill-down (macro → micro).** Walk the report ladder to find *which* papers
carry an issue, then read one paper's messages — the agent twin of clicking down the report screens.

```bash
C=arxmliv S=tex_to_html
curl -s localhost:8000/api/reports/$C/$S | jq '{total, statuses}'                 # overview: per-status totals
curl -s localhost:8000/api/reports/$C/$S/warning | jq '.categories[:5]'           # severity → top categories
curl -s localhost:8000/api/reports/$C/$S/warning/not_parsed | jq '.whats[:5]'     # category → top `what`s
curl -s "localhost:8000/api/reports/$C/$S/warning/not_parsed/%3EOPEN" | jq '.entries[:5]'  # → affected paper ids
curl -s localhost:8000/api/corpus/$C/$S/document/astro-ph0001001 | jq '{status, message_counts}'  # one paper's forensics
```

**Workflow B — improvement campaign (measure a change's effect on conversion rates).** Capture a
baseline, re-queue a filtered slice for reconversion, watch the run fill in, then diff to quantify
the macro effect — the owner's "how did this development change move the conversion rates" loop.

```bash
# 1. Snapshot the current per-task statuses as the BASELINE ("before") comparison point.
curl -s -X POST -H "X-Cortex-Token: $TOKEN" localhost:8000/api/corpora/$C/services/$S/snapshot | jq .
# 2. Re-queue a slice — opens a NEW historical run and marks the matching tasks TODO for the fleet.
curl -s -X POST -H "X-Cortex-Token: $TOKEN" \
  "localhost:8000/api/reports/$C/$S/rerun?severity=fatal&description=retry+fatals+after+parser+fix" | jq .
# 3. Watch the open run fill in as the dispatcher reconverts (live tallies).
curl -s localhost:8000/api/runs/$C/$S/current | jq '{total, no_problem, error, fatal, in_progress}'
# 4. Once it completes, snapshot AGAIN — the "after" comparison point.
curl -s -X POST -H "X-Cortex-Token: $TOKEN" localhost:8000/api/corpora/$C/services/$S/snapshot | jq .
# 5. Diff the two snapshots to see what moved. The diff compares snapshot TIMESTAMPS (from
#    `available_dates`), NOT run ids; the timestamps carry spaces, so let curl -G url-encode them.
curl -s localhost:8000/api/runs/$C/$S/diff | jq '.available_dates'   # the snapshots you can compare
curl -s -G localhost:8000/api/runs/$C/$S/diff \
  --data-urlencode "previous=<before-timestamp>" \
  --data-urlencode "current=<after-timestamp>" | jq '.transitions'
```

The CLI (§14) mirrors every step one-to-one (`cortex report … --json`, `cortex document`,
`cortex snapshot`, `cortex rerun --yes`, `cortex runs`, `cortex diff`), so the same workflows — including
the snapshot→rerun→**diff** improvement loop — run from a terminal.

## 14. Command-line management (CLI)

The `cortex` binary is the **third surface** — a scriptable twin of the web screens and the agent API,
running against the same database (`DATABASE_URL` / config precedence, §3). Install & ops commands
(`init`, `doctor`, `set-admin-token`, `tune-db`, `export-dataset`) are covered above; the management
commands below mirror the web/agent capabilities one-to-one. Add `--json` to any **read** command to
emit the same shape as the corresponding agent DTO (so a script gets identical numbers to the screen).

**Status — the operational snapshot (CLI twin of the `/admin` live-ops console):**

```bash
cortex status            # pending-task backlog, worker fleet, background jobs, latest run
cortex status --json     # same shape as the /admin/status.json (and /api/status) feed
cortex jobs              # list recent background jobs (imports/reruns/reindex) with health + heartbeat-idle age
cortex jobs --active     # only pending/running jobs; --json mirrors the agent /api/jobs JobDto list
cortex audit             # the accountability log: who did what, when (rerun/import/delete/config…) + outcome
cortex audit --actor bob # filter to one actor; --json mirrors the agent /api/audit AuditDto list
cortex corpora           # list registered corpora (public_id handle, name, doc count) — discover the names other commands take
cortex services          # list the service registry (public_id, name, version, in→out); --json mirrors the agent /api/{corpora,services}
```

`status` shows *what's happening now* (the same numbers as the dashboard + `/metrics`); `doctor`
checks the *install* is healthy. Neither mutates anything.

**Setup — register + import a corpus (the CLI twin of the web "Add a corpus" form / agent `POST
/api/corpora`):**

```bash
cortex create-service tex_to_html --inputformat tex --outputformat html   # 1. define a conversion service
cortex import   arxmliv /data/arxmliv             # 2. register a corpus, one import task per document
cortex activate arxmliv tex_to_html               # 3. queue one conversion task per document
cortex extend   arxmliv                           # later: re-scan the path, import + queue only NEW documents
```

`create-service` *defines* a service in the registry (only the built-in `init`/`import` are seeded, so
a fresh box needs this once per conversion service); `import` registers a corpus and walks its path
(one import task per document); `activate` then queues one conversion task per document for the
service, so the dispatcher can convert them — the full `create-service → import → activate → run the
dispatcher` flow, entirely scriptable. All run synchronously to completion (the web/agent run them as
background jobs) and print the count. Pre-flighted like the agent: a name clash, a non-directory path,
an already-activated pair, or an infrastructure service fails fast (exit 1) without side effects —
re-activating never wipes results (use `rerun` to re-process).

**Read — the report ladder, scriptable:**

```bash
cortex report   arxmliv tex_to_html             # service overview: valid-task total + per-status counts/shares
# …then drill the same ladder the web/agent report screens expose (rollup-backed, fast):
cortex report   arxmliv tex_to_html --severity warning                         # category breakdown
cortex report   arxmliv tex_to_html --severity warning --category not_parsed   # what breakdown
cortex report   arxmliv tex_to_html --severity warning --category not_parsed --what '>OPEN'  # affected docs (paper ids → feed `document`)
cortex runs     arxmliv tex_to_html             # run history: per-severity tallies + run-over-run delta vs the previous run (live for the open run)
cortex diff     arxmliv tex_to_html             # run-diff: the (previous → current) status-transition matrix between two snapshots (latest pair by default; --previous/--current to pick)
cortex diff     arxmliv tex_to_html --tasks --previous-status warning --current-status no_problem  # drill: which individual entries made that transition (paginated --offset/--limit)
cortex document arxmliv tex_to_html 2105.13573  # per-article forensics: status + every worker-log message
```

The drill rungs page with `--offset`/`--limit` (default 100, capped 1000) and emit the matching
agent DTO under `--json`, so a script can walk overview → severity → category → `what` → affected
paper ids → `cortex document <id>` — the same path an agent walks over `/api/reports/...`.

**Mutations — consequential, so dry-run by default; pass `--yes` to execute:**

```bash
cortex rerun          arxmliv tex_to_html --severity error           # re-queue a filtered slice (→ TODO) for reconversion
cortex deactivate     arxmliv tex_to_html                            # retire a service from a corpus (inverse of activate; deletes the pair's tasks+logs)
cortex sandbox        arxmliv err-set --service tex_to_html --status error  # carve a sandbox (task --status &/or --message-severity, intersected)
cortex delete-corpus  old-sandbox                                    # orphan-free cascade delete (run tallies survive)
cortex delete-service old_svc                                        # delete a service definition + ALL its work across every corpus (inverse of create-service)
```

Without `--yes`, a mutation prints exactly what it *would* do (the matched scope / blast radius) and
changes nothing — a safe preview. `--owner <name>` attributes the action in the audit log (default
`admin`). These are the same operations as the web forms and `POST /api/…` endpoints, on one shared
backend, so all three surfaces see the same live + historical state.

**Snapshot — capture a baseline (append-only, runs directly since it's non-destructive):**

```bash
cortex snapshot arxmliv tex_to_html    # freeze current per-task statuses into historical_tasks
```

**Run control — pause/resume a run (status-only, reversible, runs directly):**

```bash
cortex pause  arxmliv tex_to_html    # block every in-progress task (status ≥ 0) so the dispatcher stops leasing this pair
cortex resume arxmliv tex_to_html    # return the blocked tasks to TODO so the dispatcher picks them up again
```

These are the CLI twins of the report screen's **Pause run** / **Resume run** buttons and the agent
`POST /api/reports/<c>/<s>/{pause,resume}` — block in-progress tasks, then restore them, no data lost.

Take a snapshot before a rerun campaign, then compare the run's effect with `cortex runs` (the
run-over-run deltas), `cortex diff` (the snapshot-to-snapshot transition matrix), or the run-task-diff
screens. The web "save snapshot" button and the agent
`POST /api/corpora/<corpus>/services/<service>/snapshot` do the same thing; history is **append-only
over the API** (snapshots are never deleted/modified there — pruning old snapshots is a human-admin
operation, `/admin/retention`).

## 15. Troubleshooting

- **`cortex doctor`** first — it pinpoints DB/migration/seed/token problems.
- **Frontend won't start / wrong DB** — check the CWD (run from repo root) and the config precedence
  (§3); `DATABASE_URL=… cargo run --bin frontend` overrides at runtime.
- **A job is stuck `running`** — check its heartbeat age on `/jobs`; a genuinely hung one is reaped to
  `interrupted` after the timeout, and all in-flight jobs are interrupted on a frontend restart.
- **Tasks not draining** — confirm the dispatcher is up, workers are connected (`/workers/<service>`),
  and the service is activated on the corpus.
- **Dispatcher crash-loops on `Address already in use`** — another dispatcher already holds
  `:51695`/`:51696`. With the systemd unit this can't happen on `restart` (systemd stops the old
  first); the usual cause is a *manually-launched* dispatcher (e.g. a stray `scripts/run_dispatcher.sh`)
  left running. Find + stop the holder — `sudo ss -ltnp | grep -E ':5169[56]'` then `kill <pid>` (use
  `kill -9` if it hangs in graceful shutdown) — then `sudo systemctl start cortex-dispatcher`. Never
  run two dispatchers against the same ports.
- **`systemctl restart cortex-dispatcher` pauses up to ~120s** — its graceful shutdown waits for the
  ventilator to return, which can hang the full `TimeoutStopSec` (120s) when the fleet is **idle** (no
  worker traffic to unblock the lease loop) before systemd SIGKILLs it. In-flight work is re-leased on
  the next start (no loss); the wait is only the finalize-flush window. Restart during/after a fleet
  run for a prompt handover, or expect the pause when idle.
- Known limitations and in-flight hardening are tracked in
  [`docs/KNOWN_ISSUES.md`](docs/KNOWN_ISSUES.md).
