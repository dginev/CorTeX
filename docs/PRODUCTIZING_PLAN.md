# CorTeX Productizing & Hardening Plan

> **Status:** v1 draft (2026-06-13), produced after a full Phase-0 read of the codebase, the
> `migrations/`, the `scripts/`, the `examples/`, and every prior-work branch on `origin`.
> Work branch: **`productize-2026`** (off `master`).
> **North star:** *every administrative action a human can do through a screen, an agent can do
> through a documented API — and both see the same live + historical run state.*

This document is the working plan for turning CorTeX from an admin-only prototype into a
self-installing, agent-first **and** human-first local application. It is organized as:

1. **Current-state map** (the Phase-0 output): data model, task lifecycle, the admin-task
   inventory, prior-work branches, and the corrected dependency audit.
2. **Cross-cutting architecture decisions** that multiple arms depend on.
3. **The arms of work** — each with: goal · current state · target human screen · target agent
   API (1:1 with the screen) · data-model changes · observability hooks · risks · acceptance
   criteria.
4. **Sequencing & milestones.**
5. **Open questions for the owner.**

Read [`CLAUDE.md`](../CLAUDE.md) for the build/run conventions and the load-bearing facts a new
agent needs before touching code.

---

## 1. Current-state map (Phase 0)

> ⚠️ **This section is the original Phase-0 snapshot (2026-06-13) and is kept for history — much of it
> is now stale.** A large fraction of the audited gaps have since been closed (e.g. `time 0.1`, `dotenv`,
> Redis, and `libarchive-sys` are all gone; the box builds + CI is green; the `dependencies` table was
> dropped). For the **current** load-bearing facts read [`CLAUDE.md`](../CLAUDE.md); for the live
> resilience ledger read [`KNOWN_ISSUES.md`](KNOWN_ISSUES.md); for the running increment trail read
> [`archive/PROGRESS_LOG.md`](archive/PROGRESS_LOG.md). Don't treat §1 as the present state.

### 1.1 What CorTeX is, in one paragraph

A distributed corpus-conversion framework. PostgreSQL (via Diesel 2.2) is the **metadata store**:
it holds corpora, services, and one `tasks` row per `(corpus, service, document-entry)`, plus
five severity-partitioned `log_*` tables of LaTeXML-convention messages. The actual document bytes
live on a **shared filesystem** (`/data/...`); a task's `entry` column is the absolute path to its
source archive. A **ZeroMQ dispatcher** (`bin/dispatcher.rs`) leases queued tasks to remote
**workers** (the external `pericortex` crate), streams the source to them, receives result archives
into the sink, parses each result's `cortex.log` into a status + messages, and persists it. A
**Rocket web frontend** (`bin/frontend.rs`) renders read-only Tera reports over that metadata and
exposes a handful of token-gated write actions (rerun, save-snapshot). Everything else
administrative is done by hand — shell scripts, one-off `examples/*.rs` binaries, and raw SQL.

### 1.2 Entity-relationship sketch (verified against `migrations/` + `src/models/`)

```
corpora(id, path UNIQUE-ish, name UNIQUE, complex, description)
   │  (no FK — integer corpus_id only)
   ▼
tasks(id BIGSERIAL, service_id, corpus_id, status INT, entry varchar(200))
   │     UNIQUE(entry, service_id, corpus_id); partial indexes per status (-1..-5)
   │     (no FK to corpora or services)
   ├──────────────► log_infos / log_warnings / log_errors / log_fatals / log_invalids
   │                   (id, task_id, category(50), what(50), details(2000))   — NO FK to tasks
   │                   indexes on (category,what,task_id); fatals/invalids also on task_id
   └──────────────► historical_tasks(id, task_id → tasks(id) ON DELETE CASCADE, status, saved_at)
                       ★ the ONLY foreign key in the whole schema

services(id, name, version REAL, inputformat, outputformat, inputconverter?, complex, description)
   UNIQUE(name, version); seeded rows: id=1 'init', id=2 'import'
   ▲
   │ (integer service_id only, no FK)
worker_metadata(id, service_id, last_dispatched_task_id, last_returned_task_id?,
                total_dispatched, total_returned, first_seen, session_seen?,
                time_last_dispatch, time_last_return?, name)  UNIQUE(name, service_id)

historical_runs(id, service_id, corpus_id, total/invalid/fatal/error/warning/no_problem/in_progress,
                start_time, end_time?, owner, description)   — per (corpus,service) run bookmark

dependencies(master, foundation)  ★ DEAD TABLE — created in 2017, never read or written by any code.
```

**Status encoding** (`src/helpers.rs::TaskStatus`): `TODO=0`, `NoProblem=-1`, `Warning=-2`,
`Error=-3`, `Fatal=-4`, `Invalid=-5`, `Blocked = <-5`, `Queued = >0` (a positive "batch mark").
Severity → log table mapping is enum-derived (`to_table()`). These integers are also hardcoded in
the shell scripts' raw SQL.

**Magic service-id convention** (hardcoded throughout): `1=init` (bootstrap a corpus), `2=import`
(ingest sources), `>2` = real processing services. `Corpus::destroy` and `extend_corpora` both rely
on it.

### 1.3 Task lifecycle (the dispatcher contract)

1. **Import** populates `tasks` with `status=TODO(0)` rows (one per document entry).
2. **Ventilator** (`src/dispatcher/ventilator.rs`, ZMQ `ROUTER`, port **51695**): on a worker's
   request for a service name, batch-fetches up to `queue_size` (800) TODO tasks via
   `fetch_tasks` — a `SELECT … FOR UPDATE` that flips each to `status = positive random mark`
   (the lease). Streams the source archive to the worker in `message_size` (100 KB) frames.
   Tracks dispatched tasks in an in-memory `progress_queue`.
3. **Worker** (external `pericortex`) converts and pushes the result archive to the sink.
4. **Sink** (`src/dispatcher/sink.rs`, ZMQ `PULL`, port **51696**): writes the result zip next to
   the source (`<entry-dir>/<service>.zip`), calls `generate_report` (parse `cortex.log` →
   status + messages), pushes a `TaskReport` onto the shared done-queue.
5. **Finalize** thread (`src/dispatcher/finalize.rs`): every ~1s drains the done-queue via
   `mark_done` — a single transaction that updates `tasks.status` and **deletes + reinserts** all
   `log_*` rows for each task.
6. **Timeout/retry:** `progress_queue` entries expire at `created_at + (retries+1)*3600s`; expired
   tasks are re-queued up to 4 times, then marked `Fatal` (`never_completed_with_retries`).
7. **Restart hygiene:** on ventilator start, `clear_limbo_tasks` resets every `status>0` (leased)
   back to TODO.

**Supervision model:** the dispatcher deliberately uses `panic!` + mutex poisoning as a crude
"die and let a process manager restart me" mechanism (e.g. `mark_done` retries 3× then the finalize
thread panics, poisoning the done-queue mutex so the whole process aborts). The ventilator has a
known, hard-to-reproduce empty-message fragility (08–09/2025) and is re-spawned in a loop by the
manager. This is a key hardening surface.

### 1.4 Existing HTTP surface (`bin/frontend.rs`, Rocket 0.5.1)

- **Read (GET, Tera HTML):** `/` (corpora overview) · `/corpus/<c>` (services) ·
  `/corpus/<c>/<s>[/<severity>[/<category>[/<what>]]]` (drill-down reports) ·
  `/history/<c>/<s>` · `/diff-summary/<c>/<s>` · `/diff-history/<c>/<s>` · `/workers/<s>` ·
  `/preview/<c>/<s>/<entry>` · static (`/public/...`, favicon, robots).
- **Write (POST, JSON):** `/rerun/<c>/<s>[/severity[/category[/what]]]` (4 scoped variants) ·
  `/savetasks/<c>/<s>` · `/entry/<service>/<id>` (download a result archive).
- **Auth:** a shared-secret string typed into a modal, matched against `config.json`'s
  `rerun_tokens: {token → username}` map. No users, no sessions, no per-actor attribution beyond
  that map. `captcha_secret` is in config but **unused** by current code.
- **There is no JSON read API and no machine-readable schema.** Every report is HTML-only.

### 1.5 Admin tasks done out-of-band today (each must become a first-class interface)

| Task | How it's done now | Lives in |
|---|---|---|
| Stand up a new corpus (import) | run `examples/tex_to_html_import.rs` (hardcoded ports 5757/5758, `corpus_id:1`, destroys prior) | example bin |
| Extend a corpus with new months | `examples/extend_corpora.rs` + `Importer::extend_corpus` | example bin |
| Activate a service on a corpus | `examples/register_service.rs` → `Backend::register_service` (**deletes & re-queues all tasks**) | example bin + raw method |
| Create a service | `NewService` insert inside `tex_to_html_import.rs` / `examples/register_service.rs` | example bin |
| Re-index info logs | `examples/record_loading_info.rs` | example bin |
| Disaster-recover statuses from on-disk logs | `examples/recover_log_reports.rs` → `mark_done` | example bin |
| Build a sandbox archive from an ID list | `examples/sandbox_arxiv.rs` | example bin |
| Export an HTML dataset (per month / per severity) | ~~`scripts/bundle-html-dataset*.sh`~~ → `cortex export-dataset` (`backend::export_html_dataset`) | **CLI (landed)** |
| Tune Postgres for scale | hand-run `ALTER TABLE … autovacuum …` from `INSTALL.md` | manual SQL |
| Configure everything | hand-edit `config.default.json`, `.env`, `Rocket.toml` | manual |

(Full per-item breakdown — inputs, called methods, hardcoded assumptions, target interface — is in
the working notes; condensed into the arms below.)

### 1.6 Prior-work branches (git `origin`)

- **`origin/admin-ui`** — **UNMERGED, absent from `master`, 32 ahead / 249 behind, last touched
  2020-10, built on Rocket 0.4 + diesel 1.4.** A *prior, unfinished attempt at exactly this
  sprint's goals.* Adds: `users` + `user_permissions(owner/developer/viewer per corpus,service)` +
  `user_actions` (audit log) tables and models; Google-OAuth sign-in (`verify_oauth`); an admin
  dashboard with **Add Corpus / Add Service** write forms (`/dashboard`, `/dashboard_task/...`);
  server-stats reports (`sysinfo`); a `daemons(pid,name)` table + `Backend::ensure_daemon` so the
  frontend **supervises** dispatcher/worker processes (`bin/cache_worker.rs`, `bin/init_worker.rs`);
  and a `corpora.import_extension` column. **Too old to merge mechanically (two framework majors
  behind), but the authoritative design reference** for arms 7 (process supervision) and 9
  (identity/permissions/audit). Its API is OAuth+form/server-rendered, **not** agent-first JSON — so
  the agent API is greenfield.
- **`origin/historical-tasks`** — already on `master` (run-to-run task diff/regression: the
  `historical_tasks` table, `diff-summary`/`diff-history` screens). Don't redo.
- **`origin/diesel-2.2`** — already incorporated (master is on diesel 2.2.10 + nightly).
- **`origin/recover-log-reports`**, **`origin/sandbox-recoveries`** — ops/ventilator reliability
  fixes; superseded by `master` (which has the evolved versions). The `examples/recover_log_reports.rs`
  recovery script is on master.

Net: only `admin-ui` carries unmerged, productization-aligned work, and it is reference-only.

### 1.7 Dependency audit (corrected against the live tree)

Confirmed by reading `Cargo.toml` + grepping `src/`:

- 🔴 **`time = "0.1.4"` (RUSTSEC-2020-0071)** — **actively used in ~10 files** via `time::get_time()`
  / `time::now().rfc822()` (dispatcher server/sink/ventilator, `frontend/concerns.rs`,
  `frontend/cached/task_report.rs`, several examples). `chrono` is already a dep. Port all uses to
  `chrono` and drop `time 0.1`. Real, spread-out work — not a one-liner.
- 🔴 **`dotenv` + `dotenv_codegen` (RUSTSEC-2021-0141, unmaintained)** — and worse, the `dotenv!`
  **macro bakes `DATABASE_URL`/`TEST_DATABASE_URL` into the binary at compile time**
  (`src/backend.rs`: `pub const DEFAULT_DB_ADDRESS: &str = dotenv!("DATABASE_URL")`). **This is a
  productization blocker**: you cannot ship a binary and point it at a different DB without
  recompiling. Replacing this is folded into Arm 1 (runtime config), not a mere crate swap.
- ⚠️ **`redis = "1.2.2"` — NOT vestigial.** It is a **hard runtime dependency of the frontend**:
  `src/frontend/cached/{task_report,worker}.rs` cache report JSON in Redis and the `cache_worker`
  thread `.expect()`s a connection to `redis://127.0.0.1/` at boot (panics if Redis is down). The
  handoff's "is Redis still used?" lead resolves to **yes** — see Arm 11 (make it optional/embedded,
  don't silently require an extra daemon for self-install).
- `lazy_static` (6 files) → std `LazyLock` (we're on nightly; trivially stable). `rand 0.8` → `0.9`.
  `zmq 0.10` — newest, thin C binding; **keep, pin, watch** (don't rewrite the transport).
- **Git deps** `pericortex` + `libarchive-sys` are pinned by branch; pin to exact rev for
  reproducible builds (ties to Arm 10 provenance).
- **Build status today:** `libzmq/libarchive/libpq/libsodium` are **not installed** and Postgres
  isn't present — **the project does not currently build on this box.** (Arm 0.)
- **CI is stale/broken:** `.github/workflows/CI.yml` installs `diesel_cli --vers 1.1.2` (diesel **1.x**
  CLI against a diesel **2.2** project) and uses archived `actions-rs/*` + `actions/checkout@v2`.
- `~113` `unwrap()/expect()/panic!` sites in `src/`+`bin/`; many on DB results inside request
  handlers (Arm 4).
- **Dead code:** `src/backend/make_history.rs` (empty stub, **not even declared as a module**) and
  `src/dispatcher/metadata.rs` (`register_event` no-op, declared but unused). The `dependencies`
  table. Remove in Arm 12.

---

## 2. Cross-cutting architecture decisions

These are foundations several arms build on. Decide them early (some are Open Questions, §5).

- **D1 — One CLI binary, `cortex`, is the spine.** A new `clap`-derive binary becomes simultaneously
  the **self-install entry point** (`cortex init`), the **admin tool** (replacing every
  `examples/*.rs`), and a clean **agent surface** (agents shell out, or hit the HTTP API). The
  existing `frontend` and `dispatcher` binaries remain but gain a `cortex serve` / `cortex dispatch`
  front door. *Reduces scope: the example bins, the shell scripts, and the "go run this Rust file"
  workflows all collapse into subcommands.*
- **D2 — Layered runtime config via `figment`** (already vendored by Rocket 0.5). One typed
  `CortexConfig` (DB URL, ZMQ ports, corpus data root, redis URL/optional, auth mode, bind addrs)
  sourced from defaults → `cortex.toml` → env → CLI flags, validated at startup. Kills the compile-
  time `dotenv!` baking and the CWD-relative `config.json`/`Rocket.toml`/`templates/` coupling.
- **D3 — Embedded migrations via `diesel_migrations` (`embed_migrations!`).** `cortex init`
  self-migrates with **no `diesel_cli` on the host**. Direct self-install win; also fixes the CI
  diesel-version skew.
- **D4 — A real connection pool (`r2d2` + `diesel`'s `r2d2` feature).** Today **every** Rocket
  handler calls `Backend::default()` → a fresh `PgConnection` per request, and `WorkerMetadata`
  spawns **a new thread + new connection per ZMQ transaction**. Introduce a pooled `Backend` and a
  Rocket-managed pool. Foundational for any real load.
- **D5 — Typed errors: `thiserror` (library) + `anyhow` (binaries).** Replace string/`panic!`
  errors so request handlers degrade gracefully instead of 500-ing or aborting. Keep the
  dispatcher's *intentional* panic-to-restart semantics, but make them explicit and logged.
- **D6 — Structured observability from day one: `tracing` (+ `tracing-subscriber`,
  `tracing-appender`) and `metrics` (+ `metrics-exporter-prometheus`).** Every admin action and
  task-lifecycle transition emits a span/event; `/metrics` exposes queue depth, throughput,
  failures. This is the substrate that makes "live + historical, transparent to agents and humans"
  cheap (Arm 8).
- **D7 — Machine-readable API for free: `utoipa` (OpenAPI) + `schemars` (JSON Schema).** Derive the
  spec from the handlers so the agent surface is self-documenting. Serve a `/openapi.json` +
  `/rapidoc`. *The single biggest scope cut for the agent-first goal.*
- **D8 — Stable external handles (`uuid`).** Give corpora/services/runs a UUID alongside the serial
  PK, so the API and public read-only views never expose or depend on guessable serial ids. Additive
  columns, not a PK change.
- **D9 — The dispatcher stays the backbone.** Do **not** add a competing job/scheduler framework.
  Extend the ZMQ ventilator/sink; surface its lifecycle; harden its failure modes.
- **D10 — Identity is a first-class column, not a token map.** Replace `rerun_tokens` with a
  `users` + API-token model (harvest the `admin-ui` schema), and thread an **actor** (human or
  agent) through every write so `historical_runs.owner` and a new audit log are always truthful.
- **D11 — Lightweight UI: server-rendered HTML + CSS affordances, no new JS frameworks.** The admin
  screens are plain Tera-rendered HTML whose actions are native `<form>` POSTs to the
  symmetry-contract endpoints, using HTML/CSS affordances (`<form>`, `<details>`/`<summary>`,
  `<dialog>`, links, CSS) for interaction. **Light jQuery** (already in the tree alongside Bootstrap)
  is acceptable for optional polish — e.g. the existing AJAX rerun/preview. We add **no SPA / JS
  framework** (no React/Vue/Svelte/Alpine). Every admin action must work without JavaScript via a
  form POST; JS is progressive enhancement only. This keeps both the admin app and the public
  read-only view (Arm 13) cheap to host, audit, and maintain, and dovetails with the symmetry
  contract (the HTML rendering is just forms over the same controller).
  - **HTMX exception:** HTMX is *not used by default*, but — being progressive-enhancement-shaped
    rather than a SPA framework — it **may** be adopted for a specific interaction **only** if a hard
    constraint makes HTML+CSS+light-jQuery genuinely unworkable, and **only** with explicit
    justification and owner sign-off. No such hard constraint is identified today: the closest
    candidates (live import/run **progress**, the Observatory **live feed**) are covered by light
    jQuery polling the symmetry-contract JSON endpoints. If one emerges, flag it for approval.

**Convention for every arm (the symmetry contract):** a screen and its agent API are **one
controller** returning **one shared DTO** — HTML via Tera for `Accept: text/html`, JSON (schema'd)
otherwise. We do not build screens and APIs separately; we build one capability and render it two
ways. This is how we *structurally guarantee* the north star instead of hoping for it.

---

## 3. The arms of work

Arms are grouped: **Foundation (0–4)** unblock everything; **Capability (5–10)** are the management
surfaces; **Quality (11–13)** harden and ship. Within each arm, the agent API is listed 1:1 with the
screen. Status fields are point-in-time as of this draft.

> Legend for each arm: **Goal · Current · Screen · Agent API · Data model · Observability ·
> Risks · Acceptance.**

### Arm 0 — Build & dev-environment bring-up  *(prerequisite)*
- **Goal:** Get CorTeX building, testing, and running on this box; make the toolchain reproducible.
- **Current:** Nightly Rust works; `libzmq/libarchive/libpq/libsodium` absent; Postgres + diesel_cli
  absent; git deps pinned by branch; CI references diesel 1.x. Project does not build.
- **Screen:** n/a (dev concern) — but its *output* feeds the `cortex doctor` screen in Arm 2.
- **Agent API:** n/a (CLI/CI).
- **Data model:** none.
- **Observability:** none yet.
- **Risks:** Postgres must live on **NVMe**, never `/data` (QLC RAID6 is wrong for an OLTP DB —
  established lab fact). System-dep install is a privileged action — confirm before running.
- **Acceptance:** `cargo build` and `cargo test` green on `cortex`; Postgres running on NVMe; git
  deps pinned to exact revs in `Cargo.lock`; a documented one-shot dev-setup script.

### Arm 1 — Runtime config & addressability  *(foundation; unblocks self-install)*
- **Goal:** Nothing about a deployment is baked at compile time or tied to the CWD.
- **Current:** `dotenv!` bakes `DATABASE_URL` into the binary; `load_config()` opens `config.json`
  from CWD and panics if missing; `Rocket.toml`, `templates/`, `public/` are CWD-relative; ports are
  hardcoded constants (51695/51696 in prod, 5757/5758 in an example).
- **Screen:** **Settings** page — view effective config (secrets masked), per-source provenance
  (default/file/env/flag), validation status; edit + write-back to `cortex.toml`.
- **Agent API:** `GET /api/config` (effective, masked) · `GET /api/config/schema` (JSON Schema) ·
  `PATCH /api/config` (validated write). Mirrors the Settings page exactly.
- **Data model:** none (config is file/env), but defines `CortexConfig` (D2) consumed everywhere.
- **Observability:** emit a `config.loaded` event with source provenance; a `/healthz` that
  validates config + DB connectivity.
- **Risks:** secret handling (don't echo DB password / tokens); embedding template/asset dirs vs.
  shipping them next to the binary.
- **Acceptance:** binary runs from any CWD; DB URL/ports/data-root/redis all set at runtime;
  `dotenv`/`dotenv_codegen` removed; one typed config struct; tests cover precedence + validation.

### Arm 2 — Self-install / bootstrap  *(foundation)*
- **Goal:** One command stands up CorTeX; a second diagnoses/repairs a partial install.
- **Current:** `INSTALL.md` is manual ops (install PG by hand, create DB/roles by hand, run diesel
  migrations by hand, hand-tune autovacuum). No self-install.
- **Screen:** **First-run wizard** (detect PG / create DB+role / run migrations / write config /
  seed `init`+`import` services / health summary) and a **`cortex doctor`** status panel.
- **Agent API:** `POST /api/bootstrap` (idempotent; body = provisioning options) · `GET /api/health`
  (structured: DB reachable, migrations up-to-date, redis optional-status, dispatcher reachable,
  data-root writable). Same JSON the wizard renders from.
- **Data model:** embedded migrations (D3); a `schema_migrations`-aware "is this DB current?" check;
  optionally apply the `INSTALL.md` autovacuum tuning as a managed migration.
- **Observability:** every bootstrap step is a `tracing` span with success/failure + remediation
  hint; doctor output is the same structured payload live and historical.
- **Risks:** must be **idempotent** and safe to re-run on a live DB; never drop data on "repair".
- **Acceptance:** `cortex init` on a clean box → working DB + config + seeded services, no
  `diesel_cli` required; `cortex doctor` detects and offers to fix a missing migration / missing
  service / unwritable data-root.

### Arm 3 — Database maturity  *(foundation)*
- **Goal:** Pooled, referentially-sane, API-safe persistence.
- **Current:** new connection per request and per ZMQ txn; no FKs except `historical_tasks→tasks`;
  `Corpus::destroy` orphans `log_*` rows; serial PKs exposed; `dependencies` table dead.
- **Screen:** none of its own (infra) — surfaces via Arm 8 metrics (pool utilization) and Arm 2
  doctor (integrity checks).
- **Agent API:** none of its own; enables every other API.
- **Data model:** add `r2d2` pool (D4); add FKs `tasks.corpus_id→corpora`, `tasks.service_id→services`,
  `log_*.task_id→tasks ON DELETE CASCADE`, `worker_metadata.service_id→services`,
  `historical_runs.{corpus,service}_id→…` (with a data-cleanup migration for existing orphans);
  add `uuid` columns (D8); decide the `dependencies` table's fate (drop vs. revive — see Arm 6).
- **Observability:** `metrics` gauges for pool size/idle/wait; counters for txn retries/failures.
- **Risks:** adding FKs to a multi-million-row prod DB needs a careful, online migration + prior
  orphan cleanup; `WorkerMetadata`'s per-event thread+conn must move to the pool without
  re-introducing blocking on the dispatcher hot path.
- **Acceptance:** all DB access goes through the pool; FKs enforced; deleting a corpus leaves no
  orphans; UUIDs present and used by the API; load test shows stable connection count.

### Arm 4 — Error handling & robustness  *(foundation)*
- **Goal:** Bad input or a bad query degrades gracefully and is observable, never a silent panic.
- **Current:** ~113 `unwrap/expect/panic!`; report SQL `.unwrap()`s DB results inside handlers;
  `load_config` panics; dispatcher uses panic-to-restart deliberately.
- **Screen:** consistent error pages; a surfaced "last N errors" panel (ties to Arm 8).
- **Agent API:** typed JSON error envelope (`{error: {code, message, hint}}`) with correct HTTP
  status codes across all endpoints; documented in the OpenAPI spec.
- **Data model:** none (optionally an `app_errors` ring buffer for the panel).
- **Observability:** every handled error is a `tracing` event with a stable `code`; panics in the
  dispatcher are logged with context before the intentional abort.
- **Risks:** preserve the dispatcher's *deliberate* fail-fast semantics — convert ad-hoc panics to
  typed errors **only** where recovery is correct, not where restart is the design.
- **Acceptance:** no `unwrap/expect` in request paths; `thiserror`/`anyhow` adopted; fuzz/property
  tests for `parse_log` and the report SQL builders don't panic on adversarial input.

### Arm 5 — Corpus management
- **Goal:** Full corpus lifecycle as screens + API: create/import, inspect, extend, re-import,
  delete, sandbox — with progress + provenance.
- **Current:** `examples/tex_to_html_import.rs` (import, destructive, hardcoded ports/ids),
  `extend_corpora.rs` (extend), `sandbox_arxiv.rs` (build subset), `Importer` + `Corpus::destroy`.
  All CLI-only, all assume arXiv directory topology and `/data` paths.
- **Screen:** **Corpora** index (list, health, sizes) → **Corpus detail** (services, counts,
  provenance) with actions: *Import new corpus*, *Extend with new entries*, *Build sandbox from ID
  list*, *Delete* (guarded). Import/extend show live progress.
- **Agent API:** `GET /api/corpora` · `POST /api/corpora` (import: `{name, path, complex,
  description}`) · `GET /api/corpora/{uuid}` · `POST /api/corpora/{uuid}/extend` ·
  `POST /api/corpora/{uuid}/sandbox` (ID list → downloadable archive) · `DELETE /api/corpora/{uuid}`.
  Long-running ones return a **run/job handle** (Arm 8) the caller polls — same handle the screen's
  progress bar watches.
- **Data model:** promote `Importer` into a library service callable from API + CLI; add
  `corpora.import_extension` (harvest from `admin-ui`) and a provenance record (who imported, when,
  source path, counts). Run imports as managed background jobs, not inline threads with hardcoded
  ports.
- **Observability:** import emits per-checkpoint progress events + a final run summary; counts
  (entries found/imported/skipped) become metrics.
- **Risks:** import walks/unpacks huge trees and **deletes source `.gz`** as it goes — must be
  crash-safe and idempotent; deletion is destructive (confirm + soft-delete option).
- **Acceptance:** a corpus can be imported, extended, sandboxed, and deleted entirely from the UI
  and the API, with live progress and a provenance trail; no example binary needed.

### Arm 6 — Service management
- **Goal:** Define/configure services, their dependency graph, version pinning, and the pinned
  Docker-worker-image binding — as screens + API.
- **Current:** services created by ad-hoc `NewService` inserts in example bins;
  `register_service` (**deletes & re-queues all tasks** for the pair) and `extend_service` are
  CLI-only; the `dependencies` table exists but is **never used** (the README's "automatic
  dependency management" is an unfulfilled TODO); `inputconverter` is a loose string, not a FK.
- **Screen:** **Services** index → **Service detail** (formats, version, inputconverter, description,
  bound worker image) with actions: *Create service*, *Edit*, *Activate on corpus* (destructive
  re-queue — guarded), *Extend on corpus* (additive), *Delete*. A **dependency graph** view.
- **Agent API:** `GET/POST /api/services` · `GET/PATCH/DELETE /api/services/{uuid}` ·
  `POST /api/corpora/{c}/services/{s}/activate` · `.../extend` · `GET/PUT /api/services/{uuid}/dependencies`
  · `PUT /api/services/{uuid}/worker-image` (pin `repo@sha256:…`).
- **Data model:** decide `dependencies`' fate — **revive it with FKs** (`master/foundation →
  services`) to deliver the long-promised dependency management, *or* drop it; add a
  `service_worker_images` binding (image ref + digest) feeding Arm 10 provenance and the lab's
  pinned-Docker-worker plan; make `inputconverter` a real reference.
- **Observability:** activation/extension emit run events (they already open `historical_runs`);
  dependency-graph changes are audited.
- **Risks:** `register_service` is **silently destructive** (wipes tasks + opens a new run) — the UI
  must make that explicit and require confirmation; version pinning interacts with reproducibility
  (Arm 10).
- **Acceptance:** services and their activations, versions, dependencies, and worker-image bindings
  are fully managed via UI + API; no raw `services`/`dependencies` edits; activating a service is an
  explicit, audited, confirmable action.

### Arm 7 — Service-run orchestration
- **Goal:** Start/stop/monitor runs; rerun-failed-only with severity/category/what filters;
  backpressure; surface and control the dispatcher's task lifecycle.
- **Current:** rerun exists as 4 token-gated POST routes (`mark_rerun` → opens a `historical_runs`
  bookmark, blocks→clears logs→sets TODO); the dispatcher is started by hand (`bin/dispatcher.rs`)
  and self-restarts the ventilator on failure; no UI to start/stop the dispatcher or workers; the
  `admin-ui` branch's `ensure_daemon`/`daemons` table prototypes frontend-side process supervision.
- **Screen:** **Runs** dashboard — active run(s) per `(corpus,service)` with live counts &
  throughput; **Start run / Rerun** (filter by severity/category/what, with the existing
  `no_messages` special case); **Stop/pause**; **Dispatcher & workers** control panel (status,
  start/stop, queue depth, backpressure knobs: `queue_size`, `message_size`, retry policy).
- **Agent API:** `POST /api/corpora/{c}/services/{s}/runs` (start/rerun with filter body) ·
  `GET /api/runs` / `GET /api/runs/{uuid}` (live status) · `POST /api/runs/{uuid}/stop` ·
  `GET /api/dispatcher` · `POST /api/dispatcher/{start,stop,reload}` ·
  `GET /api/workers?service=` (already have the data via `worker_metadata`).
- **Data model:** make `historical_runs` the run handle (add `uuid`, `status`, `actor`); optionally
  adopt the `admin-ui` `daemons(pid,name)` table for supervised processes; expose `queue_size`/
  retry as runtime config (Arm 1) instead of constants in `bin/dispatcher.rs`.
- **Observability:** **this is half the north star** — every dispatch/return/retry/timeout becomes a
  `tracing` event + metric (queue depth, in-flight, completion rate, retry/fatal counts); the live
  Runs view and the agent `GET /api/runs/{uuid}` read the *same* status surface.
- **Risks:** controlling OS processes from the web app is a security-sensitive, `admin-ui`-style
  capability — gate hard (Arm 9) and prefer a supervised-process model over `kill(pid)`; don't
  destabilize the dispatcher's deliberate restart semantics (D9).
- **Acceptance:** a run can be started, filtered, monitored live, and stopped from UI + API; the
  dispatcher and worker fleet are observable and controllable; backpressure is tunable at runtime;
  humans and agents see identical run state.

### Arm 8 — Observability (the reason for all this)
- **Goal:** One unified, structured, pollable surface for **live** and **historical** runs, identical
  for humans and agents, with per-run and per-task drill-down and export.
- **Current:** read-only HTML reports; `historical_runs` (per-run severity tallies) and
  `historical_tasks` (per-task status snapshots, with `diff-summary`/`diff-history` regression
  views); `worker_metadata` liveness; report-time logging via the doomed `time 0.1`; Redis-cached
  report pages. No metrics endpoint, no structured live feed, no JSON of any report.
- **Screen:** **Observatory** — a live run feed (auto-refreshing, replacing the ad-hoc JS refresh
  toggle), historical run archive + regression diffs (keep the existing Vega views), per-run and
  per-task drill-down, and **Export** (CSV/JSON) of any report. A `/metrics` scrape target for
  external dashboards.
- **Agent API:** **every** report screen gets a JSON twin (D-symmetry contract): `GET /api/reports/{c}/{s}[/severity[/category[/what]]]` ·
  `GET /api/history/{c}/{s}` · `GET /api/diff/{c}/{s}` · `GET /api/runs/{uuid}` · `GET /api/tasks/{id}`
  · `GET /metrics` (Prometheus). Pollable, schema'd, paginated.
- **Data model:** none required (reads existing tables); optionally a lightweight live-status cache
  keyed by run uuid; reconcile the Redis cache (Arm 11) so JSON and HTML share one cached source.
- **Observability:** *is* the deliverable — `tracing` spans across the whole task lifecycle and every
  admin action; `metrics` for queue/throughput/failure; consistent run/task ids in every log line so
  an agent can correlate.
- **Risks:** report SQL is already heavy at arXiv scale (hence the Redis cache and autovacuum
  tuning) — adding JSON twins must reuse the cached path, not double the query load.
- **Acceptance:** for any report a human can open, an agent can `GET` the same data as schema'd JSON;
  live run state is pollable and matches the UI exactly; `/metrics` exposes queue depth, throughput,
  and failure counts; reports export to CSV/JSON.

### Arm 9 — Agent-first API + identity
- **Goal:** A documented HTTP/JSON surface mirroring every screen, with agent-vs-human
  identity/attribution so every run and write is traceable to its initiator.
- **Current:** no JSON read API, no OpenAPI/schema; writes gated by a shared `rerun_tokens` string →
  username map; `captcha_secret` unused; no users/sessions; `admin-ui` prototyped
  `users`/`user_permissions`/`user_actions` + Google OAuth but it's unmerged and on Rocket 0.4.
- **Screen:** **Auth & API** — login (human), **API tokens** management (create/revoke per actor,
  scoped), and an **Audit log** of who did what. An embedded **API docs** page (RapiDoc over the
  generated OpenAPI).
- **Agent API:** the whole `/api/**` surface, documented via `utoipa` (`GET /openapi.json`); auth via
  an `X-Cortex-Token` header / `?token=` query for agents (the planned `Authorization: Bearer` was
  simplified to this in the tokens-first implementation) and session cookies for humans;
  `GET /api/me` (current actor); `GET/POST/DELETE /api/tokens`.
- **Data model:** harvest `admin-ui`'s schema — `users`, `api_tokens` (hashed via `argon2`/
  `password-hash`; or JWT via `jsonwebtoken`), `user_permissions(owner/developer/viewer per
  corpus/service)`, and `user_actions` (audit). Thread an **actor** into `historical_runs.owner` and
  every write (D10). Migrate `rerun_tokens` → API tokens with a compat shim.
- **Observability:** every authenticated action is an audit row **and** a `tracing` event with the
  actor id; the audit log is itself a report (Arm 8).
- **Risks:** security-critical — token hashing, scope enforcement, and not regressing the public
  read-only dashboard (which must stay unauthenticated read-only). **Identity is tokens-first
  (decided §5.1)** — human accounts layer on after the token/actor model.
- **Acceptance:** every screen has a documented JSON endpoint in the OpenAPI spec; agents
  authenticate with scoped tokens; every write is attributed to an actor and audited; the public
  dashboard remains read-only without auth.

### Arm 10 — Data management / datasets
- **Goal:** Dataset export/bundling as a first-class, parameterized feature with versioning and
  provenance — results attributable to an exact toolchain.
- **Current:** two near-duplicate shell scripts (`bundle-html-dataset.sh` per-month,
  `bundle-html-dataset-by-severity.sh` per-severity) using raw `psql`, magic status ints, `/data`
  paths, and `tex_to_html.zip` assumptions; `sandbox_arxiv.rs` for source subsets;
  `libarchive-sys` (C dep) for archive I/O.
- **Landed (CLI step, D1 — 2026-06-15):** both scripts are **retired**, collapsed into the
  parameterized `cortex export-dataset <corpus> <service> --out <dir> --group-by month|severity
  [--severity no_problem,warning,error]` subcommand (`backend::export_html_dataset`,
  `src/backend/export.rs`). Pure-Rust `zip` only — **no `psql`/`unzip`/`zip`/`egrep`** shell-outs and
  no `libarchive` C dep; reads existing result archives off the filesystem (sandbox-aware via
  `helpers::result_archive_path`). The `no_problem`/`no-problem` naming is reconciled to the canonical
  `TaskStatus::to_key` spelling. Resumable (an existing archive is skipped). Provenance ships as a
  sidecar `<corpus>-manifest.json` (corpus/service/severities/group-by/per-archive counts/
  `generated_at`/`cortex_version`). **Still outstanding for the full arm:** the web **Datasets**
  screen + `POST /api/corpora/{c}/services/{s}/exports` background-job API + an `exports` DB table +
  exact toolchain/worker-image-digest provenance (the manifest is the lightweight stand-in for now).
- **Screen:** **Datasets** — define an export (corpus, service, severity filter, partition
  by month|severity), run it (background job with progress), browse/download produced archives,
  see each export's provenance (toolchain/service version/worker-image digest, row counts, date).
- **Agent API:** `POST /api/corpora/{c}/services/{s}/exports` (partition + severity options) ·
  `GET /api/exports` / `GET /api/exports/{uuid}` (status + artifacts) · `GET /api/exports/{uuid}/download`.
  Collapses the two shell scripts into one parameterized endpoint.
- **Data model:** an `exports`/`datasets` table (uuid, corpus, service, filters, partition,
  toolchain/version/worker-image provenance, artifact paths, counts, created_by, created_at). Pin
  git deps (Arm 0) so provenance is exact. Consider replacing `libarchive-sys` with pure-Rust
  `zip`/`tar` + `zstd`/`flate2` (+ `walkdir` for traversal, `csv` for tabular export) to drop the C
  dep and gain portable, high-ratio bundles — evaluate against existing archive compatibility.
- **Observability:** export jobs are runs (Arm 8) with progress + provenance; artifact integrity
  hashes recorded.
- **Risks:** large exports are I/O-heavy on the QLC RAID `/data`; the per-month/per-severity naming
  inconsistency in the current scripts (`no_problem` vs `no-problem`) must be reconciled;
  pure-Rust archive migration must preserve downstream consumers' expectations.
- **Acceptance:** a dataset can be defined, exported (either partitioning), versioned, and downloaded
  from UI + API, each carrying exact toolchain/worker provenance; the shell scripts are retired.
  *(Partial: shell scripts retired + export runnable via the CLI with both partitionings and a
  provenance manifest; the UI/API + versioned `exports` table + exact toolchain/worker provenance
  remain.)*

### Arm 11 — Caching & scale hardening
- **Goal:** Make the report cache a help, not a hidden hard dependency; share one cached source
  between HTML and JSON.
- **Current:** frontend **hard-requires Redis at `127.0.0.1`** (the `cache_worker` thread panics at
  boot if it's down); cache keys are corpus/service/severity/category strings; a 2-minute
  invalidation loop recomputes pages on `queued`-count change. Redis is undocumented in self-install.
- **Screen:** cache status in **Settings**/**doctor** (hit rate, last invalidation, backend
  enabled?).
- **Agent API:** `GET /api/cache/status` · `POST /api/cache/invalidate` (scoped).
- **Data model:** none.
- **Observability:** cache hit/miss/invalidation metrics.
- **Risks:** removing Redis entirely changes scaling characteristics at arXiv volume; making it
  *optional* (graceful no-cache fallback — already half-present in `task_report.rs`, fully absent in
  `cache_worker.rs`) is the safer first step.
- **Acceptance:** the frontend boots and serves correctly with Redis absent (degraded, logged);
  when present, JSON and HTML reports share one cached entry; Redis is either documented in
  self-install or made truly optional (Open Question §5).

### Arm 12 — Hardening pass (bugs, races, tests, CI, supply chain)
- **Goal:** Close prototype-grade defects and stop the tree from rotting.
- **Current:** ventilator empty-message fragility (manager re-spawns it in a loop as a workaround);
  panic-driven supervision; orphan-prone deletes (Arm 3); ~113 panic sites (Arm 4); broken CI
  (diesel 1.x CLI, archived actions); dead code (`make_history.rs`, `dispatcher/metadata.rs`,
  `dependencies` table); `unwrap` in report SQL; integration-only tests requiring a live DB +
  `latexmlc`.
- **Screen:** a "known-issues / self-test" surface in **doctor**.
- **Agent API:** `GET /api/health` deep-check (reuses Arm 2).
- **Data model:** migration hygiene — add the missing FKs, write **down** migrations, verify
  reversibility; remove `dependencies` if not revived (Arm 6).
- **Observability:** the empty-message and retry-exhaustion paths must emit clear events, not just
  `eprintln!`.
- **Risks:** reproducing the ventilator race is hard — instrument first (Arm 8), then fix with a
  principled framing protocol rather than the current swap/skip heuristics.
- **Acceptance:** CI green on current toolchain with `cargo-audit` + `cargo-deny` (mirror
  latexml-oxide's `deny.toml`) gating advisories/licenses/duplicate-majors; dead code removed; unit
  tests for `parse_log`, status mapping, and report SQL builders; the ventilator race is either
  reproduced+fixed or fully instrumented with a documented mitigation; `time 0.1`, `dotenv`,
  `lazy_static` gone.

### Arm 13 — Productization basics, public dashboard & deployment
- **Goal:** Real docs, packaging, and the end-state deployment topology — including the public
  read-only web view at **`https://corpora.latexml.rs`** that hosts the ar5iv browsing +
  conversion-report UIs for the wider community.
- **Current:** `INSTALL.md` is now a complete, verified installation (Arm 0 ✅); `MANUAL.md` is now a
  real operator manual (the fresh-box → manage-historical-runs admin journey), no longer `### TODO`;
  `README` says "not ready for off-the-shelf use"; no packaging; deployment is
  hand-run binaries. **CORS fixed (2026-06-15):** `src/frontend/cors.rs` no longer pairs
  `Access-Control-Allow-Origin: *` with `Access-Control-Allow-Credentials: true` — the credentials
  header is dropped. `*` is retained as the *correct* posture for the deliberately-public,
  no-credential read API (agents authorize via the explicit `X-Cortex-Token` header, the admin UI is
  same-origin); an origin *allowlist* was deliberately **not** added — it would only restrict reads of
  public data and break browser-based agent tooling (decision noted in `OPEN_QUESTIONS.md`). The same
  fairing's dead `Content-Security-Policy-Report-Only` header (report-only, on JSON only, reporting to
  a `report-uri` route that doesn't exist → enforcing/collecting nothing) was removed; a real
  *enforcing* CSP on the HTML + ar5iv-preview surface remains the open task (see Risks).
- **Screen:** the **public read-only dashboard at `corpora.latexml.rs`** — the existing overview /
  reports / history / diff / **ar5iv document-preview** screens, served read-only; plus in-app
  first-run docs + contextual help on the internal admin app.
- **Agent API:** the OpenAPI doc (Arm 9) *is* the agent manual + a short "agent quickstart". The
  public host exposes **only** the read-only JSON twins; the write API, admin UIs, token endpoints,
  and `/metrics` stay on the internal (Tailscale / token-authed) surface.
- **Data model:** none. Config (Arm 1) gains `web.public_base_url = "https://corpora.latexml.rs"`,
  a `web.public_readonly` mode (serve only safe GETs), and a **CORS origin allowlist** replacing the
  `*`.
- **Observability:** public-traffic metrics (requests, cache hit rate) feed Arms 8/11; the public
  view is the most cache-sensitive surface (Arm 11b) and the most read-amplified.
- **Risks:** the public boundary is security-critical — it must serve **only** read-only dashboards +
  previews and never the write API, admin UIs, token endpoints, or `/metrics`. ✅ CORS
  misconfiguration (`*` + credentials) fixed by dropping the credentials header (see Current). The
  ar5iv preview renders
  untrusted converted HTML client-side — keep the existing consent/sandbox posture and a tight CSP.
- **Acceptance:** a real `MANUAL.md` (human) + agent quickstart; packaged/installable binaries;
  **`https://corpora.latexml.rs` live**, serving the read-only ar5iv dashboards + previews via a
  Caddy reverse-proxy over Tailscale to the frontend on `cortex`, with write/admin/metrics surfaces
  unreachable from the public host; documented full topology: **dispatcher + frontend on `cortex`,
  Postgres on NVMe, pinned-Docker-image workers across cortex/science-kit/science-pup, external
  workers via Tailscale, public read-only dashboard at `corpora.latexml.rs`.**

### Arm 14 — Resource & performance rationalization
- **Goal:** Make CorTeX's resource use fit a fast-worker reality and stop reports needing a cache.
- **Current:** legacy workers were compute-bound, so the dispatcher/DB/disk were idle. With
  **latexml-oxide (~1 s/task × ~200 workers ≈ 200 tasks/s)** the bottleneck moves to the dispatcher,
  DB, and disk I/O — and per-task overheads dominate: `WorkerMetadata` spawns a thread+connection per
  ZMQ event; `mark_done` delete+reinserts all five `log_*` tables per task; results write to the slow
  `/data` QLC RAID6; reports are O(millions of rows) scans shielded by a Redis cache.
- **Screen / Agent API:** surfaces via Arm 8 metrics (throughput, queue depth, write/commit latency)
  rather than a dedicated screen.
- **Data model:** **incremental report rollup tables** (the headline change — per-`(corpus, service,
  severity, category, what)` counts maintained in the finalize path), generalizing `historical_runs`.
- **Observability hooks:** a measurement spike (instrument dispatcher + DB at 100→200 tasks/s) gates
  the work; throughput/latency become first-class metrics.
- **Risks:** crash-consistency of NVMe staging; not regressing correctness while replacing the blind
  log delete+reinsert; keeping the ZeroMQ transport (don't rewrite it — D9).
- **Acceptance:** reports are O(categories) and always fresh (Redis becomes optional); the per-ZMQ
  -event thread+connection churn is gone; sustained 100→200 tasks/s with latexml-oxide workers without
  the DB or RAID becoming the wall.
- **Detail:** six mini-choices + mini-plans (async I/O, NVMe staging, batch/commit tuning, fast-worker
  re-tune, DB choice = keep Postgres, incremental rollups) in
  [`archive/RESOURCE_RATIONALIZATION.md`](archive/RESOURCE_RATIONALIZATION.md).

### Arm 15 — Experience rationalization (admin UI · CLI · agent API as one surface)
- **Goal:** Generalize the symmetry contract from two surfaces (web HTML + agent JSON) to **three**
  (add the **CLI**), complete across **four magnifications** (macro trends → meso reports → micro
  per-article forensics → management ops), so a human or an agent can answer any question at any zoom
  without keeping local notes. Owner directive (2026-06-15) folding five directions — live ops
  console, design-system polish, smoother workflows, a rich guided CLI, rich agentic workflows — into
  one program.
- **Principle:** one capability = one backend op = one DTO, surfaced through all three consumers;
  **agent-API-first** (each typed endpoint is then rendered by web + CLI for free).
- **Arms:** **A** agent forensic + trend completeness (per-article forensics, macro-trend series,
  service-overview hub) — the keystone; **B** CLI as a first-class surface (management subcommands +
  guided init, ratatui TBD); **C** web cohesion + smoother workflows + rendering the new
  magnifications. The **live ops console** (Arm-A's dashboard facet) has **landed**.
- **Detail + grounded coverage audit + open decisions:**
  [`EXPERIENCE_RATIONALIZATION.md`](EXPERIENCE_RATIONALIZATION.md).

---

## 4. Sequencing & dependency plan (who blocks whom)

Calendar estimates matter less than the blocking structure (this box is the dev *and* prod node).
The **symmetry contract** (§2) reshapes the naive "config + UIs first" order: three capabilities
each split into an *early substrate* and a *late surface*, and the substrate is a foundation, not a
late arm:

- **Observability (Arm 8)** → **8a** `tracing`/`metrics` rails (early) + **8b** the Observatory
  screens (late). Lay the rails before the track, or retrofit instrumentation into every feature.
- **Agent API + identity (Arm 9)** → **9a** tokens + `actor` + content-negotiation + OpenAPI
  derivation (early foundation) + **9b** the per-screen endpoints — which are *not a separate arm*:
  each management screen's JSON twin ships with the screen under the symmetry contract.
- **Cache (Arm 11)** → **11a** make Redis optional (early; self-install needs no extra daemon) +
  **11b** unified HTML+JSON cache (late, with 8b).

**Layer 0 — Foundation bring-up. ✅ DONE** (Arm 0): box builds, Postgres on NVMe, migrations
applied on both DBs, frontend boots + serves (HTTP 200); `INSTALL.md` rewritten and verified
end-to-end.

**Layer 1 — Invisible foundations** (block the *trustworthy* version of everything):
- **Arm 1 (runtime config)** — the deepest root: kills the compile-time `dotenv!` DB URL and the CWD
  coupling, defines `CortexConfig` that ports/data-root/redis-url/auth-mode/bind all read. Home for
  the `dotenv`/`time-0.1` purge. **Do this first** — everything reads it.
- **Arm 4 (typed errors)** — the JSON error envelope every endpoint needs.
- **Arm 8a (tracing/metrics rails)** — so 5/6/7/10 emit signal for free; also a prerequisite for
  *fixing* the ventilator race (instrument, then fix).
- **Arm 3 (pool + FKs + uuid)** — pool for scale under the new endpoints; FKs for *safe delete* in
  the corpus/service UIs (today deletes orphan `log_*` rows).
- **Arm 9a (tokens + actor + OpenAPI plumbing)** — blocks the API half and the attribution of every
  capability below. **Tokens-first** (decided §5.1): a token model + `Bearer` guard + an `actor`
  abstraction; no OAuth on the critical path, human accounts layered on later.
- **CI revival + `cargo-audit`/`cargo-deny`** (from Arm 12) — guard refactors from here on.
- *Internal order:* Arm 1 first; then 4, 8a, 3, 9a are largely orthogonal and parallelizable.

**Layer 2 — The management surfaces** (the "admin UIs hardened"; M1):
- **Pilot: the Settings/Config screen** (Arm 1's own UI: `GET/PATCH /api/config` + page) — the
  lowest-risk surface to prove the symmetry contract before the corpus/service UIs bet on it.
- Then the trio in **data-dependency order: Corpus (5) → Service (6) → Runs (7)** — a service
  activates *on a corpus*; a run executes *a service*. Each ships screen + JSON twin + telemetry +
  `actor`-attribution by construction. Example bins / shell scripts retire as each lands.
- **Self-install (Arm 2)** runs **in parallel** here on the CLI track (needs only Arm 1 + embedded
  migrations), reusing the `clap` scaffolding that also becomes the agent shell-out surface.

**Layer 3 — Transparent + reproducible** (M2):
- **Arm 8b (Observatory)** — consumes 8a's signal + Arm 7's run model: live + historical run state
  as one pollable surface, identical for humans and agents. **Arm 11b (unified cache)** ships with
  it; **Arm 10 (datasets)** consumes Arm 7's jobs + Arm 6's worker-image provenance; the
  **ventilator-race fix** lands here.

**Layer 4 — Ship** (M3): Arm 12 finish (remaining bugs/hygiene) + Arm 13 (real `MANUAL.md`,
packaging, deployment topology, public read-only dashboard — which needs 9a to separate public from
authed surfaces).

**Critical path (the spine):**
`Arm 1 (config) → 9a (tokens + actor + API plumbing) → Arm 7 (runs) → 8b (Observatory) → Arm 13 (ship)`.
Everything else parallelizes off this spine.

| This… | …must precede | …because |
|---|---|---|
| **1 config** | 2, 9a, 11a, 13 | runtime DB/ports/data-root/auth-mode; kills compile-time + CWD coupling |
| **4 errors** | 9a + all endpoints | the JSON error envelope |
| **8a rails** | 5, 6, 7, 10; race-fix | instrument before you build / before you fix |
| **3 pool+FKs** | robust 5/6/7/8b; safe delete | scale + referential integrity |
| **9a tokens+actor** | API-half & attribution of 5/6/7 | symmetry contract + truthful `owner`/audit from day one |
| **5 corpus** | 6 | services activate on a corpus |
| **6 service** | 7, 10 | runs execute a service; datasets need worker-image provenance |
| **7 runs** | 8b, 10 | run state to observe; job machinery to export |
| **2 self-install** | (parallel) | only needs 1 + embedded migrations |

Each arm ships on its own branch off `master` (owner workflow: branch + push, no PR). The
`tracing`/`metrics` rails and the screen↔API symmetry contract are applied **continuously**, not as
a final arm.

---

## 5. Open questions for the owner

These change scope or sequencing; flagged for the review the handoff asked for *before large
refactors*:

1. **Identity model (Arm 9):** ✅ **RESOLVED 2026-06-13 — tokens-first.** Build the API-token model
   + `Bearer` guard + `actor` abstraction first; human accounts (sessions/login) layer on later;
   revive the `admin-ui` `user_permissions`/`user_actions` audit schema incrementally. No Google
   OAuth on the critical path.
2. **CLI-first vs Web-first (D1):** is the `cortex` CLI the priority deliverable (fastest path to an
   agent surface + self-install), with screens following — or should the web admin UI lead?
3. **Redis (Arm 11):** keep it (documented in self-install), make it **optional with graceful
   fallback**, or replace the report cache with an in-process cache? Affects the "one command, no
   extra daemons" self-install promise.
4. **`dependencies` table (Arm 6):** finally implement automatic service dependency management
   (revive the table with FKs), or drop it as dead weight for now? ✅ **RESOLVED — stays dropped**
   (already removed in migration `…050000`, Arm 12). No automatic dependency management for now; the
   `inputconverter` string covers the one real pipeline link. Revive only if a concrete need appears.
5. **`libarchive-sys` → pure-Rust archives (Arm 10):** worth dropping the C dependency for portable
   bundles, or keep the battle-tested C path given the arXiv-scale corpus already depends on it?
6. **UUID external handles (D8):** adopt now (cleaner API/public-view story) or defer (serial PKs
   are simpler short-term)? ✅ **RESOLVED 2026-06-16 — adopt now, additively.** Serial `id` STAYS
   the primary key and the FK target; the UUID is an *additional* stable external handle
   (`public_id`), not a replacement — internal joins keep `bigserial`, only public/API references
   use the UUID. Generated DB-side via PostgreSQL 18's built-in `uuidv7()` (time-ordered) on the
   **small** external-handle tables only (`corpora`, `services`, later `historical_runs`); never on
   `tasks`/`log_*`. **Phase 1 landed** (corpora + services, surfaced on `CorpusDto`/`ServiceDto`).
   Raises the deployment floor to **PostgreSQL 18**.
7. **Scope of "stop":** several operations are intentionally destructive (`register_service` wipes
   tasks, import deletes source `.gz`, `Corpus::destroy`). Confirm we want **soft-delete / undo**
   semantics in the productized versions, not just confirmation dialogs. ✅ **RESOLVED 2026-06-16 —
   keep hard-delete + confirm.** The existing dry-run + `--yes` / confirm-dialog guards over the
   transactional, orphan-free `Corpus::destroy`/`Service::destroy` are sufficient; no `deleted_at`
   soft-delete / retention layer (avoids filtered-read + purge complexity across all three surfaces).

---

*Appendix pointers:* build/run conventions and load-bearing facts → [`CLAUDE.md`](../CLAUDE.md).
The owner's original brief → `docs/CorTeX-productizing-HANDOFF.md` (not committed upstream).
