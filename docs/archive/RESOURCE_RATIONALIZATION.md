# Resource & Performance Rationalization (Plan Arm 14)

> Status: design notes / decisions, not yet implemented. Captures the owner's resource-use questions
> as **mini-choices with mini-plans**. Cross-reference: [`PRODUCTIZING_PLAN.md`](../PRODUCTIZING_PLAN.md)
> Arm 14, and the load profile in the sprint notes (~2 admins, 20 users, ~200 ZeroMQ workers,
> ~100 tasks/s upper bound).

## The reframing that drives everything: fast workers move the bottleneck

The legacy workers (Perl LaTeXML) took tens of seconds per task, so **worker compute** was the
bottleneck and the dispatcher / DB / disk sat idle. **latexml-oxide round-trips a task in ~1 s**;
× ~200 workers ⇒ **~200 tasks/s capacity, which exceeds the ~100 tasks/s estimate.** So the
bottleneck moves to the **dispatcher, the database, and disk I/O**, and the per-task overheads that
used to be invisible become the throughput ceiling. The three that bite first at 100–200 tasks/s:

- `WorkerMetadata::record_dispatched` / `record_received` spawn **a new thread + new PG connection
  per ZeroMQ event** → ~200–400 connection opens/sec.
- `mark_done` does a **delete-all-five-log-tables-then-reinsert per task** every finalize cycle.
- Result archives are written to the **`/data` QLC RAID6** (slow random write).

This reframing makes choices #6, #4, #2 the throughput-critical ones.

## Decision summary

| # | Choice | Decision | Priority |
|---|---|---|---|
| 6 | Aggregate-report cost / Redis cache | **Incremental rollup tables** (cheap + fresh; drop hard Redis dep) | **1 (highest)** |
| 4 | Constraints from a 1 s/task worker | **Re-tune for fast workers**; kill per-event thread+conn first | **2** |
| 2 | NVMe as dispatch staging | **Yes** — stage on NVMe, async-move results to RAID | **3** |
| 3 | Batch sizing / commit checkpointing | **Tune finalize interval + fix `mark_done`** | **4** |
| 1 | Async file I/O | **Measure first**; prefer write thread-pool over async/ZMQ rewrite | **5** |
| 5 | DB choice (Postgres vs sqlite/duckdb) | **Keep PostgreSQL** for OLTP; columnar only for reports if needed | resolved |

---

## 1. Async I/O for file read/write

- **Question:** Should the ventilator's source reads and the sink's result writes be async?
- **Considerations:** The dispatcher is sync + threaded; the `zmq 0.10` binding is synchronous, so a
  full async path means an awkward async/sync boundary or a transport rewrite (which we have ruled
  out — the ZeroMQ dispatcher is the backbone). At 100–200 tasks/s a single sink thread doing
  blocking writes to slow storage can serialize.
- **Decision:** **Measure before rewriting.** After NVMe staging (#2) the writes may no longer be the
  bottleneck. If they are, add a **bounded write thread-pool** in the sink (cheap, local) rather than
  an async/ZMQ rewrite. Async is a real lever but the *last* one to reach for.
- **Mini-plan:** (a) instrument sink write time per task at target load; (b) if write-bound after #2,
  hand result-archive writes to a small `rayon`/thread-pool; (c) re-measure. No transport changes.

## 2. NVMe fast-dispatch staging with async staging back to the SSD RAID

- **Question:** Use NVMe as a fast scratch/dispatch layer and asynchronously stage results back to
  the `/data` QLC RAID6?
- **Considerations:** `/data` is heavy-random-I/O-tuned QLC RAID6 (slow writes); NVMe has ~1.5 TB
  free and is fast. Per-task random writes to the RAID are a prime bottleneck at 200 tasks/s. Staging
  converts them into fast NVMe writes + batched sequential moves to the RAID.
- **Decision:** **Yes — design a staging layer.** Highest-leverage I/O fix.
- **Mini-plan:** (a) a configurable NVMe scratch dir; (b) sink writes result archives to NVMe first;
  (c) a background **stager** batches/moves completed results to `/data` and updates the task's
  on-disk location; (d) crash-consistency: a result on NVMe-not-yet-staged is recoverable (re-stage
  on restart); (e) space guard on NVMe (watermark + backpressure). **Open question for owner:** is
  the canonical archived location always `/data`, with NVMe purely transient? (assumed yes).

## 3. Batch sizing & commit checkpointing

- **Question:** Are the dispatch batch (`queue_size=800`) and the finalize commit cadence right?
- **Considerations:** Dispatch batch is fine. The cost is the **finalize cycle**: `mark_done` runs
  every ~1 s and, per task, deletes rows from all five `log_*` tables and reinserts — even when
  nothing changed. At 100–200 tasks/s that is the dominant DB write cost.
- **Decision:** Tune the finalize interval/batch and **stop the blind delete+reinsert** in
  `mark_done` (only churn logs that actually changed; or upsert).
- **Mini-plan:** (a) measure `mark_done` transaction time at load; (b) make the finalize interval +
  max-batch configurable; (c) rewrite `mark_done` to diff/upsert log rows instead of delete-all +
  reinsert; (d) feed the incremental rollups (#6) from this same path.

## 4. New constraints/bottlenecks from latexml-oxide (1 s/task × 200)

- **Question:** What breaks when the worker fleet can sustain ~200 tasks/s?
- **Considerations:** See the reframing above — compute is no longer the limit; per-task dispatcher +
  DB overheads are. The worst offender is the **per-ZMQ-event thread+connection** in
  `WorkerMetadata`.
- **Decision:** **Re-tune the dispatcher for fast workers.** First: eliminate the per-event
  thread+connection churn (use the pool + batch metadata updates). Then #2/#3/#6 follow.
- **Mini-plan:** (a) replace `record_dispatched/received`'s `thread::spawn` + `from_address` with a
  shared pool checkout (and/or batch metadata writes on the finalize cadence); (b) load-test at
  100 → 200 tasks/s with latexml-oxide workers; (c) confirm the bottleneck order matches this doc
  and adjust priorities from evidence.

## 5. Database choice — keep PostgreSQL, or a lightweight DB?

- **Question:** Is Postgres adding perf degradation / setup complexity vs SQLite / DuckDB / others?
- **Considerations:**
  - **SQLite** has a single-writer lock — a dealbreaker with concurrent finalize + frontend +
    worker_metadata + reruns at 100–200 writes/s.
  - **DuckDB** is columnar OLAP — excellent for the report aggregation (#6), poor for
    high-frequency OLTP row updates / concurrent writers.
  - **Setup complexity** was the strongest argument for switching, but Arms 1–2 already removed it:
    `cortex init` + embedded migrations + no `diesel_cli`, all behind one command.
- **Decision:** **Keep PostgreSQL** for the OLTP task store. Consider a columnar/materialized layer
  **only** for reports (#6), never for the task lifecycle.
- **Mini-plan:** no migration. If #6's rollups prove insufficient, evaluate Postgres materialized
  views or an embedded DuckDB *read-replica* for analytics — additive, not a replacement.

## 6. Aggregate reports — beyond the Redis cache, cheap *and* fresh

- **Question:** Reports are expensive O(millions of log rows) scans, currently shielded by a Redis
  cache (with staleness + an extra daemon + a `cache_worker`). Cheaper alternatives without
  staleness?
- **Considerations:** Caching only *hides* the O(rows) cost and adds staleness, a Redis dependency,
  and invalidation complexity. The real fix is to not recompute aggregates from raw logs on read.
- **Decision:** **Maintain incremental rollup/summary tables** — per-`(corpus, service, severity,
  category, what)` counts updated in the task-completion path (a generalization of what
  `historical_runs` already does coarsely). Reports become O(categories) lookups: **cheap and always
  fresh**, which lets us **drop the hard Redis dependency** (also Arm 11).
- **Mini-plan:** (a) add a `report_rollups` table (or counters) keyed by the report dimensions;
  (b) update it transactionally in the finalize path (#3) as task statuses/log messages change;
  (c) point the report queries at the rollups; (d) keep raw `log_*` for drill-down only;
  (e) backfill rollups once from existing data; (f) make Redis caching optional/removable.
- **Open question for owner:** acceptable to add per-task rollup-maintenance write cost in exchange
  for removing the expensive read-time scans + Redis? (recommended yes).

---

## Sequencing

A **measurement spike** should precede implementation: instrument the dispatcher + DB at a simulated
100 → 200 tasks/s (ideally with latexml-oxide workers) to turn the priorities above into evidence.
Then, in order: **#6 rollups** (cheap fresh reports, drop Redis) → **#4** (kill per-event connection
churn) → **#2** (NVMe staging) → **#3** (`mark_done` / finalize tuning) → **#1** (async, only if still
I/O-bound). #5 is resolved (keep Postgres). Each lands as its own TDD increment under the project's
quality gates.

---

## Measurement spike findings (2026-06-13)

A quick spike on the `cortex` box (quiet, test DB, synthetic uniform data) to make the priorities
evidence-led. Order-of-magnitude conclusions hold; exact numbers shift with real arXiv data
distribution and concurrent load.

**#4 — connection churn is a hard wall (highest-confidence result).** 300 iterations each:

| Path | Per connection |
|---|---|
| fresh `PgConnection::establish` (today's per-ZMQ-event pattern) | **4.46 ms** (TCP + scram-sha-256 auth) |
| pooled `pool.get()` | **0.011 ms** |
| **speedup** | **~395×** |

At 200 tasks/s the dispatcher does ~400 metadata writes/s (dispatch + return) ⇒ **~1.8 s of pure
connection-open overhead per wall-second** — impossible, so the current `WorkerMetadata` thread+
connection-per-event pattern **cannot sustain target load**; even 100 tasks/s (~0.9 s/s) nearly
saturates it, before the `thread::spawn` cost. **Fix:** route `record_dispatched/received` through
the existing r2d2 pool (Arm 3) and/or batch them — cheap, high-impact.

**#6 — aggregate reports are expensive at scale.** Synthetic: 1 corpus/service, **500k** warning
tasks, **1.5M** `log_warnings` rows.

| Query | Time | Plan |
|---|---|---|
| top-level status counts (`progress_report`) | **~19 ms** | cheap group-by count |
| **category report** (the Redis-cached one) | **~500 ms** warm | Seq Scan 1.5M + Hash Join + **external merge Sort spilling 38 MB to disk** (exceeds `work_mem`) |

Half a second of DB scan **per uncached category-page load**, spilling to disk — exactly why the
Redis cache exists. The cost is at the **category/what drill-down** (join+group over `log_*`), not
the top-level counts. **Fix:** precomputed rollup counts keyed by the report dimensions make this an
indexed sub-millisecond lookup — cheap *and* fresh, removing the hard Redis dependency. (The
"rollup read" line in the bench recomputed live and is **not** representative of a true rollup read.)

**Net:** the priority order holds — **#4 (pool the metadata writes)** and **#6 (rollup tables)** are
the two highest-leverage and both are tractable. Re-measure after each fix to verify the wins.

---

## Full-pipeline validation of #4 (2026-06-13)

The 395× figure above is an *isolated* connection-open microbenchmark. To validate it end-to-end we
added `examples/bench_pipeline.rs`, which drives the **real** ventilator → echo-worker → sink →
finalize → `worker_metadata` loop over a backlog of tiny tasks for a fixed time window, then counts
what flowed through. Same binary, same config (20 000-task backlog, **4 echo workers**, 8 s window);
the only change between arms is reverting the four #4 files to their pre-#4 (`HEAD~1`) versions.

| Arm | tasks/s | PG connection errors (8 s) | worker-metadata recorded | process |
|---|---|---|---|---|
| **pooled (#4, HEAD)** | ~2425 | **0** | ~19 391 dispatched (intact) | clean exit |
| **unpooled (pre-#4)** | ~2500\* | **15 500** | **245** dispatched (~98.8 % **dropped**) | **crashed (SIGSEGV)** |

\* Task *throughput* looks similar because the metadata writes are fire-and-forget (off the dispatch
critical path) — so raw tasks/s is **not** where the damage shows. The damage is systemic:

- The unpooled path opens **2 fresh `PgConnection`s per task** (one in `record_dispatched`, one in
  `record_received`). At ~2400 tasks/s that is ~**9 600 connection opens/s**, which in 8 s produced
  **15 500 hard failures**: Postgres `FATAL: sorry, too many clients already` (it **exhausted
  `max_connections=100`**), `remaining connection slots are reserved for ... SUPERUSER` (it ate the
  reserved slots too — i.e. it would **take the frontend and every other DB client down with it**),
  and `Cannot assign requested address` (it **exhausted ephemeral TCP source ports**).
- `connection_at` uses `PgConnection::establish(...).expect(...)`, so each failure **panics in its
  detached thread and silently drops that metadata write** — **~98.8 %** of metadata was lost (245 of
  ~20 000), and the thread+connection storm ultimately **crashed the process (SIGSEGV, exit 139)**.
- The pooled arm: **zero** connection errors, metadata intact, stable — connections bounded by the
  pool (`pool_size=32`) regardless of task rate.

**Conclusion.** #4 is not merely a latency win; the pre-#4 per-event-connection pattern is a
**fault-injection bomb** under pipeline load — it exhausts the shared Postgres connection pool +
OS ports and crashes, destabilising the *whole* deployment. Pooling converts an O(tasks) connection
storm into a bounded, reused set. This is the first concrete proof point for the
[maximum-robustness](../DESIGN_PRINCIPLES.md) mandate; see also [`KNOWN_ISSUES.md`](../KNOWN_ISSUES.md) for
the residual liabilities this run surfaced (the metadata **thread-per-event spawn** still remains —
only the *connection* is pooled, not the thread — and the metadata read-before-insert **race** drops
the `returned` counts even in the pooled arm).

*Sandbox note:* this dev box's seccomp limits capped what could be measured here — an in-process
`pg_stat_activity` connection sampler tripped the sandbox (SIGSTKFLT), so peak-backend counts were
read from the Postgres `FATAL` messages instead. The definitive 200-worker / 100–200 tasks/s run
belongs on the real `cortex` box; the failure modes above already reproduce decisively at 4 workers.
