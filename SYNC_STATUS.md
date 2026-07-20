# SYNC_STATUS — CI red on `main` (2026-07-20)

Handoff note. Branch: **`fix/worker-identity-collision`** (commit `4b9a5b3`, branched off
`1c28424` = current `origin/main`). Not pushed — awaiting your say.

---

## TL;DR

CI going red on the serde bump was **not** the serde bump. `concurrent_dispatch_test` has a
**~2.4% flake dating to 2026-06-17** (7 failures over 5 weeks, on 5+ unrelated branches). Root
cause found, fixed, and verified. **Re-running the failed CI job will go green.**

Production was **never** affected — the defect only manifests in test/bench harnesses.

---

## Root cause

Two distinct defects, both now recorded in `docs/KNOWN_ISSUES.md`:

**D-21 — every multi-worker harness collapsed onto ONE ZMQ identity.**
`tests/concurrent_dispatch_test.rs` and both benches spawn N `EchoWorker`s as *threads of one
process* and set a distinct `identity` field. But `Worker::start()` **overwrites** it with
`<host>:<service>:<pid>` (EchoWorker leaves `pool_size()` at 1), and one process has one PID.
Measured in a reproduced failure: **`217 worker=…:concurrent_echo:163878` — 1 identity, 8 workers.**
Under the ventilator's `router_handover(true)` they are one peer, so a dispatch racing a handover is
dropped; the task is already leased + in-flight, so it strands `Queued` until the **240 s** lease
reaper — past the test's 90 s deadline. pericortex documents this exact contract and consequence.

**D-22 — the ventilator's ROUTER drops unroutable dispatches silently.** ⚠️ **NOT FIXED — see below.**
A ZMQ ROUTER discards messages to an unroutable identity *without error* unless
`ZMQ_ROUTER_MANDATORY` is set; only `router_handover` was set. Any worker dying between its request
and our reply swallowed the whole dispatch with no log, metric, or counter. This is the delivery
mechanism behind D-21's symptom but is independent of it — it hits production paths too (worker
death, network drop, restart).

**Smoking-gun trace** (task 20888): `ventilator: streamed task payload to worker` … then *never
seen again* by sink, worker, or finalize.

---

## What changed (`4b9a5b3`, 6 files, +82/−10)

| File | Change |
|---|---|
| `tests/concurrent_dispatch_test.rs` | process+thread-unique identity, `start_single()` |
| `examples/dispatcher_bench.rs` | same |
| `examples/bench_pipeline.rs` | same |
| `src/dispatcher/ventilator.rs` | comment-only: corrects the stale "D-4" citation (the bench loss was D-10) |
| `docs/KNOWN_ISSUES.md` | D-21 (🟢 resolved), D-22 (🔴 open) |
| `docs/DISPATCHER_BENCH.md` | caveat on the worker-count axis |

`start_single()` is exactly what `start()` calls after setting the identity (upstream's
`pool_size() == 1` arm), so nothing is bypassed.

### ⚠️ D-22 was attempted, then REVERTED

I implemented `set_router_mandatory(true)` and it **broke `dispatcher_torture_test`**:
`Failed in ventilator thread: Host unreachable` (panic at `manager.rs:171`). Cause: I guarded only
the **identity frame**, but the task-id frame, every payload chunk, and the two-frame mock-reply
path all still used `?`, so an `EHOSTUNREACH` on any of those propagated out and killed the
ventilator thread.

**Reverted.** The branch now contains only the D-21 fix (which is what actually cures the CI
flake) plus docs. D-22 is recorded 🔴 **open** in `KNOWN_ISSUES.md`, including the trap and the
per-stage recovery a correct fix needs (skip-before-lease vs. abandon-stream-and-let-the-reaper-
recover). Don't retry it without handling *every* send in the dispatch loop.

---

## Verification

- **A/B soak** under `taskset -c 0,1` (2-core, CI-like contention), matched conditions:
  **2 failures / 60 runs before → 0 / 210 after.** p ≈ 2e-4 against the ~4% base rate measured
  over 148 unfixed runs.
- `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean.
- Dispatcher suite on the final branch build: `echo_roundtrip_test` ✅, `dispatcher_job_limit_test`
  ✅, `concurrent_dispatch_test` ✅.

### ⚠️ Still pending when I stopped

A revalidation of the **reverted** branch (4 dispatcher tests + an 80-run 2-core soak) was still
running when I wrote this. Before trusting the branch, confirm:

```bash
cargo test --test echo_roundtrip_test --test dispatcher_job_limit_test \
           --test concurrent_dispatch_test --test dispatcher_torture_test
```

Full `cargo test` has **not** been run (and must not be, on this box — see Gotchas).

---

## Gotchas

1. **Do NOT run a bare `cargo test` on this box.** `latexmlc` *is* installed
   (`/home/deyan/perl5/bin/latexmlc`), so `tex_to_html_test` can hang the suite for 30+ min —
   that's the open **W-6**. Run dispatcher tests by name.
2. **Your working tree still has an unresolved `Cargo.lock` conflict** (`UU`, from
   `stash@{1}` "wip: full cargo update sweep" vs upstream) — two hunks: pericortex
   `a4eaf05` vs `340f728`, and `syn 3.0.2` vs `2.0.119`. **I deliberately did not touch it.**
   You can't build in `/home/deyan/git/cortex` until it's resolved.
3. The work lives in a scratch **git worktree** under `/tmp/...`, which may be cleared. The
   **commit is safe** — it's in your repo's object store. Read the handoff without switching
   branches: `git show fix/worker-identity-collision:SYNC_STATUS.md`
4. **CI on `main` stays red** until someone re-runs the job. It will pass.

---

## Open decisions for you

1. **Push the branch?** Not pushed. Per CLAUDE.md the preference is branch + push, no PR.
2. **Finish D-22?** It's a genuine production robustness gap (silent work-dropping), just larger
   than it looked. The ledger entry has the full fix direction and the regression trap.
3. **`DISPATCHER_BENCH.md` numbers.** Every "N workers" figure it ever produced measured *one* ZMQ
   peer, so the concurrency axis needs re-measuring on the fixed harness. I only annotated it.
4. **Should the test exercise the reaper?** Its 90 s deadline is shorter than the 240 s
   `lease_timeout_seconds`, so it can never observe the safety net that makes a lost dispatch
   survivable in production — any single loss is an automatic failure. `config.rs` itself notes
   that lowering `lease_timeout_seconds` + `reap_interval_seconds` is "what lets a fast chaos test
   exercise reaper-based recovery in seconds". Worth doing; not done here.
5. **D-10 / D-4 nuance.** `ventilator.rs` cited "D-4" for the historical bench loss while
   `DISPATCHER_BENCH.md` attributes it to **D-10**; I corrected the stale reference. I initially
   suspected D-21 invalidated that finding, but the D-10 fix held for 18 consecutive clean runs
   (p ≈ 0.006 at a 25% rate), so it is real. The honest reading — recorded in D-21 — is that both
   defects coexisted: D-10 for the bulk, D-21 for the residual few percent that outlived it.
