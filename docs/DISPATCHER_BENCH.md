# Dispatcher quality benchmark

`examples/dispatcher_bench.rs` is the **canonical, long-term dispatcher benchmark** — it measures
**perf** *and* asserts **robustness** so a regression in either fails loudly. Run it after any
dispatcher change and compare against the baselines below.

It drives the *real* `TaskManager` (ventilator → sink → finalize) over a real `pericortex::EchoWorker`
fleet against the test DB — not a transport spike — and **drains a fixed backlog to completion**, so
"N tasks in T seconds" is directly comparable across commits.

## Run

```bash
cargo run --release --example dispatcher_bench            # defaults: 20000 tasks, 4 workers, 8 KB
BENCH_TASKS=50000 BENCH_WORKERS=8 BENCH_PAYLOAD_KB=64 \
  cargo run --release --example dispatcher_bench
```

Exit code is **0 only if every correctness gate passes** (so it doubles as a heavy integration test).
Knobs: `BENCH_TASKS`, `BENCH_WORKERS`, `BENCH_PAYLOAD_KB` (source/result archive size), `BENCH_DEADLINE_S`,
`BENCH_JSON=1` (one-line JSON record for tracking over time), `BENCH_LABEL`, `BENCH_CHAOS` (the
churn-recovery gate — see below).

## What it asserts (robustness gates)

Payloads are *valid* result `.zip`s carrying a `cortex.log` that derives to `NoProblem`, one per-task
subdir (the arXiv topology), so the per-task result-parse hot path runs for real. After draining:

- **No loss** — all `N` tasks reach a **terminal** status (`status < 0`); none left `TODO` or `Queued`.
- **Parse correctness** — the status distribution is exactly `N × NoProblem` (a parse-path regression,
  e.g. a broken `cortex.log` read, shows up as Fatals).
- **Drains within the deadline** — a stuck/lost task makes it time out (a real failure, not "slow").

`worker_metadata` totals are **reported but not asserted** — the D-1 writer is intentionally
best-effort (drops under saturation), so its counts would flake.

## Baselines (2026-06-14, dev box: 128 cores, NVMe test DB; release build)

| Config | tasks/s | MB/s (src+result) | gates |
| --- | --- | --- | --- |
| 20000 tasks · 4 workers · 8 KB | **~10,900** | ~170 | ✓ pass |
| 5000 tasks · 4 workers · 256 KB | ~3,500 | **~1,770** | ✓ pass |
| 20000 tasks · 8 workers · 8 KB | ~9,800 | ~155 | ✓ pass (18/18 after the D-10 fix) |

These are loopback/in-process numbers (worker + dispatcher + DB on one box) — they bound *relative*
regressions, not absolute production throughput (which is network + `/data` disk bound). The headline
metric to watch over time is **tasks/s at the 4-worker baseline** and **MB/s at the fat-payload
config**.

## ✅ Resolved finding — single-task loss under higher worker concurrency (2026-06-14)

The **8-worker** run used to lose **exactly one task** (`19999/20000`, `Queued 1`): leased but never
finalized, so it sat `Queued` until the ≥1 h reaper — past the bench deadline. The 4-worker run was
clean, so it surfaced only under higher concurrency. This bench was the repro harness; running the
8-worker config in a loop reproduced it at **~25 % of runs (2/8)**.

**Root cause (D-10) — not the originally-suspected D-4.** The ventilator recorded the lease in the
in-flight `progress_queue` (`push_progress_task`) **after** streaming the payload to the worker. With a
fast echo worker the result could reach the **sink before that record existed** → the sink's
`pop_progress_task` missed it → it **discarded the result** → the task stranded `Queued`. A
check-then-act ordering race; the window widens with worker count (more concurrent echoes lapping the
ventilator), which is exactly why it tracked concurrency.

*(The earlier sink **envelope hardening** — every result `RCVMORE`-checked `[identity, service, taskid,
…data]` — was a real, independent fix for short/malformed replies desyncing the framing, and is kept;
it narrowed but did not close this loss because the true cause was the dispatch-ordering race, not a
desync.)*

**Fix:** record the lease **before** the payload send (`ventilator.rs` — `push_progress_task`
immediately after `task_queue.pop()`). The push completes before the first content frame is sent, so a
worker can't return a result before the task is tracked — the race is eliminated, not narrowed.
**Verified:** **18 consecutive clean runs** at the previously-failing concurrencies (12×8-worker +
6×16-worker, 20000 tasks, 0 loss). The 8-worker config is now a standing correctness gate. (Ledger:
KNOWN_ISSUES **D-10**.)

## Finalize batch-size (N) knee — `finalize_batch_size` tuning (2026-06-14)

The phase-2 DB-coalescing knob `dispatcher.finalize_batch_size` (N) was tuned with this bench
(`CORTEX_DISPATCHER__FINALIZE_BATCH_SIZE=<N>`, 20000 tasks · 4 workers · 8 KB, two trials each):

| N | tasks/s (trial 1 / 2) | note |
| --- | --- | --- |
| 256 | 8900 / 9834 | below the knee; dips |
| 512 | 8944 / 9836 | below the knee; dips |
| **1024** | **9852 / 9846** | **knee — tightest, reliably high** |
| 2048 | 10963 / 9848 | marginal/noisy gain over 1024 |
| 4096 | 8217 / 8222 | **regresses** — a 4096-row transaction holds row locks long enough to stall the pipeline |

So **N = 1024** is the chosen default: it captures the throughput gain with the lowest run-to-run
variance, sits clear of the 4096 cliff, and bounds worst-case crash *re-work* to ~1024 tasks. Under
saturation the size threshold fires (batches of exactly N in one ~8 ms transaction, ~17 writes/s);
at steady-state load the **T** time window (`finalize_flush_ms`, default 300 ms) fires first. N only
governs the burst/saturation regime — row locks don't pressure `max_locks_per_transaction`, so it is
safe to raise, but past ~2048 the long transaction backs up the pipeline faster than it drains.

## Relationship to `bench_pipeline.rs`

`bench_pipeline.rs` is the older, narrower A/B harness for the Arm-14 worker-metadata pooling change
(fixed-window, no correctness gates). `dispatcher_bench.rs` supersedes it for general perf + robustness
regression tracking; keep `bench_pipeline` for that specific historical A/B.

## Chaos / churn-recovery gate (`BENCH_CHAOS`)

```bash
BENCH_TASKS=2000 BENCH_CHAOS=50 BENCH_WORKERS=4 BENCH_DEADLINE_S=120 \
  cargo run --release --example dispatcher_bench
```

With `BENCH_CHAOS=<n>`, before the real workers start, a **saboteur** (a raw ZMQ `DEALER`) leases `n`
tasks from the ventilator and **dies without returning any result** — simulating workers that crash
mid-task. Those `n` tasks are stranded in the dispatcher's in-flight set; only the **lease/visibility-
timeout reaper** can recover them (re-lease → a live worker finishes them). The bench then asserts the
**same no-loss / all-terminal / N×NoProblem gates** — so a regression in the reaper, the lease timeout,
or the re-queue routing shows up as stranded `Queued` tasks at the deadline.

This is the reaper-recovery path made a **standing gate** in the canonical bench (previously only the
throwaway `zmq_resilience` spike covered churn). It unblocked once the lease/visibility timeout +
reaper interval became `DispatcherConfig` knobs: `BENCH_CHAOS` auto-sets
`CORTEX_DISPATCHER__LEASE_TIMEOUT_SECONDS=2` + `CORTEX_DISPATCHER__REAP_INTERVAL_SECONDS=2` (override
either) to compress the hour-scale production timing into seconds. Validated: 50 stranded → **2000/2000
finalize, 0 lost**.

> **Timing caveat:** recovery wall-clock is dominated **not** by the (fast) dispatcher reap timing but
> by the **worker's empty-queue throttle** — `pericortex` workers `sleep(60s)` on a mock/empty reply
> (`worker.rs:216`). Once the non-stranded tasks drain and the queue empties, workers back off 60 s
> before polling again, and the reaper only sweeps *on a request* — so a small chaos run takes ~60 s
> even with a 2 s lease. The gate still proves **no loss**; making that throttle a `pericortex` config
> knob (it's a hardcoded constant today) would let the gate run in seconds — tracked in OPEN_QUESTIONS.

## Extending (future)

- **Latency percentiles** (dispatch→finalize) would complement throughput.
- **Throttled-disk mode** to surface the D-7 sink-write ceiling on fast loopback (today the NVMe test
  DB + small payloads hide it; phase 3's fan-out needs a way to *show* its benefit here).
