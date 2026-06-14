# Open questions — decisions made autonomously, for owner review

Recorded while working solo (owner away). These are choices I made with a sensible default so progress
wasn't blocked; each can be revised/refactored on return. Newest first.

## Decisions taken (with a default) — confirm or revise

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

5. **Symmetry mechanism: parallel `/api/*` routes vs. one content-negotiated controller.** Today every
   human screen has a 1:1 `/api/*` twin (good parity), but they're *separate handlers* — `GET /corpus/…`
   with `Accept: application/json` returns HTML, not JSON. CLAUDE.md's contract prefers **one controller**
   that content-negotiates so HTML/JSON can't drift. Converging them is a sizable refactor. *Direction:*
   accept parallel routes as the pattern (update the contract wording), or converge onto negotiation?

6. **Two rerun endpoints coexist.** The modern `reports::rerun_report` (`POST /api/reports/<c>/<s>/rerun`,
   Actor-gated, typed DTO) and the legacy `bin/frontend.rs` `/rerun/...` (token-in-JSON-body, the path the
   human `rerun.html.tera` UI posts to). *Direction:* migrate the human UI onto the modern endpoint and
   retire the legacy routes, or keep both?

7. **API-docs framework — DECIDED: `rocket_okapi` (owner, 2026-06-14).** After previewing both spikes
   side by side, the owner chose `rocket_okapi` and asked for the full API docs generated. **Landing
   in progress:** `rocket_okapi` + `schemars` are now real deps; `frontend::apidoc` serves the
   generated **OpenAPI 3** spec at `GET /api/openapi.json` and a **RapiDoc** page at `GET /api/docs`,
   built from the `#[openapi]`-annotated routes (DTOs derive `JsonSchema`). The corpora read slice
   (`GET /api/corpora`, `GET /api/corpora/{name}`) is annotated + documented as the proven first
   vertical slice (tested in `management_api_test`). **Remaining:** annotate the rest of the `/api`
   routes module-by-module — including the write endpoints (whose `(Status, Json<T>)` / bare `Status`
   responders need an okapi responder check) and the `Actor` token guard (needs an `OpenApiFromRequest`
   impl to document the security scheme); then prune the `utoipa` dev-dep + its spike example.

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

11. **W-1 — oversized-result cap needs a value + behaviour (diagnosed, not patched).** The concrete
    CorTeX-side residual of W-1 is the sink streaming a worker's result archive to `/data` with **no
    size bound** (`sink.rs:121-136`) — a runaway/malicious worker or a decompression bomb can fill the
    disk. I did **not** patch it because (a) the cap is a value you'd want to set (a safety backstop —
    e.g. a few GB, clearly beyond any legitimate conversion result — but still your call, given how
    particular you are about such numbers), and (b) the fix lives on the **ZMQ frame path**: on
    overflow it must *drain the remaining frames* to keep the PULL socket aligned (a botched drain
    desyncs every later result — the same fragility as D-4), then clean up the partial file and
    finalize the task `Fatal`. That behaviour (reject vs. truncate; configurable vs. fixed default) +
    the frame-drain are worth a review before I touch the sink hot path. *Direction:* approve a
    `dispatcher.max_result_size_bytes` design (I can draft it). (`src/dispatcher/sink.rs`, KNOWN_ISSUES
    W-1)
