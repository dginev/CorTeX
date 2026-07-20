# SYNC_STATUS — CI red on `main` + dispatcher silent-drop gap (2026-07-20)

Handoff note. Branch: **`fix/worker-identity-collision`** (branched off `1c28424` = current
`origin/main`; now **two commits** — D-21 harness fix, then the D-22 dispatcher fix). Not pushed —
awaiting your say.

---

## TL;DR

1. **CI flake — fixed (D-21).** CI going red on the serde bump was **not** the serde bump.
   `concurrent_dispatch_test` had a **~2.4% flake dating to 2026-06-17** (7 failures over 5 weeks, on
   5+ unrelated branches). Root-caused and fixed. **Re-running the failed CI job will go green.**
2. **The production gap behind it — now also fixed (D-22).** The delivery mechanism under the flake
   was a genuine **production** robustness hole: the ventilator's ROUTER *silently dropped* any
   dispatch to a worker that had vanished, stranding the task for a full 240 s+ lease timeout with
   zero observability. That is now closed — an unroutable dispatch is caught, logged, counted, and
   the task stays queued for the next worker (no strand). Both defects are 🟢 in
   `docs/KNOWN_ISSUES.md`.

---

## Root cause (both defects)

**D-21 — every multi-worker harness collapsed onto ONE ZMQ identity.** *(fixed, commit 1)*
`tests/concurrent_dispatch_test.rs` and both benches spawn N `EchoWorker`s as *threads of one
process* and set a distinct `identity` field. But `Worker::start()` **overwrites** it with
`<host>:<service>:<pid>` (EchoWorker leaves `pool_size()` at 1), and one process has one PID.
Measured in a reproduced failure: **`217 worker=…:concurrent_echo:163878` — 1 identity, 8 workers.**
Under `router_handover(true)` they are one peer, so a dispatch racing a handover is dropped; the task
is already leased + in-flight, so it strands `Queued` until the **240 s** reaper — past the test's
90 s deadline. Fix: each harness worker now gets a process- *and* thread-unique identity
(`<host>:<service>:<pid>-<NN>`) + `start_single()`. **Production was never affected** —
`scripts/run_worker.sh` runs one *process* per slot with a distinct `stable_identity_suffix`.

**D-22 — the ventilator's ROUTER dropped unroutable dispatches silently.** *(fixed, commit 2)*
A ZMQ ROUTER discards a message to an unroutable identity *without error* unless
`ZMQ_ROUTER_MANDATORY` is set; only `router_handover` was. Any worker dying between its request and
our reply swallowed the whole dispatch with no log, metric, or counter, then the (already-leased)
task sat unheld until the reaper — **in production** (worker death, network drop, restart), not just
the harness. **Smoking-gun trace** (task 20888): `ventilator: streamed task payload to worker` …
then never seen again by sink, worker, or finalize.

---

## What changed

**Commit 1 (D-21, harness):** `tests/concurrent_dispatch_test.rs`, `examples/dispatcher_bench.rs`,
`examples/bench_pipeline.rs` (unique identity + `start_single()`); `docs/DISPATCHER_BENCH.md` (caveat
on the untrustworthy worker-count axis); a comment fix in `ventilator.rs`.

**Commit 2 (D-22, dispatcher):**

| File | Change |
|---|---|
| `src/dispatcher/ventilator.rs` | `set_router_mandatory(true)` + a `route_worker_frame` helper guarding the routing frame at all **three** send sites (unknown-service mock-reply, backpressure mock-reply, real dispatch): `EHOSTUNREACH` → rate-limited `warn!` + count + `continue`. |
| `docs/KNOWN_ISSUES.md` | D-22 → 🟢 resolved, with the empirical correction (below). |
| `SYNC_STATUS.md` | this update. |

### How D-22 was done right (the reverted attempt's trap, corrected)

The first attempt set `set_router_mandatory(true)` but guarded only the real-dispatch identity frame,
so `dispatcher_torture_test` aborted with `Failed in ventilator thread: Host unreachable`. The old
ledger blamed "body frames still using `?`" and prescribed "handle *every* send." **An empirical
libzmq-4.3.5 probe corrected that:**

- Under `ROUTER_MANDATORY`, **only the routing (first) frame** ever returns `EHOSTUNREACH`.
- A rejected routing frame is **never started as a multipart**, so the socket stays cleanly at
  message-start — a fresh reply to a live peer routes correctly on the very next send. **No desync.**
- So the revert didn't fail from desync; it crashed by letting `EHOSTUNREACH` **propagate via `?`**
  on the *unguarded* routing sends — the two mock-reply paths, plus each following `SNDMORE` frame
  (after a failed routing frame, libzmq re-interprets the next frame as a new, also-unroutable
  routing frame).

**Therefore guarding the routing frame at every site and `continue`-ing before the body frames is
necessary and sufficient.** The body frames keep `?` (after a *successful* route they can't surface
`EHOSTUNREACH`, and the blocking socket can't return `EAGAIN`). Bonus: the real dispatch's routing
frame is sent **before** the `pop()` lease, so an unroutable worker there is caught having leased
nothing → the task stays queued for the next worker (better than the reaper fallback the ledger
planned). Only a worker dying *mid-payload-stream* (routing frame already sent) still falls to the
reaper — and the task is recorded in-flight before streaming, so it's recovered.

---

## Verification

- **D-21:** A/B soak under `taskset -c 0,1`: **2 fails / 60 before → 0 / 210 after** (p ≈ 2e-4).
- **D-22:** `dispatcher_torture_test` (the `vent_flood` trap-exposer) passes with **255
  `EHOSTUNREACH` events caught + handled** from the disconnecting `torture-flood-peer` — the reverted
  attempt aborted on the first. `echo_roundtrip_test` / `dispatcher_job_limit_test` /
  `concurrent_dispatch_test` all green; a fresh **2-core `concurrent_dispatch_test` soak: 60/60**.
- `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean.

Full `cargo test` has **not** been run (and must not be, on this box — see Gotchas).

---

## Gotchas

1. **Do NOT run a bare `cargo test` on this box.** `latexmlc` *is* installed, so `tex_to_html_test`
   can hang the suite for 30+ min — the open **W-6**. Run dispatcher tests by name.
2. **The main working tree (`/home/deyan/git/cortex`) still has an unresolved `Cargo.lock` conflict**
   (`UU`, pericortex `a4eaf05` vs `340f728`, `syn 3.0.2` vs `2.0.119`). **Untouched — you can't build
   there until it's resolved.** All work here was done in a scratch worktree under `/tmp/...` on the
   branch, which builds cleanly (the branch's `Cargo.lock` is the clean one off `main`).
3. The scratch worktree may be cleared; the **commits are safe** in the object store. Read this
   without switching branches: `git show fix/worker-identity-collision:SYNC_STATUS.md`
4. **CI on `main` stays red** until someone re-runs the job. It will pass.

---

## Open decisions for you

1. **Push the branch?** Not pushed. Per CLAUDE.md the preference is branch + push, no PR.
   *(D-22 is a production dispatcher change — you may want it on its own branch off `main` rather than
   riding on the harness branch; say the word and I'll split them.)*
2. **`DISPATCHER_BENCH.md` numbers.** Every "N workers" figure it ever produced measured *one* ZMQ
   peer, so the concurrency axis needs re-measuring on the fixed harness. Only annotated so far.
3. **Should the test exercise the reaper?** `concurrent_dispatch_test`'s 90 s deadline is shorter
   than the 240 s `lease_timeout_seconds`, so it can never observe the safety net. Lowering
   `lease_timeout_seconds` + `reap_interval_seconds` (config.rs notes this is "what lets a fast chaos
   test exercise reaper-based recovery in seconds") would let a chaos test prove the mid-stream →
   reaper recovery that D-22 now leans on. Worth doing; not done here.
4. **D-10 / D-4 nuance** (unchanged from before). `ventilator.rs`'s stale "D-4" citation was
   corrected to D-10; the D-10 fix held for 18 consecutive clean runs (p ≈ 0.006), so it's real. Both
   defects coexisted: D-10 for the bulk of the historical bench loss, D-21 for the residual.
