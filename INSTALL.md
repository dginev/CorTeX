# Installing CorTeX

A complete, step-by-step installation for the **entire** CorTeX system, verified on
**Ubuntu 26.04 with PostgreSQL 18** (the `cortex` production node). Every command below was run
end-to-end on a clean box; copy-paste them in order.

> **Fast path (productize-2026):** once the binaries are built (Step 6), a single **`cortex init`**
> applies the embedded migrations and scaffolds `cortex.toml` — **no `diesel_cli` needed** — and
> `cortex doctor` verifies the install (see [`docs/PRODUCTIZING_PLAN.md`](docs/PRODUCTIZING_PLAN.md),
> Arm 2). The manual steps below remain the explicit, transparent reference for what `cortex init`
> automates; you only need the diesel_cli path in Step 5 for the *test* database or when authoring
> new migrations.

---

## 0. What you are installing

CorTeX is three cooperating processes plus two backing services:

| Component | What it is | Provided by |
|---|---|---|
| **dispatcher** | ZeroMQ ventilator/sink that leases tasks to workers and collects results | `cargo run --bin dispatcher` |
| **frontend** | Rocket web app: dashboards, reports, rerun/snapshot actions | `cargo run --bin frontend` |
| **worker(s)** | Convert documents (e.g. TeX→HTML); connect to the dispatcher over ZeroMQ | the `pericortex` crate / `examples/tex_to_html_worker.rs` |
| **PostgreSQL** | The metadata store (corpora, services, tasks, logs, history) | system package |

Document **bytes** live on a shared filesystem (a task's `entry` is an absolute path); the database
stores only metadata and pointers.

Administration — install, corpus/service setup, reports, reruns, snapshots, dataset export — is
driven by the **`cortex` CLI** (`cargo run --bin cortex -- <command>`), and every command is equally
available as a web dashboard action and a `/api` endpoint. See [`MANUAL.md`](MANUAL.md).

## 1. Prerequisites

- **OS:** Linux (tested on Ubuntu 26.04). `sudo` access for the system packages.
- **Rust:** the **nightly** toolchain. It is pinned by `rust-toolchain.toml` and auto-selected by
  `rustup` when you build inside the repo — you do not need to install a toolchain by hand if you
  already have `rustup`.
- **Disk:** PostgreSQL's data directory must live on **fast local storage (NVMe/SSD)**, **never** on
  a large QLC RAID array. On the `cortex` node the OS default (`/var/lib/postgresql`) is already on
  NVMe — leave it there. Plan for **≥250 GB** of database space for a full LaTeXML run over arXiv
  (see [issue #10](https://github.com/dginev/CorTeX/issues/10)).

## 2. System packages

```bash
sudo apt-get update
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y \
  postgresql postgresql-contrib \
  libpq-dev libzmq3-dev libsodium-dev \
  pkg-config
```

- `postgresql` — the database server (installs PostgreSQL 18 on Ubuntu 26.04). **PostgreSQL 18+ is
  required**: a migration uses the built-in `uuidv7()` to mint corpus/service external handles.
- `libpq-dev` — PostgreSQL client headers (needed to build Diesel and `diesel_cli`).
- `libzmq3-dev`, `libsodium-dev` — ZeroMQ transport for the dispatcher/workers.

Verify the libraries are discoverable and the service is up:

```bash
pkg-config --modversion libzmq libsodium   # prints versions, no errors
sudo systemctl enable --now postgresql
pg_lsclusters                                          # PostgreSQL cluster should be "online"
```

## 3. PostgreSQL roles and databases

CorTeX uses two databases — `cortex` (production) and `cortex_tester` (the test suite) — each owned
by a same-named login role. Create them:

```bash
# login roles (idempotent)
sudo -u postgres psql -c "CREATE ROLE cortex        LOGIN PASSWORD 'cortex';"
sudo -u postgres psql -c "CREATE ROLE cortex_tester LOGIN PASSWORD 'cortex_tester';"

# databases owned by those roles
sudo -u postgres createdb -O cortex        cortex
sudo -u postgres createdb -O cortex_tester cortex_tester

# PostgreSQL 15+ requires an explicit grant on the public schema
sudo -u postgres psql -d cortex        -c 'GRANT ALL ON SCHEMA public TO cortex;'
sudo -u postgres psql -d cortex_tester -c 'GRANT ALL ON SCHEMA public TO cortex_tester;'
```

Verify the exact connection strings CorTeX uses actually authenticate over TCP (Ubuntu's default
`pg_hba.conf` already allows password auth on `localhost` — no edits needed):

```bash
PGPASSWORD=cortex        psql "postgres://cortex:cortex@localhost/cortex"               -c '\conninfo'
PGPASSWORD=cortex_tester psql "postgres://cortex_tester:cortex_tester@localhost/cortex_tester" -c '\conninfo'
```

Both should connect. (If you prefer different credentials, change them here **and** in `.env` — see
Step 4.)

## 4. Configuration

CorTeX is configured at **runtime** via `cortex.toml` (figment), with environment overrides. The
database URL is resolved in this order (later wins): built-in defaults → `cortex.toml [database]` →
`CORTEX_DATABASE__URL` → the legacy `DATABASE_URL` (also read from a local `.env`). **No recompile is
needed to change databases** — e.g. point the frontend at a populated DB with
`DATABASE_URL=… cargo run --bin frontend`.

The simplest setup is `cortex init`, which scaffolds a `cortex.toml` (operational sections) and runs
migrations (Step 5). The repo also ships a working `.env` default:

```
DATABASE_URL=postgres://cortex:cortex@localhost/cortex
TEST_DATABASE_URL=postgres://cortex_tester:cortex_tester@localhost/cortex_tester
```

**Admin / API tokens** — every write action (rerun, import, service activation, maintenance, the
`/admin` sign-in) is gated by a token that maps to an **owner** (the identity recorded in the audit
log). Set one with the CLI — no hand-editing:

```bash
cortex set-admin-token --generate --owner alice   # prints a fresh random token (shown once)
cortex set-admin-token my-chosen-token --owner bob # or set a specific value
```

This merges `[auth].rerun_tokens` into `cortex.toml`, preserving the other sections and any existing
tokens (give each admin their own token for per-person attribution). Re-running with an existing
token updates its owner.

> **Legacy `config.json`:** the prototype kept `captcha_secret` + `rerun_tokens` in a
> `config.json`. If that file is present in the working directory it remains **authoritative for the
> `[auth]` section** (back-compat), so it will *shadow* `cortex.toml`'s tokens — `set-admin-token`
> warns when it detects this. Migrate by moving the tokens into `cortex.toml` and removing
> `config.json`.

> `.env`, `cortex.toml`, `config.json`, `templates/`, `public/`, and `Rocket.toml` are resolved
> relative to the **current working directory** — run the binaries from the repository root.

## 5. Database schema (migrations)

The migrations under `migrations/` are **embedded in the binary** (`src/migrations.rs`), so the
production database is migrated by **`cortex init`** after the build (Step 6) — **no `diesel_cli` on
the host**. That is the supported deployment path; you can skip ahead to Step 6 and let `cortex init`
apply the schema.

`diesel_cli` is only needed for the **test** database (the test harness does not run `cortex init`)
and for *authoring* new migrations:

```bash
cargo install diesel_cli --no-default-features --features postgres

# test database (production is migrated by `cortex init` in Step 7)
DATABASE_URL="postgres://cortex_tester:cortex_tester@localhost/cortex_tester" diesel migration run
```

Verify the schema landed (after migrating — `cortex init` for production in Step 7, or the
`diesel migration run` above for the test database — you should see `corpora`, `services`, `tasks`,
the five `log_*` tables, `historical_runs`, `historical_tasks`, `worker_metadata`, …):

```bash
PGPASSWORD=cortex psql "postgres://cortex:cortex@localhost/cortex" -c '\dt'
```

The `services` table is seeded with the two built-in services `init` (id 1) and `import` (id 2).

## 6. Build

```bash
cargo build            # add --release for production binaries
```

This compiles the workspace plus the git dependency (`pericortex`). The first
build downloads and compiles ~360 crates and takes several minutes; subsequent builds are
incremental.

Run the test suite to confirm the database wiring (requires the `cortex_tester` DB from Step 3; the
TeX→HTML test additionally needs `latexmlc` on `PATH` and self-skips if absent):

```bash
cargo test
```

## 7. Run the system

First **initialize the production database** — `cortex init` applies the embedded migrations (the
schema from Step 5, no `diesel_cli` needed) and scaffolds `cortex.toml` if absent; `cortex doctor`
then confirms the box is ready (database reachable, migrations current, services seeded, admin token
set):

```bash
cargo run --release --bin cortex -- init      # idempotent: safe to re-run
cargo run --release --bin cortex -- doctor     # green checklist, or actionable fixes
```

Then start the pieces from the repository root, each in its own shell (or under a process manager):

```bash
# 1) the dispatcher (ZeroMQ ventilator on :51695, sink on :51696)
cargo run --release --bin dispatcher

# 2) the web frontend (serves the dashboards)
cargo run --release --bin frontend

# 3) one or more workers (example: TeX→HTML). Workers connect to the dispatcher over ZeroMQ.
cargo run --release --example tex_to_html_worker
```

Open the frontend (default Rocket address `http://127.0.0.1:8000`) to see the corpora overview.

**Stand up your first corpus + service** — entirely from the `cortex` CLI (this supersedes the old
`examples/` drivers; the CLI is now the supported, first-class path, equally available as web screens
and `/api` endpoints):

```bash
# 1) define a conversion service (only the built-in init/import services are seeded)
cargo run --release --bin cortex -- create-service tex_to_html --inputformat tex --outputformat html
# 2) register a corpus and import its documents (one import task per document)
cargo run --release --bin cortex -- import arxmliv /data/arxmliv
# 3) queue one conversion task per document for the service
cargo run --release --bin cortex -- activate arxmliv tex_to_html
```

With the dispatcher + at least one worker running (above), the queued tasks now convert. Watch
progress with `cortex status` (or `cortex report arxmliv tex_to_html`), or the dashboard at `/admin`.
As the corpus grows, `cortex extend arxmliv` re-scans the path for newly-arrived documents. The full
command surface — reports, reruns, snapshots, sandboxes, dataset export, teardown — is in
[`MANUAL.md`](MANUAL.md) §14; every command has a 1:1 web screen and `/api` endpoint.

## 8. Database tuning for large datasets

**Per-table autovacuum is now automatic** — migration `2026-06-14-030000_autovacuum_tuning` applies
aggressive, size-relative autovacuum/autoanalyze + PostgreSQL-13 insert-based autovacuum to the
high-churn and append-heavy tables (`tasks`, the five `log_*`, `historical_tasks`), so report
performance does not degrade as they grow into the tens/hundreds of millions of rows. No manual step.

**Server-level tuning** (`shared_buffers`, `work_mem`, `effective_cache_size`, …) is sized to the
host's RAM/cores/storage — the PostgreSQL defaults are drastically undersized for a real CorTeX box.
CorTeX is a **"Mixed" workload** (OLTP task/log writes + DW bulk-loads + reporting). Run
**`cortex tune-db`** to print the pgtune inputs pre-filled for *this* host, then generate the config
with the pgtune service at <https://pgtune.leopard.in.ua/> (inputs: `mixed` / your RAM / physical
cores / `300` connections / `nvme`), apply the `ALTER SYSTEM` block it prints, and restart
PostgreSQL. (`cortex init` prints the same guidance as its last step.) A **verified example block**
for a 256 GB / 64-core / NVMe box — plus the build caveats for `wal_compression=lz4` /
`io_method=io_uring` — is in [`docs/DB_TUNING.md`](docs/DB_TUNING.md).

**Index maintenance:** indexes on the high-churn tables bloat over time; periodically rebuild them
online with `REINDEX (CONCURRENTLY) …` (see `docs/DB_TUNING.md` for the maintenance routine).

## 9. Troubleshooting

- **`config.json` not found / parse error** — you must run binaries from the repository root and
  `config.json` must be valid JSON (Step 4).
- **`password authentication failed`** — re-check Step 3 credentials and that they match `.env`;
  remember a changed `.env` needs a rebuild (Step 6 note).
- **`libpq`/`libzmq` not found at build time** — re-run Step 2 and confirm
  `pkg-config --modversion <lib>` succeeds.
- **A migration fails on the public schema** — re-run the `GRANT ALL ON SCHEMA public` commands from
  Step 3 against the affected database.

## 10. Development notes

- Toolchain, `rustfmt`, and `clippy` are pinned via `rust-toolchain.toml`; `cargo fmt` and a clean
  `cargo clippy` are expected before pushing.
- See [`CLAUDE.md`](CLAUDE.md) for architecture facts and conventions, and
  [`docs/PRODUCTIZING_PLAN.md`](docs/PRODUCTIZING_PLAN.md) for the roadmap. `cortex init` +
  `cortex doctor` already automate the schema-migration, config-scaffold, and verification steps
  (Steps 4–5, 7); the remaining roadmap work folds the OS/PostgreSQL provisioning (Steps 2–3) into
  the guided bootstrap.
