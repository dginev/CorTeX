# Installing CorTeX

A complete, step-by-step installation for the **entire** CorTeX system, verified on
**Ubuntu 26.04 with PostgreSQL 18** (the `cortex` production node). Every command below was run
end-to-end on a clean box; copy-paste them in order.

> **Coming soon (productize-2026):** a single `cortex init` command will collapse Steps 3–6 into a
> guided, self-healing bootstrap (see [`docs/PRODUCTIZING_PLAN.md`](docs/PRODUCTIZING_PLAN.md),
> Arm 2). Until then, this manual path is the supported installation, and `cortex init` will be
> built to reproduce exactly these steps.

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
  libpq-dev libzmq3-dev libarchive-dev libsodium-dev \
  pkg-config
```

- `postgresql` — the database server (installs PostgreSQL 18 on Ubuntu 26.04).
- `libpq-dev` — PostgreSQL client headers (needed to build Diesel and `diesel_cli`).
- `libzmq3-dev`, `libsodium-dev` — ZeroMQ transport for the dispatcher/workers.
- `libarchive-dev` — archive handling for corpus import and result bundles.

Verify the libraries are discoverable and the service is up:

```bash
pkg-config --modversion libzmq libarchive libsodium   # prints versions, no errors
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

CorTeX reads two files from the repository root:

**`.env`** — database connection strings. The repo ships a working default:

```
DATABASE_URL=postgres://cortex:cortex@localhost/cortex
TEST_DATABASE_URL=postgres://cortex_tester:cortex_tester@localhost/cortex_tester
```

> ⚠️ **Current limitation:** `DATABASE_URL` is read at **compile time**, so if you change `.env`
> you must rebuild (`cargo build`) for it to take effect. Making this fully runtime-configurable is
> Arm 1 of the productization plan.

**`config.json`** — frontend secrets and rerun tokens. Create it from the template:

```bash
cp config.default.json config.json
```

Then edit `config.json`: set a real `captcha_secret` and replace the `rerun_tokens` map with your
own `{ "secret-token": "username" }` entries (these gate the rerun/save-snapshot actions).

> Both `.env` and `config.json` (and `templates/`, `public/`, `Rocket.toml`) are resolved relative
> to the **current working directory** — run the binaries from the repository root.

## 5. Database schema (migrations)

Install the Diesel CLI (PostgreSQL only) and apply the migrations to **both** databases:

```bash
cargo install diesel_cli --no-default-features --features postgres

# production database (uses DATABASE_URL from .env)
diesel migration run

# test database
DATABASE_URL="postgres://cortex_tester:cortex_tester@localhost/cortex_tester" diesel migration run
```

Verify the schema landed (you should see `corpora`, `services`, `tasks`, the five `log_*` tables,
`historical_runs`, `historical_tasks`, `worker_metadata`, …):

```bash
PGPASSWORD=cortex psql "postgres://cortex:cortex@localhost/cortex" -c '\dt'
```

The `services` table is seeded with the two built-in services `init` (id 1) and `import` (id 2).

## 6. Build

```bash
cargo build            # add --release for production binaries
```

This compiles the workspace plus the git dependencies (`pericortex`, `libarchive-sys`). The first
build downloads and compiles ~360 crates and takes several minutes; subsequent builds are
incremental.

Run the test suite to confirm the database wiring (requires the `cortex_tester` DB from Step 3; the
TeX→HTML test additionally needs `latexmlc` on `PATH` and self-skips if absent):

```bash
cargo test
```

## 7. Run the system

Start the pieces from the repository root, each in its own shell (or under a process manager):

```bash
# 1) the dispatcher (ZeroMQ ventilator on :51695, sink on :51696)
cargo run --release --bin dispatcher

# 2) the web frontend (serves the dashboards)
cargo run --release --bin frontend

# 3) one or more workers (example: TeX→HTML). Workers connect to the dispatcher over ZeroMQ.
cargo run --release --example tex_to_html_worker
```

Open the frontend (default Rocket address `http://127.0.0.1:8000`) to see the corpora overview.
To stand up your first corpus and service today, see the example drivers in `examples/`
(`tex_to_html_import.rs`, `register_service.rs`) — these CLI workflows are being promoted to
first-class screens + API in the productization sprint (plan Arms 5–6).

## 8. Tuning for large datasets (optional, recommended at arXiv scale)

As the `log_*` and `tasks` tables grow into the tens of millions of rows, aggressive autovacuum
thresholds keep report performance from degrading
([source](https://lob.com/blog/supercharge-your-postgresql-performance/)):

```sql
-- run as the cortex user against the cortex database
ALTER TABLE log_infos    SET (autovacuum_enabled = true, autovacuum_vacuum_scale_factor = 0.0002,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50, autovacuum_vacuum_threshold = 50);
ALTER TABLE log_warnings SET (autovacuum_enabled = true, autovacuum_vacuum_scale_factor = 0.0002,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50, autovacuum_vacuum_threshold = 50);
ALTER TABLE log_errors   SET (autovacuum_enabled = true, autovacuum_vacuum_scale_factor = 0.0002,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50, autovacuum_vacuum_threshold = 50);
ALTER TABLE log_fatals   SET (autovacuum_enabled = true, autovacuum_vacuum_scale_factor = 0.0002,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50, autovacuum_vacuum_threshold = 50);
ALTER TABLE tasks        SET (autovacuum_enabled = true, autovacuum_vacuum_scale_factor = 0.0002,
  autovacuum_analyze_scale_factor = 0.0005, autovacuum_analyze_threshold = 50, autovacuum_vacuum_threshold = 50);
```

## 9. Troubleshooting

- **`config.json` not found / parse error** — you must run binaries from the repository root and
  `config.json` must be valid JSON (Step 4).
- **`password authentication failed`** — re-check Step 3 credentials and that they match `.env`;
  remember a changed `.env` needs a rebuild (Step 6 note).
- **`libpq`/`libzmq`/`libarchive` not found at build time** — re-run Step 2 and confirm
  `pkg-config --modversion <lib>` succeeds.
- **A migration fails on the public schema** — re-run the `GRANT ALL ON SCHEMA public` commands from
  Step 3 against the affected database.

## 10. Development notes

- Toolchain, `rustfmt`, and `clippy` are pinned via `rust-toolchain.toml`; `cargo fmt` and a clean
  `cargo clippy` are expected before pushing.
- See [`CLAUDE.md`](CLAUDE.md) for architecture facts and conventions, and
  [`docs/PRODUCTIZING_PLAN.md`](docs/PRODUCTIZING_PLAN.md) for the roadmap that turns this manual
  install into a one-command `cortex init`.
