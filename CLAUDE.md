# CLAUDE.md — CorTeX conventions for agents

CorTeX is a distributed corpus-conversion framework for scholarly documents. We are mid-sprint
**productizing** it (admin-only prototype → self-installing, agent-first + human-first app). The
plan and current-state map live in **[`docs/PRODUCTIZING_PLAN.md`](docs/PRODUCTIZING_PLAN.md)** —
read it before non-trivial work. Active work branch: **`productize-2026`**.

## What this system is (the 60-second model)

- **Postgres (Diesel 2.2) = metadata store.** It holds `corpora`, `services`, one `tasks` row per
  `(corpus, service, document-entry)`, five severity-partitioned `log_*` tables, `historical_runs`
  (per-run tallies), `historical_tasks` (per-task status snapshots), `worker_metadata`. Document
  **bytes live on a shared filesystem** (`/data/...`); `tasks.entry` is the absolute path to a
  document's source archive.
- **ZeroMQ dispatcher** (`bin/dispatcher.rs`, `src/dispatcher/`) leases TODO tasks to **workers**
  (the external `pericortex` crate), streams sources out (ventilator, port **51695**), receives
  result archives (sink, port **51696**), parses each result's `cortex.log` into a status +
  messages, and persists via the finalize thread.
- **Rocket frontend** (`bin/frontend.rs`, `src/frontend/`, Tera `templates/`) renders read-only
  reports and a few token-gated writes (rerun, save-snapshot).

## Load-bearing facts (don't get burned)

- **Task status is a signed int** (`src/helpers.rs::TaskStatus`): `TODO=0`, `NoProblem=-1`,
  `Warning=-2`, `Error=-3`, `Fatal=-4`, `Invalid=-5`, `Blocked<-5`, `Queued>0` (a positive lease
  mark). These ints are also hardcoded in `scripts/*.sh`.
- **Magic service ids:** `1=init`, `2=import`, `>2`=real services. Code relies on this.
- **`Backend::default()` opens a NEW PgConnection** — every Rocket handler does this per request;
  `WorkerMetadata` spawns a new thread+connection per ZMQ transaction. (Pooling is Arm 3; don't add
  more unpooled connections.)
- **DB URL is now RUNTIME config** (Arm 1 landed): `backend::default_db_address()` reads
  `config().database.url` from figment (`src/config.rs`) — precedence: defaults → `cortex.toml` →
  `CORTEX_`-prefixed env (`CORTEX_DATABASE__URL`) → legacy `DATABASE_URL`/`.env` (loaded at runtime via
  `dotenvy`, highest precedence). **No recompile to switch databases** — e.g. point the frontend at a
  populated DB with `DATABASE_URL=… cargo run --bin frontend` (see `docs/TEST_DRIVE.md`). The old
  compile-time `dotenv!`/`DEFAULT_DB_ADDRESS` baking is gone.
- **Redis has been removed** (Arm 14 #6.2). Frontend reports are now served from the
  `report_summary` materialized-view rollup (`src/backend/rollup.rs`, `reports::task_report`),
  refreshed on the run-completion path (finalize drain + at-least-daily, plus `mark_new_run`); the
  old `cached/worker.rs` cache daemon, the `redis` crate, and the dead `CacheConfig` (Redis settings,
  incl. the phantom Settings-page inputs) are gone. **The frontend boots without Redis.**
  (The thin uncached proxy formerly at `src/frontend/cached/` was renamed to
  `src/frontend/render.rs`.)
- **CWD-coupled:** `load_config()` reads `config.json` from the CWD (panics if missing), and
  `Rocket.toml`/`templates/`/`public/` are CWD-relative — **run binaries from the repo root.**
- **The dispatcher panics on purpose** (mutex poisoning → process abort → external restart). Don't
  "fix" those panics into silent recovery; preserve fail-fast where it's the design (see Arm 4/12).
- **Referential FKs are landing in Arm 3.** As of migration `…140000`, the five `log_*` tables now
  have `task_id → tasks(id) ON DELETE CASCADE` (added `NOT VALID` + `VALIDATE` after an orphan-sweep),
  joining the original `historical_tasks.task_id → tasks` FK — so a raw `DELETE FROM tasks` now
  cascades its logs safely. **Still missing (Phase 2b): `tasks → corpora`/`services` FKs**, so a raw
  `DELETE FROM corpora` still orphans its *tasks* — keep deleting a corpus through **`Corpus::destroy`**
  (removes `log_*` + tasks + corpus in **one transaction**; orphan-free + crash-consistent; the
  frontend `delete_corpus` path uses it) until that FK lands. **External UUIDv7 handles** (`public_id`,
  migration `…130000`) exist on `corpora`/`services` — additive, the serial `id` is still the PK + FK
  target. (The dead `dependencies` table was dropped — migration `…050000`, Arm 12.)

## Build / run

Build deps (Ubuntu; not yet installed on a fresh box):
```bash
sudo apt install -y postgresql libpq-dev libzmq3-dev libsodium-dev pkg-config
cargo install diesel_cli --no-default-features --features postgres   # only for the test DB / authoring migrations
```
Then (from repo root): `cargo build`, then `cortex init` — migrations are **embedded**
(`src/migrations.rs`), so `init` self-migrates the production DB and scaffolds `cortex.toml` with **no
`diesel_cli` on the host**; `cortex doctor` verifies. (diesel_cli above is still needed to migrate the
*test* DB and to author new migrations.) Toolchain is **nightly** (`rust-toolchain.toml`, floating).
DB on **NVMe, never `/data`** (QLC RAID6 is wrong for an OLTP DB).

Tests are integration-heavy and need a live test DB (`TEST_DATABASE_URL`); `tex_to_html_test`
additionally needs `latexmlc` (skips otherwise): `cargo test`.

## Coding conventions

- **Maximum robustness is the prime directive** ([`docs/DESIGN_PRINCIPLES.md`](docs/DESIGN_PRINCIPLES.md)).
  arXiv data is hostile and `latexml-oxide` fails unpredictably, so design every component for
  resilience, fault-tolerance, and *transparent* failure: **no unbounded per-event resource
  acquisition** (pool/bound connections, threads, in-flight work), **no `unwrap`/`expect`/`panic!` on
  request or dispatch paths** (return `Result`, log, count, continue — never drop work silently),
  isolate blast radius, degrade gracefully, idempotent crash-consistent writes. Record every
  resilience gap you find in [`docs/KNOWN_ISSUES.md`](docs/KNOWN_ISSUES.md) (the running ledger —
  owner: *we go back and solve them all at the end*); never fix-and-forget or leave a gap unrecorded.
- **Style:** `.rustfmt.toml` (2-space indent, custom). Run `cargo fmt`; `cargo clippy` must stay
  clean. `src/lib.rs` has `#![deny(missing_docs)]` — **every public item needs a doc comment.**
- **License header:** every source file starts with the MIT copyright block (copy an existing file).
- **The symmetry contract (the sprint's core rule):** a screen and its agent API are **one
  controller returning one shared DTO** — render HTML for `Accept: text/html`, schema'd JSON
  otherwise. Don't build screens and APIs separately. North star: *every human screen action has a
  1:1 documented agent API, and both see the same live + historical run state.*
- **Observability is not optional:** new admin actions and task-lifecycle transitions emit
  `tracing` events + `metrics` (once the substrate lands, Arm 8). Thread an **actor** (human/agent)
  through every write.
- **Prefer the foundations** in the plan (figment config, clap CLI, r2d2 pool, thiserror/anyhow,
  utoipa/schemars, embedded migrations) over hand-rolling config/logging/errors/endpoints.

## Git workflow

- Branch off `master`; **do not push to `master`.** Owner reviews on GitHub — current preference is
  **branch + push, no PR**. One branch per arm.
- If a pre-push hook can't find `cargo`, `source ~/.cargo/env` first.
- Git identity: `Deyan Ginev <deyan.ginev@gmail.com>`.
- Migrations: always write a working `down.sql`; verify reversibility.

## Map

`bin/{frontend,dispatcher}.rs` · `src/backend/` (aggregate DB ops) · `src/dispatcher/`
(ventilator/sink/finalize/manager/server) · `src/frontend/` (routes, render.rs, concerns, params) ·
`src/models/` (Diesel structs) · `src/helpers.rs` (`TaskStatus`, log parsing, `generate_report`) ·
`src/importer.rs` (corpus ingest) · `src/worker.rs` (the `init` worker) · `migrations/` ·
`templates/` (Tera) · `scripts/` + `examples/` (the out-of-band admin tasks we are productizing).
