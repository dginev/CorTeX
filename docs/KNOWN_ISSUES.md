# CorTeX Known Issues — resilience & correctness ledger

> Running list of known problems, weighted toward **robustness/fault-tolerance** gaps (see
> [`DESIGN_PRINCIPLES.md`](DESIGN_PRINCIPLES.md)). Owner's direction: **record every known problem as
> we find it; we go back and solve them all at the end.** Do not silently fix-and-forget, and do not
> silently leave a discovered gap unrecorded.
>
> Status legend: 🔴 open · 🟡 partially mitigated · 🟢 resolved (kept for history).
> Severity: **S1** can crash/corrupt/destabilise the system · **S2** drops or hides work ·
> **S3** correctness/UX papercut · **S4** cleanup/polish.

## Dispatcher / pipeline

| # | Sev | Status | Issue |
|---|---|---|---|
| D-1 | S1 | 🟡 | **Per-event connection storm in `worker_metadata`.** `record_dispatched`/`record_received` formerly opened a fresh `PgConnection` per ZMQ event; under load this exhausted `max_connections` + ephemeral ports and **crashed the process**, dropping ~99 % of metadata (proven, `RESOURCE_RATIONALIZATION.md`). **#4 pooled the connection** (🟢 for the connection), but the **thread-per-event `thread::spawn` remains** (🔴) — at 200 tasks/s that is ~400 detached threads/s of unbounded churn. Fix: a bounded metadata writer (worker pool or batched on the finalize cadence). |
| D-2 | S2 | 🔴 | **Metadata read-before-insert race.** `record_received` (sink thread) can run `find_by_name` before `record_dispatched` (ventilator thread) has inserted the worker row → `"Can't record worker metadata for unknown worker"` → the update is lost. Observed dropping ~half of `total_returned` even in the pooled arm. Fix: upsert (`ON CONFLICT`) keyed by `(name, service_id)`, or serialise per-worker metadata. |
| D-3 | S1 | 🔴 | **`.expect()`/`.unwrap()`/`panic!` on dispatch paths.** `connection_at` does `establish(...).expect(...)`; the ventilator does `.bind().unwrap()`; sink/finalize/manager `panic!` on thread errors. Any of these turns a transient fault into a crash or a silently-dead thread. Violates principle #2. Fix: convert to `Result` + log + continue; reserve panics for true invariants. (Note: the `establish` error message is also a **bug** — `"Error connecting to {address}"` is a literal, the `{address}` is never interpolated.) |
| D-4 | S2 | 🔴 | **Ventilator empty-message fragility** (pre-existing, code-commented). A rare "3 adjacent empty messages" mode permanently shuffles ROUTER state; the manager works around it by **restarting the ventilator thread in a loop**. Needs a root-cause fix, not a restart band-aid. |
| D-5 | S2 | 🔴 | **`job_limit` lockstep termination hangs.** With a finite `job_limit`, mock-replies (taskid 0 when the queue momentarily empties) desync the ventilator/sink/finalize counters, so the pipeline can **hang on shutdown** (hit while writing `bench_pipeline.rs`). Clean, deterministic drain/shutdown is missing. |
| D-6 | S2 | 🔴 | **No backpressure / bounded in-flight set.** The ventilator fire-and-forgets; nothing bounds in-flight tasks or the metadata-writer fan-out. Under overload it consumes shared resources until something breaks (see D-1). Fix: bounded queues + backpressure (principle #4). |
| D-7 | S3 | 🔴 | **Single sink thread does blocking result-archive writes** to slow `/data` QLC RAID6 — serialises I/O at high task rates. Tracked as Arm 14 #1/#2 (NVMe staging + optional write thread-pool). |
| D-8 | S3 | 🔴 | **`mark_done` blind delete+reinsert** of all five `log_*` tables per task on every finalize, even when logs are unchanged — write amplification on the hot path. Tracked as Arm 14 #3 (diff/upsert). |

## Worker / task handling

| # | Sev | Status | Issue |
|---|---|---|---|
| W-1 | S1 | 🔴 | **No per-task timeout / resource cap at the worker boundary.** `latexml-oxide` can hang, OOM, or segfault on hostile input; nothing here time-boxes or memory-caps a task or guards against decompression bombs / oversized results. Retry budget exists in the ventilator but task-level isolation does not. Principles #3, #6. |
| W-2 | S3 | 🔴 | **Non-UTF-8 / malformed worker logs** are handled ad hoc in `generate_report` (defaults to Fatal). Needs a single, well-tested tolerant log parser as the contract with unpredictable workers. |
| W-3 | S4 | 🟡 | **Worker identity is self-assigned** (`hostname:service:pid`-style), overriding the configured `identity` field — fine today, but means metadata keys are not operator-controlled. |

## Reports / storage

| # | Sev | Status | Issue |
|---|---|---|---|
| R-1 | S2 | 🟢 | **Hard Redis dependency for reports — removed.** Aggregate reports were O(millions of log rows) shielded by a Redis cache (staleness + an extra daemon). Arm 14 #6 replaced it with the `report_summary` materialized view: **#6.1** the read model + matview + contract test; **#6.2** wired `task_report`'s category/`what` grains to the rollup (`category_grain_from_rollup`/`what_grain_from_rollup`, sharing `aux_task_rows_stats` with — and pinned equivalent to — the retained live `task_report_live`), refreshes it on the run-completion path (finalize **drain + at-least-daily**, plus `mark_new_run`), and **dropped the `redis` crate** (`cache_worker` + the boot `.expect()` are gone — the frontend now boots without Redis). |
| R-4 | S3 | 🔴 | **`report_summary` uses non-concurrent `REFRESH`** (brief `ACCESS EXCLUSIVE` lock). The refresh cadence is now run-completion **plus at-least-daily** while long runs are in flight (a single conversion run can take weeks; `finalize.rs`), so the lock is taken more often — `REFRESH ... CONCURRENTLY` (needs a UNIQUE index disambiguating the ROLLUP `NULL`s, e.g. on `category_is_total`/`what_is_total` + `NULLS NOT DISTINCT`, PG15+) is the follow-up. |
| R-2 | S3 | 🔴 | **`tasks.entry varchar(200)`** length cap — long arXiv paths could exceed it; silent truncation risk. Audit + widen. |
| R-3 | S4 | 🔴 | **Reports return `Vec<HashMap<String,String>>`** (stringly-typed) rather than DTOs — fragile contract for the agent-first API (Arm 8/9). |

## Tooling / environment

| # | Sev | Status | Issue |
|---|---|---|---|
| E-1 | S4 | 🟡 | **CI refreshed to current requirements** (`/.github/workflows/CI.yml`): Postgres + roles, nightly via `dtolnay/rust-toolchain` (the `actions-rs/*` actions are archived), diesel_cli 2.x migrations on both DBs, **no Redis**, and `fmt --check` + `clippy -D warnings` gates mirroring `.githooks/`. **Still pending:** publishing API docs + rustdoc to GH Pages (Arm 9/12), and the L-1 teardown flake can still red the run. |
| E-2 | S4 | 🟢 | *(dev-env note, not a product bug)* The sandbox's seccomp filter kills a process that polls `pg_stat_activity` in a tight loop (SIGSTKFLT) and the harness's background-run wrapper signals long jobs — so in-sandbox load tests must run foreground without a live connection sampler. Recorded so the next session doesn't re-discover it. |

## Frontend / routes

| # | Sev | Status | Issue |
|---|---|---|---|
| F-1 | S2 | 🟡 | **Legacy binary report/diff routes panic on bad input.** The binary's `diff_historical_summary`/`diff_historical_tasks` routes do `NaiveDateTime::parse_from_str(date).unwrap()` on a user-supplied query param — a malformed date **panics the request** (violates principle #2, no panic on the request path). The migrated library twins handle it (`runs::api_run_diff` returns `400`); 🟡 until the legacy routes are removed in favor of the library surface. Audit the other legacy bin routes for the same pattern as they migrate. |

## Process lifecycle / shutdown

| # | Sev | Status | Issue |
|---|---|---|---|
| L-1 | S2 | 🔴 | **Flaky at-exit SIGSEGV in DB-pool processes.** Integration binaries that build a Rocket `Client` over an r2d2/libpq pool (and the `jobs::spawn_job` detached-thread path) intermittently SIGSEGV **during process teardown, after all their tests pass**. Reproduced on a clean `master` checkout (so pre-existing, not from the rollup/Redis work): ~3–5 of 5 runs for `jobs_api_test` / `management_api_test`, and **never under gdb** → a thread/connection teardown-ordering race, not a logic bug. It aborts `cargo test` (and can red CI) even though the assertions passed; the bench works around the same class with `process::exit(0)`. Fix is clean shutdown: join spawned job threads and drop the pool before exit rather than racing detached threads against libpq teardown (no unbounded detached threads — principles #1/#3). |

---

*Add new findings here the moment they're discovered, with a stable ID, severity, and a one-line
fix direction. Promote 🔴→🟢 (don't delete) when fixed, with the commit that did it.*
