# Open questions — decisions made autonomously, for owner review

Recorded while working solo (owner away). These are choices I made with a sensible default so progress
wasn't blocked; each can be revised/refactored on return. Newest first. Resolved items are condensed to
the one-line index at the bottom (full detail in git / `KNOWN_ISSUES.md` / `archive/PROGRESS_LOG.md`).

## Decisions taken (with a default) — confirm or revise

0. **CORS fix keeps `*` (no origin allowlist).** Arm 13 flagged `src/frontend/cors.rs` pairing
   `Access-Control-Allow-Origin: *` with `Access-Control-Allow-Credentials: true` (spec-invalid +
   unsafe). I fixed it by **dropping the credentials header only**, keeping `*`. Rationale: the public
   surface is read-only public data; agents authorize via the explicit `X-Cortex-Token` header (not
   ambient cookies) and the admin UI is same-origin — so no legitimate consumer needs credentialed
   cross-origin, and `*`-without-credentials is the standard correct posture for a public read API. The
   plan's "origin allowlist replacing `*`" would only *restrict who can read public data* (pointless)
   while breaking browser-based agent tooling, so I did **not** build it (no `web.*` config added). The
   same fairing's dead `Content-Security-Policy-Report-Only` header (report-only, JSON-only, reporting
   to a non-existent `report-uri` route → enforcing/collecting nothing) was removed; a real *enforcing*
   CSP on the HTML + ar5iv-preview surface remains the open Arm 13 task. *Revise* if you want reads
   origin-restricted anyway. (`src/frontend/cors.rs`)

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
   Actor-gated, typed DTO) and the legacy `bin/frontend.rs` `/rerun/...` (JSON-XHR, the path the human
   `rerun.html.tera` UI posts to, now `AdminSession`-gated). *Direction:* migrate the human UI onto the
   modern endpoint and retire the legacy routes, or keep both?

9. **Stalled-job handling: observe now, auto-interrupt deferred (W-4).** A hung job *body* (e.g. the
   importer blocked on a stale mount) can't be force-cancelled in Rust, so its thread + pooled connection
   leak for the process's life. I added **judgment-free observability** — `JobDto.seconds_since_update`
   (heartbeat age vs the DB clock), so a stalled running job is *visible* on `/api/jobs` and the `/jobs`
   dashboard, plus a runtime reaper (`jobs::reap_stale`, 2 h heartbeat-silence → `interrupted`) — but did
   **not** add any hard auto-kill, because every safe remedy needs a **tuning threshold the owner should
   set**, and a too-tight one would false-kill legitimately-long ops (a full-table `REINDEX (CONCURRENTLY)`
   or a production-scale `REFRESH` can run for many minutes with no `step()`):
   - a **watchdog** that flips a running job past a deadline to `failed`/`stalled` (registry-accurate, but
     the thread keeps running);
   - a **`lock_timeout`** on the refresh/reindex connections (caps *lock acquisition*, not runtime — so it
     won't false-kill a progressing op; the safest of the operation bounds, my tentative recommendation);
   - a **`statement_timeout`** (caps total runtime — riskier for legitimately-long maintenance);
   - **timeouts on the importer's blocking filesystem I/O**.
   *Direction:* which remedies, and what thresholds (you're particular about DB tuning values — same as the
   pgtune episode)? Until then the leak is *surfaced + heartbeat-reaped, not hard-bounded*. (`src/jobs.rs`,
   `src/frontend/jobs.rs`, KNOWN_ISSUES W-4)

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

11. **W-1 oversized-result cap — confirm the default.** Shipped as `dispatcher.max_result_bytes` (default
    **2 GiB**): on overflow the sink stops writing, drains the remaining ZMQ frames to stay aligned,
    removes the partial file, and finalizes the task **`Invalid`** (`result_too_large`). Torture-tested at
    real scale (1.99 GB accepted, 3 GB rejected/cleaned). *Confirm* 2 GiB is the cap you want (runtime
    knob, override per deployment) and that `Invalid` (vs `Fatal`) is the right terminal status.
    (`src/dispatcher/sink.rs`, KNOWN_ISSUES W-1.)

## Resolved (for history — full detail in git / KNOWN_ISSUES / archive/PROGRESS_LOG)

- **4 — R-5 async rerun refresh.** `mark_new_run` is bookkeeping-only; the rollup refresh runs off the
  request path, so rerun returns in <1 s. (KNOWN_ISSUES R-5)
- **7 — API-docs framework = `rocket_okapi`** (owner-chosen). All 26 agent endpoints documented at
  `GET /api/openapi.json` + RapiDoc at `GET /api/docs`; `utoipa` runner-up pruned. (`src/frontend/apidoc.rs`)
- **8 — `src/frontend/cached/` renamed → `src/frontend/render.rs`** (flattened the one-function module).
- **12 — Dispatcher phase 3 (sink fan-out, closes D-7):** std-thread writer pool (`dispatcher.sink_writers`,
  default 4); receive loop streams each result round-robin to a writer. (KNOWN_ISSUES D-7)
- **13 — Dispatcher phase 4 (lock-free maps):** in-flight set → `DashMap` + `AtomicUsize`, service cache →
  `DashMap`; throughput-neutral (DB finalize is the wall), win is architectural. (`dashmap` dep approved.)
- **14 — `pericortex` worker empty-queue throttle** is now `CORTEX_WORKER_THROTTLE_SECS` (default 60,
  behaviour unchanged); shipped in pericortex 0.2.5. (`cortex-peripherals/src/worker.rs`)

> Still owner-gated (not autonomous): dispatcher **phase 5** (tokio + pure-Rust `zeromq` transport +
> async file I/O) and the W-4 hard auto-kill threshold.
