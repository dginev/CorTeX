# PostgreSQL tuning & ongoing maintenance for CorTeX

Three layers, all productization concerns ("from the installation of cortex…"):

1. **Per-table autovacuum** — automatic, in migration `2026-06-14-030000_autovacuum_tuning` (the
   `tasks` / `log_*` / `historical_tasks` tables get aggressive, size-relative autovacuum +
   PostgreSQL-13 insert-based autovacuum). Nothing to do.
2. **Server-level config** (`postgresql.conf` / `ALTER SYSTEM`) — sized to the host. The stock
   PostgreSQL defaults (`shared_buffers = 128MB`, `work_mem = 4MB`, `effective_cache_size = 4GB`)
   are wildly undersized for a real CorTeX box and leave most of RAM unused while report sorts spill.
3. **Index maintenance** — periodic online `REINDEX`.

## Server config — pgtune

Source of truth is the **le0pard pgtune** web service — <https://pgtune.leopard.in.ua/> (open
source: <https://github.com/le0pard/pgtune>). CorTeX is a **"Mixed type of application"** in its
taxonomy — *mixed DW + OLTP characteristics, a wide mixture of queries*: the dispatcher does a
steady stream of small `tasks`/`log_*` writes (OLTP), the importer does large bulk corpus loads
(DW), and the frontend runs large complex aggregations over the `log_*` tables + the
`report_summary` rollup (DW). It is **not** "Web" (that wants a DB *much smaller* than RAM and 90%
simple queries — both false here) and not pure OLTP/DW.

The tool's model, paraphrased (parameterized by host RAM `R`, CPU count `C`, chosen connection
count `N`, storage class, and a *total-data-size-vs-RAM* selector):

| setting | le0pard rule (Mixed) | rationale |
|---|---|---|
| `shared_buffers` | `R/4` | PG's own cache; 25% of RAM is the sweet spot |
| `effective_cache_size` | `R*3/4` | planner's estimate of OS+PG cache — biases toward index scans |
| `maintenance_work_mem` | `min(R/16, 8GB)` | vacuum / index build / REINDEX (8 GB cap on Linux) |
| `work_mem` | `≈ (R − shared_buffers) / ((N + C)·3)`, trimmed when data > RAM | per-sort/hash; sized down for many connections |
| `random_page_cost` | `1.1` (SSD/NVMe/SAN) · `4` (HDD) | NVMe: random ≈ sequential |
| `effective_io_concurrency` | HDD `2` · SSD `200` · SAN `300` · **NVMe `1000`** | NVMe handles very deep queues |
| `max_worker_processes` / `max_parallel_workers` | `C` | parallelism budget |
| `max_parallel_workers_per_gather` | `ceil(C/2)` capped at `4` | per-query cap (big report scans) |
| `max_parallel_maintenance_workers` | `ceil(C/2)` capped at `4` | parallel index builds / vacuum |
| `wal_buffers` | `3%` of `shared_buffers`, capped `16MB` | |
| `min_wal_size` / `max_wal_size` | `1GB` / `4GB` (Mixed) | |
| `checkpoint_completion_target` | `0.9` | spread checkpoint I/O |
| `default_statistics_target` | `100` (Mixed) | planner sample size |
| `huge_pages` | `try` when `shared_buffers ≥ 2GB` | reduce TLB pressure for a multi-GB cache |
| `jit` | `off` | avoid per-query JIT compile overhead on this profile |
| `wal_compression` | `lz4` *(needs PG built `--with-lz4`)* | cheaper WAL for the write-heavy dispatcher |
| `autovacuum_max_workers` | `5` (high-mem hosts) | bigger global autovacuum pool |
| `autovacuum_work_mem` | `2GB` | each autovacuum worker vacuums big tables in fewer passes |
| `io_method` | `io_uring` *(PG18; needs `--with-liburing`)* | async I/O — big win on NVMe |

### Concrete values for *this* box — verbatim le0pard output

Inputs: **DB Version 18 · OS linux · DB Type `mixed` · Total RAM 256 GB · CPUs 64 · Connections
300 · Storage `nvme`**. Two deliberate input choices: **CPUs = 64** (the box has 128 *threads* /
64 physical cores — parallel workers should be sized to physical cores, not HT siblings), and
**Connections = 300** (our ~200-worker ZMQ fleet plus the per-transaction connection pattern need
headroom above the Mixed default of 100; this is also the value CI runs at). For "total data size"
we are **larger than RAM** — a full LaTeXML-over-arXiv run is ≥250 GB of metadata + `log_*` rows
(est. 1–3× RAM), so the DB does not fit in 256 GB.

```
ALTER SYSTEM SET max_connections = '300';
ALTER SYSTEM SET shared_buffers = '64GB';
ALTER SYSTEM SET effective_cache_size = '192GB';
ALTER SYSTEM SET maintenance_work_mem = '8GB';
ALTER SYSTEM SET checkpoint_completion_target = '0.9';
ALTER SYSTEM SET wal_buffers = '16MB';
ALTER SYSTEM SET default_statistics_target = '100';
ALTER SYSTEM SET random_page_cost = '1.1';
ALTER SYSTEM SET effective_io_concurrency = '1000';
ALTER SYSTEM SET work_mem = '92182kB';
ALTER SYSTEM SET huge_pages = 'try';
ALTER SYSTEM SET jit = 'off';
ALTER SYSTEM SET wal_compression = 'lz4';
ALTER SYSTEM SET autovacuum_max_workers = '5';
ALTER SYSTEM SET autovacuum_work_mem = '2GB';
ALTER SYSTEM SET io_method = 'io_uring';
ALTER SYSTEM SET min_wal_size = '1GB';
ALTER SYSTEM SET max_wal_size = '4GB';
ALTER SYSTEM SET max_worker_processes = '64';
ALTER SYSTEM SET max_parallel_workers_per_gather = '4';
ALTER SYSTEM SET max_parallel_workers = '64';
ALTER SYSTEM SET max_parallel_maintenance_workers = '4';
```

**This is applied and live on the `cortex` node** (Ubuntu 26.04, PostgreSQL 18.4). Notes:

- `shared_buffers`, `max_connections`, `max_worker_processes`, `io_method` need a **restart**; the
  rest take effect on `SELECT pg_reload_conf();`.
- **Build-dependent settings** (`wal_compression = lz4`, `io_method = io_uring`) require PostgreSQL
  compiled `--with-lz4` / `--with-liburing`. Both are present in the Ubuntu 26.04 PG 18.4 build and
  **verified active** (`SHOW io_method;` reports `io_uring`, not a silent fallback). On a build
  lacking them, `cortex tune-db` must detect support (the enum value is absent from
  `pg_settings.enumvals`) and skip/downgrade (`wal_compression = on` for pglz; leave `io_method`
  default).
- The tool prints a **"not optimal for very high memory systems"** warning. That is fine here: the
  per-table autovacuum migration is what keeps the huge tables healthy at scale; these globals just
  size the caches/parallelism/WAL to the host.
- `huge_pages = try` degrades gracefully — the kernel currently has no huge pages reserved
  (`HugePages_Total: 0`), so PG falls back to normal pages without failing. Reserving
  `vm.nr_hugepages` for the 64 GB `shared_buffers` (~32k × 2 MB pages) is an optional further win.

> The `report_summary` matview REFRESH does a large `GROUP BY … ROLLUP` with `COUNT(DISTINCT)`; if it
> spills under the (deliberately modest, 300-connection) `work_mem = 92182kB`, give *that session*
> more room with `SET work_mem = '512MB'` before the refresh rather than raising the global.

## Wiring into `cortex init` (decided: guide + link, don't reimplement)

**Decision:** `cortex init` does *not* port the pgtune algorithm in-tree. Server tuning is a heuristic
that the upstream tool maintains (and updates per PG version — `io_uring`/`lz4`/`jit` are recent
additions), and it even warns it's "not optimal for very high memory systems". Reproducing it would
be a maintenance burden for marginal gain over pointing operators at the authoritative source. So the
init env-setup step **prints guidance + the live link**, and the repo carries one verified example
block (above) for reference. Exact message `cortex init` should emit (drop-in when the init binary
lands, Arm 2):

```text
NOTE: PostgreSQL server tuning is recommended but not automated.
  CorTeX is a "Mixed" workload (OLTP task/log writes + DW bulk-loads + reporting).
  Generate a config at https://pgtune.leopard.in.ua/
    inputs: DB Type = mixed · OS = linux · DB Version = <your PG major>
            Total RAM = <host RAM> · CPUs = <physical cores> · Connections = 300 · Storage = nvme|ssd
  Apply the ALTER SYSTEM block it prints, then restart PostgreSQL.
  A verified example (256 GB / 64 cores / nvme) is in docs/DB_TUNING.md.
  Build note: the tool may emit wal_compression=lz4 / io_method=io_uring — keep them only if
  `SELECT name, enumvals FROM pg_settings WHERE name IN ('wal_compression','io_method')` lists them.
```

`cortex init` can still *fill in* `<host RAM>` / `<physical cores>` / `<your PG major>` from the host
so the operator's inputs are pre-computed; it just won't compute or apply the output itself. A future
`cortex doctor` may add a cheap, high-signal warning if `shared_buffers` is still at the `128MB` stock
default (no pgtune port needed for that one comparison) — optional, deferred. Per-table autovacuum is
already handled by the migration, so the init guidance is server-config only.

## Index maintenance (REINDEX)

High-churn tables bloat their indexes over time, slowing scans. Rebuild **online** (no exclusive
lock) periodically — monthly at arXiv scale, or when index bloat is observed:

```sql
REINDEX (CONCURRENTLY) TABLE tasks;
REINDEX (CONCURRENTLY) TABLE log_warnings;   -- and log_errors / log_fatals / log_infos / log_invalids
REINDEX (CONCURRENTLY) TABLE historical_tasks;
```

`REINDEX … CONCURRENTLY` (PG12+) rebuilds without blocking reads/writes; run during a low-traffic
window (it needs transient extra disk ≈ the index size + holds a session per table). This too is a
candidate for a `cortex maintenance` subcommand (a background [[job|jobs]] with the pending/health
observability), so operators get a one-command, observable maintenance routine.
