# PostgreSQL tuning & ongoing maintenance for CorTeX

Two layers, both productization concerns ("from the installation of cortex…"):

1. **Per-table autovacuum** — automatic, in migration `2026-06-14-030000_autovacuum_tuning` (the
   `tasks` / `log_*` / `historical_tasks` tables get aggressive, size-relative autovacuum +
   PG13 insert-based autovacuum). Nothing to do.
2. **Server-level config** (`postgresql.conf`) — sized to the host. The stock PostgreSQL defaults
   (`shared_buffers = 128MB`, `work_mem = 4MB`, `effective_cache_size = 512MB`) are wildly
   undersized for a real CorTeX box and leave most of RAM unused while report sorts spill to disk.
3. **Index maintenance** — periodic online `REINDEX`.

## Server config — pgtune algorithm

CorTeX is a **mixed OLTP + reporting** workload (the dispatcher does many small task-status writes;
the frontend runs heavy aggregations over the huge `log_*` tables + the `report_summary` rollup).
The values below follow the standard pgtune algorithm for that workload on **SSD/NVMe**, parameterized
by host RAM (`R`), core count (`C`), and a chosen `max_connections` (`N`):

| setting | formula | rationale |
|---|---|---|
| `shared_buffers` | `R/4` | PG's own cache; 25% of RAM is the long-standing sweet spot |
| `effective_cache_size` | `R*3/4` | planner's estimate of OS+PG cache — biases toward index scans |
| `work_mem` | `(R - shared_buffers) / (N*3) / parallelism` | per-sort/hash; sized to avoid the report spills |
| `maintenance_work_mem` | `min(R/16, 2GB)` | vacuum / index build / REINDEX |
| `random_page_cost` | `1.1` | NVMe: random ≈ sequential |
| `effective_io_concurrency` | `200` | NVMe handles deep queues |
| `max_worker_processes` | `C` | parallelism budget |
| `max_parallel_workers` | `C` | |
| `max_parallel_workers_per_gather` | `4` | per-query cap (big report scans) |
| `max_parallel_maintenance_workers` | `4` | parallel index builds / vacuum |
| `wal_buffers` | `16MB` | |
| `min_wal_size` / `max_wal_size` | `2GB` / `8GB` | write-heavy dispatcher → fewer checkpoints |
| `checkpoint_completion_target` | `0.9` | spread checkpoint I/O |
| `huge_pages` | `try` | reduce TLB pressure for a multi-GB `shared_buffers` |

### Concrete values for *this* box (246 GiB RAM, 128 cores, NVMe; `N=200`)

```
ALTER SYSTEM SET max_connections = '200';
ALTER SYSTEM SET shared_buffers = '61GB';
ALTER SYSTEM SET effective_cache_size = '184GB';
ALTER SYSTEM SET work_mem = '64MB';
ALTER SYSTEM SET maintenance_work_mem = '2GB';
ALTER SYSTEM SET random_page_cost = '1.1';
ALTER SYSTEM SET effective_io_concurrency = '200';
ALTER SYSTEM SET max_worker_processes = '128';
ALTER SYSTEM SET max_parallel_workers = '128';
ALTER SYSTEM SET max_parallel_workers_per_gather = '4';
ALTER SYSTEM SET max_parallel_maintenance_workers = '4';
ALTER SYSTEM SET wal_buffers = '16MB';
ALTER SYSTEM SET min_wal_size = '2GB';
ALTER SYSTEM SET max_wal_size = '8GB';
ALTER SYSTEM SET checkpoint_completion_target = '0.9';
ALTER SYSTEM SET huge_pages = 'try';
```

`shared_buffers`, `max_connections`, and `huge_pages` need a **restart**; the rest take effect on
`SELECT pg_reload_conf();`. (Compare: the box ships with `shared_buffers = 128MB`, `work_mem = 4MB`,
`effective_cache_size = 512MB` — i.e. ~0.2% of RAM in cache.)

> The `report_summary` matview REFRESH does a large `GROUP BY … ROLLUP` with `COUNT(DISTINCT)`; if it
> spills, give *that session* more room with `SET work_mem = '512MB'` before the refresh rather than
> raising the global.

## Wiring into `cortex init` (planned)

DB tuning is a natural env-setup step for the self-installing CLI. Planned `cortex tune-db`:
- read host RAM (`/proc/meminfo`) + cores (`available_parallelism`) + storage class,
- compute the table above (pure, unit-testable),
- **print** the `ALTER SYSTEM` block by default (no elevated privileges needed), and with
  `--apply <superuser-url>` execute it + `pg_reload_conf()` (and warn which keys need a restart),
- `cortex doctor` flags settings that are still at the stock default (observability).

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
