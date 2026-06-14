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
`BENCH_JSON=1` (one-line JSON record for tracking over time), `BENCH_LABEL`.

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
| 20000 tasks · 8 workers · 8 KB | — | — | **✗ see below** |

These are loopback/in-process numbers (worker + dispatcher + DB on one box) — they bound *relative*
regressions, not absolute production throughput (which is network + `/data` disk bound). The headline
metric to watch over time is **tasks/s at the 4-worker baseline** and **MB/s at the fat-payload
config**.

## ⚠ Open finding — single-task loss under higher worker concurrency

The **8-worker** run loses **exactly one task** (`19999/20000`): it is leased but never finalized, so
it sits `Queued` until the ≥1 h visibility-timeout reaper — past the bench deadline. The 4-worker run
is clean, so this surfaces only under higher concurrency. Leading suspects:

1. **D-4** — the ventilator's "3 adjacent empty messages" fragility (the manager restarts the
   ventilator thread; a task leased right at the restart boundary could be stranded).
2. A **sink/worker envelope desync** — a short/malformed multipart reply desyncing the sink's
   `[identity, service, taskid, …data]` framing, mis-attributing or dropping a result.

**Update (2026-06-14):** the sink **envelope hardening** landed — every result is now `RCVMORE`-checked
`[identity, service, taskid, …data]`, so a short/empty/malformed reply is skipped without desyncing the
*next* reply's framing (this is real, and is the fix for case (2)). It **helps but does not fully
close** the 8-worker loss: re-runs are now intermittent (~1 in 2 at 8 workers passes/fails), where
before it failed consistently. So a **deeper, racy single-task-loss remains** — leading suspect **D-4**
(the ventilator-restart boundary stranding one in-flight task). Still open; needs a dedicated repro
(the bench is the repro harness — run the 8-worker config in a loop). **Tracked here so it isn't lost.**

## Relationship to `bench_pipeline.rs`

`bench_pipeline.rs` is the older, narrower A/B harness for the Arm-14 worker-metadata pooling change
(fixed-window, no correctness gates). `dispatcher_bench.rs` supersedes it for general perf + robustness
regression tracking; keep `bench_pipeline` for that specific historical A/B.

## Extending (future)

- **Chaos / churn recovery** needs a *configurable* visibility timeout (today ≥1 h, so reaper-based
  recovery can't be exercised in a fast bench). When the lease timeout becomes a `DispatcherConfig`
  knob (a phase-2+ item), add a mode that kills workers mid-drain and asserts every task still lands.
- **Latency percentiles** (dispatch→finalize) would complement throughput.
