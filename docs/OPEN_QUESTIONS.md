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

4. **R-5 deferred (rerun still refreshes inline, ~2 min request block).** The async helper
   (`jobs::spawn_report_refresh`) now exists; wiring it into `rerun_report`/`serve_rerun` (and removing
   the inline `mark_new_run` refresh) is the planned next increment — deferred to keep the force-refresh
   tick additive/low-risk. (`docs/KNOWN_ISSUES.md` R-5)

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

7. **API-docs framework pick (Arm 9).** `utoipa` vs `rocket_okapi` are both dev-dependencies for a
   side-by-side spike. *Direction:* pick one and generate the OpenAPI surface for the agent API.

8. **`src/frontend/cached/` naming.** It's now a thin uncached proxy (Redis is gone); CLAUDE.md notes a
   rename is pending. *Direction:* rename to something like `frontend/render/` or fold into the capability
   modules.
