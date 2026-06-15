# Open questions — decisions made autonomously, for owner review

Recorded while working solo (owner away). These are choices I made with a sensible default so progress
wasn't blocked; each can be revised/refactored on return. Newest first.

## Decisions taken (with a default) — confirm or revise

0. **CORS fix keeps `*` (no origin allowlist).** Arm 13 flagged `src/frontend/cors.rs` pairing
   `Access-Control-Allow-Origin: *` with `Access-Control-Allow-Credentials: true` (spec-invalid +
   unsafe). I fixed it by **dropping the credentials header only**, keeping `*`. Rationale: the public
   surface is read-only public data; agents authorize via the explicit `X-Cortex-Token` header (not
   ambient cookies) and the admin UI is same-origin — so no legitimate consumer needs credentialed
   cross-origin, and `*`-without-credentials is the standard correct posture for a public read API. The
   plan's "origin allowlist replacing `*`" would only *restrict who can read public data* (pointless)
   while breaking browser-based agent tooling, so I did **not** build it (no `web.*` config added).
   *Revise* if you want reads origin-restricted anyway. (`src/frontend/cors.rs`)

1. **Forced report-refresh is token-gated (Actor) + debounced.** `POST /api/reports/refresh` requires a
   rerun token, like the other write actions, and threads the actor onto the job. *Alternative:* since a
   refresh is non-destructive and debounced (at most one runs at a time), it could be **ungated** for
   easier agentic use. Chose gated for consistency + attribution. (`src/frontend/reports.rs`)

2. **Automatic refresh interval default tightened 24 h → 1 h.** Now that the refresh is non-blocking
   (`CONCURRENTLY`), a 1 h baseline is cheap and gives better freshness; it's runtime-configurable
   (`dispatcher.report_refresh_interval_seconds`). *Confirm* 1 h is the right default for the production
   box's DB-load budget, or set it in `cortex.toml`. (`src/config.rs`, `src/dispatcher/finalize.rs`)

3. **pgtune server config applied to the live `cortex` node (persistent).** `ALTER SYSTEM` values from
   pgtune.leopard.in.ua (Mixed / 256 GB / 64 cores / 300 conn / nvme) are live, incl. `io_method=io_uring`,
   `wal_compression=lz4`, `jit=off` (`docs/DB_TUNING.md`). *Confirm* these suit the box; revert via
   `ALTER SYSTEM RESET ...` + restart if not.

4. **R-5 resolved** — the rerun refresh is now async (off the request path); rerun returns in <1 s.
   `mark_new_run` is bookkeeping-only. No open decision; left here as a pointer. (`docs/KNOWN_ISSUES.md` R-5)

## Open design questions — need a direction

15. **"Add a service" screen composes two agent primitives rather than a new combined endpoint.** The
    new admin **Add-a-service** flow (`POST /services/create`) defines a service *and* activates it on
    the checked corpora in one human action. I did **not** add a combined `corpora: [...]` body to
    `POST /api/services` (it would break the stable `201 + ServiceDto` shape its test/spec assert);
    instead an agent reproduces the screen by calling the two existing documented primitives —
    `POST /api/services` (define) then `POST /api/corpora/<c>/services/<s>` (activate, now 409 on a
    duplicate). This is the same parallel-routes-vs-one-controller tension as #5. *Direction:* accept
    the composition (the agent surface is complete, just not 1:1 with this convenience screen), or add
    a combined `POST /api/services` activation body? (`src/frontend/services.rs`.)

5. **Symmetry mechanism: parallel `/api/*` routes vs. one content-negotiated controller.** Today every
   human screen has a 1:1 `/api/*` twin (good parity), but they're *separate handlers* — `GET /corpus/…`
   with `Accept: application/json` returns HTML, not JSON. CLAUDE.md's contract prefers **one controller**
   that content-negotiates so HTML/JSON can't drift. Converging them is a sizable refactor. *Direction:*
   accept parallel routes as the pattern (update the contract wording), or converge onto negotiation?

6. **Two rerun endpoints coexist.** The modern `reports::rerun_report` (`POST /api/reports/<c>/<s>/rerun`,
   Actor-gated, typed DTO) and the legacy `bin/frontend.rs` `/rerun/...` (token-in-JSON-body, the path the
   human `rerun.html.tera` UI posts to). *Direction:* migrate the human UI onto the modern endpoint and
   retire the legacy routes, or keep both?

7. **API-docs framework — DONE: `rocket_okapi`, full docs generated (owner-chosen 2026-06-14).** The
   owner previewed both spikes and chose `rocket_okapi`. **Landed in full:** `rocket_okapi` + `schemars`
   are real deps; `frontend::apidoc` serves the generated **OpenAPI 3** spec at `GET /api/openapi.json`
   and a **RapiDoc** page at `GET /api/docs`, built from the `#[openapi]`-annotated routes. **The
   complete agent surface — all 26 endpoints (reads *and* writes) across all 6 capability modules — is
   documented**, request/response DTOs derive `JsonSchema`, and the `Actor` token guard is an
   `OpenApiFromRequest` impl advertising a `CortexToken` ApiKey security scheme (`X-Cortex-Token`). The
   `utoipa` runner-up (dev-dep + spike example) is **pruned**. No open question remains; left here as a
   pointer. (`src/frontend/apidoc.rs`, `src/frontend/actor.rs`)

9. **Stalled-job handling: observe now, auto-interrupt deferred (W-4).** A hung job *body* (e.g. the
   importer blocked on a stale mount) can't be force-cancelled in Rust, so its thread + pooled connection
   leak for the process's life. I added **judgment-free observability** — `JobDto.seconds_since_update`
   (heartbeat age vs the DB clock), so a stalled running job is *visible* on `/api/jobs` and the `/jobs`
   dashboard — but did **not** add any auto-kill, because every safe remedy needs a **tuning threshold the
   owner should set**, and a too-tight one would false-kill legitimately-long ops (a full-table
   `REINDEX (CONCURRENTLY)` or a production-scale `REFRESH` can run for many minutes with no `step()`):
   - a **watchdog** that flips a running job past a deadline to `failed`/`stalled` (registry-accurate, but
     the thread keeps running);
   - a **`lock_timeout`** on the refresh/reindex connections (caps *lock acquisition*, not runtime — so it
     won't false-kill a progressing op; the safest of the operation bounds, my tentative recommendation);
   - a **`statement_timeout`** (caps total runtime — riskier for legitimately-long maintenance);
   - **timeouts on the importer's blocking filesystem I/O**.
   *Direction:* which remedies, and what thresholds (you're particular about DB tuning values — same as the
   pgtune episode)? Until then the leak is *surfaced, not bounded*. (`src/jobs.rs`, `src/frontend/jobs.rs`,
   KNOWN_ISSUES W-4)

8. **`src/frontend/cached/` naming — done.** Flattened the one-function nested module and renamed it to
   `src/frontend/render.rs` (the presentation layer). No open decision.

10. **D-5 — `job_limit` drain protocol needs a design (diagnosed, not patched).** Root-caused the
    finite-`job_limit` shutdown hang: the ventilator counts `job_limit` in **requests** (including
    mock-replies), the sink in **results received**, and finalize in **drain *cycles*** (each drains
    the whole `done_queue`) — three incompatible units that can never agree on "done" (full analysis in
    KNOWN_ISSUES D-5). I deliberately did **not** patch it: a correct fix is a cross-thread coordination
    protocol (a shared *dispatched-real-task* counter; finalize/sink terminating when *finalized ==
    dispatched* once the ventilator signals source-exhausted; an explicit "no more TODO tasks" drain),
    and it must stay consistent with `bench_pipeline`'s expectations — a wrong move deadlocks in either
    direction. *Direction:* approve a drain-protocol design (I can draft one) before I touch the three
    dispatcher threads. **Not urgent for production** — the perpetual dispatcher runs `job_limit = None`;
    this is benchmark/bounded-run-only. (`src/dispatcher/{ventilator,sink,finalize,manager}.rs`)

11. **W-1 — oversized-result cap: IMPLEMENTED (2026-06-14), confirm the default.** Shipped as
    `dispatcher.max_result_bytes` (default **2 GiB**): on overflow the sink stops writing, **drains the
    remaining ZMQ frames frame-by-frame** to keep the PULL socket aligned, removes the partial file, and
    finalizes the task **`Invalid`** (`result_too_large`) — chosen over `Fatal` because an unacceptably
    large result is a rejected *input/output*, not a conversion failure. Torture-tested at real scale
    (`tests/dispatcher_torture_test.rs`, `CORTEX_TORTURE_BIG=1`: 1.99 GB accepted, 3 GB rejected/cleaned,
    alongside a 200k-malformed-reply barrage proving the frame-drain stays aligned). *Only open bit:*
    confirm **2 GiB** is the cap you want (it's a runtime knob, so deployments can override) and that
    `Invalid` (vs `Fatal`) is the right terminal status. (`src/dispatcher/sink.rs`, KNOWN_ISSUES W-1.)

12. **Dispatcher phase 3 (sink fan-out, closes D-7): RESOLVED (2026-06-14) — std-thread writer pool
    (option a).** Owner chose the lower-risk intermediate. **Landed:** the sink is now a receive loop +
    a pool of `dispatcher.sink_writers` (default 4) std-thread archive-writers, each fed a bounded
    per-writer command channel; the receive loop owns the socket and streams each result
    (`Begin → Chunk* → Commit|Reject`) to one writer round-robin, so receiving is no longer hostage to
    the `/data` write + `cortex.log` parse. Per-task ordering preserved (contiguous FIFO per task),
    fan-out across tasks, memory O(chunk) (streamed, never the whole archive). Every receive-side
    invariant (RCVMORE envelope hardening, size cap + frame-drain, rate-limited discard, metadata
    enqueue) unchanged; fail-fast preserved (writer death → receive-loop detection → manager abort).
    **Gated green:** `dispatcher_torture_test` (byte-exact integrity + cap), `echo_roundtrip`,
    `dispatcher_bench` (8-worker, 20000 tasks, no loss, throughput-neutral vs the inline baseline on
    loopback — the win is on the slow production disk loopback can't exercise). The tokio async core +
    `tokio::fs` *async* file I/O the plan's default envisioned is deferred to phase 5 (transport swap),
    where it's natural; the std-thread pool already closes D-7's blocking-serialization essence.
    (`src/dispatcher/sink.rs`, `src/config.rs`, KNOWN_ISSUES D-7 🟢.)

13. **Dispatcher phase 4 (lock-free maps): RESOLVED (2026-06-14) — `dashmap` approved, landed.** Owner
    approved the `dashmap` dependency and the 4-after-3 sequencing. **Landed:** the in-flight set is now
    `server::InFlightSet` (a sharded `DashMap<i64, TaskProgress>` + an `AtomicUsize` size counter for the
    O(1) backpressure read), and the service cache is `server::ServiceCache` (a `DashMap<String,
    Option<Service>>`) — so the ventilator lease, the sink return, the reaper sweep, and every dispatch
    lookup no longer serialise on one global `Mutex`. The counter is maintained in lock-step with the
    map (the only mutation site), with the fail-fast hard-limit backstop preserved. Built **red/green
    TDD**: the `InFlightSet` unit tests (incl. **200 concurrent leases/drains** with a consistent
    counter, the duplicate-insert / negative-remove edge cases) were written first (red: type absent),
    then implemented green. **Empirical finding (the "testing reveals more" the owner anticipated):** as
    predicted, the in-flight map was *never* the throughput wall at this scale — `dispatcher_bench`
    8-worker is throughput-neutral within noise (median ~8.9k tasks/s, runs 8.2k–9.8k, straddling the
    phase-3 baseline; the DB finalize at ~9k/s is the bottleneck). The win is architectural (no global
    lock, O(1) size) and will matter as the DB ceiling lifts / under the phase-5 async core. Also added
    a 200-task end-to-end gate (`tests/concurrent_dispatch_test.rs`: 200 tasks × 8 workers, zero loss,
    byte-exact). (`src/dispatcher/{server,ventilator,sink,manager}.rs`, `Cargo.toml`.) **Remaining
    dispatcher work:** phase 5 (tokio + pure-Rust `zeromq` transport, carrying the deferred async file
    I/O) — still owner-gated on the tokio async core.

14. **`pericortex` worker empty-queue throttle — RESOLVED (2026-06-14).** On a mock/empty reply the
    worker used to `sleep(60s)` (hardcoded), which (a) made tail-recovery of a reaped task slow once the
    queue emptied (bounded by `max(lease+reap, 60s)`, not the fast dispatcher reap), (b) made the
    `BENCH_CHAOS` gate take ~60 s, and (c) is the prime suspect behind the D-12 straggler. **Fixed:** the
    throttle is now read from `CORTEX_WORKER_THROTTLE_SECS` (default 60 — behaviour unchanged), shipped in
    **pericortex 0.2.5** (`357b29f`) and adopted by cortex via `cargo update -p pericortex`. Non-breaking.
    *Next:* use a short throttle to **confirm the D-12 mechanism** and, if it holds, re-add a deterministic
    ventilator malformed-flood gate (+ a fast `BENCH_CHAOS`). (`cortex-peripherals/src/worker.rs`.)
