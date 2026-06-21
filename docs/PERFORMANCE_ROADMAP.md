# CorTeX Long-Term Performance and Robustness Roadmap

**Status:** OPEN. Reviewed 2026-06-20, tightened to non-speculative work. (Was `HANDOFF.md` at the repo
root; relocated here as a standing roadmap once the original perf-opt easy-win handoff was completed.)

This roadmap only lists improvements that are certain to help CorTeX long-term because they address observed workload behavior, current code structure, or already-documented operational limits. It intentionally excludes minor cleanup and deployment-dependent ideas.

## Performance Review

### P1. Add end-to-end pipeline timing before further throughput work

**Certain value:** The current dispatcher has multiple real shared stages: source streaming in `src/dispatcher/ventilator.rs`, result receive/write/parse in `src/dispatcher/sink.rs`, report parsing in `src/helpers.rs::generate_report`, and DB persistence in `src/backend/mark.rs::mark_done` through `src/dispatcher/finalize.rs`. The 72-worker latexml-oxide run measured about **13.4 papers/s**, far below the engine-level speedup expected from latexml-oxide, so CorTeX needs stage-level evidence before changing architecture.

**Plan:**

1. Add structured timing for each finalized task: task leased, source stream started/finished, sink received bytes, writer began/finished archive write, `generate_report` began/finished, done queue handoff, `mark_done` transaction began/finished.
2. Emit low-cardinality aggregate metrics: p50/p95/p99 per stage, bytes in/out, finalize batch size, finalize transaction latency, and sink writer utilization.
3. Run the same 10k sandbox at fixed worker counts, then repeat after each major change. This becomes the permanent regression harness for dispatcher throughput.

**Done when:** a run can answer, with numbers, whether the bottleneck is source I/O, sink I/O, log parsing, DB writes, or queueing.

### P2. Compact high-volume log storage

**Certain value:** The current model stores every parsed log message as an independent row across `log_infos`, `log_warnings`, `log_errors`, `log_fatals`, and `log_invalids`. A 10k run already writes roughly 1M+ `log_*` rows, and `loaded_file`/info rows dominate. This is a permanent scaling cost: larger corpora and more complete Rust dependency logging will increase DB size, WAL volume, index churn, finalize latency, report latency, and maintenance work.

**Plan:**

1. Measure current duplication by category, especially `loaded_file`, grouped through tasks by `(corpus_id, service_id, category, what/details)`.
2. Introduce compact storage for repetitive dependency/load facts. A durable shape is:
   - `loaded_files(id, service_id, logical_name, source_kind)`
   - `task_loaded_files(task_id, loaded_file_id)`
3. Keep diagnostic severities in the existing log tables unless measurement shows another high-duplication class.
4. Adapt `generate_report`/`mark_done` so repetitive facts enter the compact path while report pages and APIs keep their current semantics.
5. Keep old `log_infos` rows readable; compact new runs first. Backfill only if historical storage pressure justifies it.

**Done when:** rows written per 10k run, WAL generated per run, DB size growth, and finalize p95 are materially lower with no report fidelity loss.

### P3. Fix status overview aggregation with measured indexing

**Certain value:** `src/backend/reports.rs::progress_report` computes live status counts from `tasks` for each corpus/service. The schema has partial status indexes and a `service_id` index, but no covering `(corpus_id, service_id, status)` index for this query shape. The code path is known and durable; improving it reduces page/API cost for large corpora.

**Plan:**

1. Capture `EXPLAIN (ANALYZE, BUFFERS)` for `progress_report` on the largest corpus/service pairs.
2. If the plan is not already index-efficient, add:

```sql
CREATE INDEX CONCURRENTLY tasks_corpus_service_status_idx
ON tasks (corpus_id, service_id, status);
```

3. After the index exists, replace per-service N+1 calls with one query per corpus: `WHERE corpus_id = $1 GROUP BY service_id, status`.
4. Keep status aggregation separate from `report_grain_cache`; status includes `TODO`, `Queued`, and `Blocked`, not only message-backed severities.

**Done when:** corpus overview/API status counts use one measured efficient query per corpus.

## Robustness Review

### R1. Make lease timeout service-specific

**Certain value:** The current lease timeout is global (`dispatcher.lease_timeout_seconds`, read by `TaskProgress::expected_at`). This already conflicts with mixed worker runtimes: fast latexml-oxide wants a short lease for prompt dead-worker recovery, while slow Perl workers need a much longer lease to avoid false reaping. A per-service lease is a correctness and operations improvement independent of future architecture.

**Plan:**

1. Add nullable `services.lease_timeout_seconds`; `NULL` preserves the global dispatcher default.
2. Store the effective lease timeout in `TaskProgress` at dispatch time so later config changes do not alter already-leased tasks.
3. Update reaper tests to cover two services with different lease durations.
4. Expose the setting in CLI/API/admin service configuration and document recommended values for known worker classes.

**Done when:** one dispatcher can safely serve fast and slow services without either false reaps or unnecessarily delayed recovery.

### R2. Add hard timeouts to blocking admin operations where the backend supports them

**Certain value:** Background jobs are detached in-process threads (`src/jobs.rs::spawn_job`). The job table can mark stale jobs interrupted, but Rust cannot force-cancel a blocked thread. Adding timeouts to blocking calls that support them reduces the chance of leaked job threads and pinned DB connections.

**Plan:**

1. Set PostgreSQL `statement_timeout` and `lock_timeout` on maintenance-job connections before `REINDEX`, `ANALYZE`, report-cache invalidation, and any future long-running SQL job.
2. Make timeout values configurable under `[jobs]` or a maintenance-specific config section.
3. Ensure timed-out jobs finish as `failed` with the failing table/operation in the message.
4. Document the remaining in-process limitation: a non-DB blocking syscall can still require process restart to reclaim the thread.

**Done when:** DB-backed admin jobs cannot block forever on a lock or runaway statement.

### R3. Move detailed storage probing off request paths

**Certain value:** `std::fs` calls can block indefinitely on a hung network mount. The public `/healthz` is already storage-independent, which is correct. The detailed admin health/storage view should also avoid doing blocking filesystem probes directly in request handling.

**Plan:**

1. Add a periodic storage probe that records per-corpus status and timestamp: ok, unreadable, timed out, stale.
2. Serve detailed health from the last recorded probe result instead of calling filesystem metadata APIs in the request.
3. Keep `/healthz` DB-only/storage-independent.
4. If imports are expected to run against network mounts, route them through the same timeout policy or document that hung mounts require frontend restart.

**Done when:** a hung storage mount cannot tie up repeated admin health requests.

### R4. Make finalize DB failure visible and controlled

**Certain value:** `server::mark_done_batch` retries DB persistence three times, then returns an error that causes dispatcher restart. That is crash-consistent because tasks remain recoverable, but it is operationally coarse: repeated DB failures can look like restart loops without enough signal.

**Plan:**

1. Add metrics/log fields for finalize retry count, exhausted retries, batch size, transaction latency, and time since last successful finalize.
2. On exhausted retries, set a shared “persistence unhealthy” state before aborting so the final logs and health surfaces explain why leasing stopped/restarted.
3. Stop leasing new work as soon as persistence is known unhealthy; let in-flight work drain as far as possible before the supervisor restart path takes over.

**Done when:** DB persistence failures are diagnosable from metrics/logs and do not continue leasing avoidable new work during a known persistence outage.

## Suggested Execution Order

1. Add pipeline timing and finalize metrics.
2. Implement per-service leases.
3. Compact high-volume log storage.
4. Measure and fix status overview aggregation.
5. Add DB timeouts for admin jobs.
6. Move detailed storage probing off request paths.

## Validation Gates

- 10k shuffled arXiv run with stage timing enabled and overhead confirmed acceptable.
- Mixed fast/slow service test showing service-specific lease behavior.
- Before/after DB write-volume numbers for compacted log storage.
- `EXPLAIN (ANALYZE, BUFFERS)` before and after the status aggregation index/query change.
- Admin job timeout test using a lock-waiting SQL statement.
- Storage-probe test where a timed-out probe does not block the health request path.
