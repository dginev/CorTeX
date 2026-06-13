# CorTeX productization — progress log

Append-only, dated log of increments (one entry per working session/loop iteration). The plan and
current-state map live in [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md); the resilience ledger in
[`KNOWN_ISSUES.md`](KNOWN_ISSUES.md). This file is the lightweight "what changed, in order" trail.

## 2026-06-13

- **Arm 14 #6.2 — reports served from the `report_summary` rollup; Redis removed** (commit `b5509b5`).
  Matview extended to `ROLLUP(category, what)`; `reports::task_report` reads the category/`what`
  grains from indexed rollup lookups (sharing `aux_task_rows_stats` with the retained
  `task_report_live`, proven equivalent); rollup refreshed on finalize drain + at-least-daily +
  `mark_new_run`; `redis` crate dropped, frontend boots without it. `TaskStatus` is now `Copy`.
  CI (`.github/workflows/CI.yml`) refreshed (nightly, diesel 2.x, no Redis, fmt + clippy gates).
- **Found + recorded (not fixed):** KNOWN_ISSUES **L-1** — pre-existing flaky at-exit SIGSEGV in
  DB-pool test binaries (teardown race; reproduced on clean `master`, never under gdb).
- **Arm 14 #7 — report pagination (done):** the category and `what` aggregate reports now paginate
  (previously only task-list reports did). Backend: `category_rollup`/`what_rollup` take `limit`/
  `offset` (deterministic `task_count DESC, name ASC` order for stable paging), threaded from
  `TaskReportOptions`; the always-present `total`/`no_messages` summary rows stay whole-severity on
  every page. The report proxy's "next page" signal now counts only *data* rows (excludes the
  summary rows) so it doesn't over-signal. UI: prev/next controls added to `severity-report` and
  `category-report` templates (additive — they only appear past one page). Tests: paging through
  `task_report` + the public `Backend::category_rollup` paging contract.
  - *Deferred refinements:* exact total-page count (needs a `COUNT(*) OVER ()` or a return-type with
    pagination metadata — part of the broader stringly-typed-report cleanup, KNOWN_ISSUES R-3); a
    render smoke-test for the report templates (blocked on draining the legacy report routes into the
    testable library surface).
- **Arm 7 — historical-runs read capability (started):** new testable library module
  `frontend/runs.rs` with a typed `RunDto` (stable `id` handle, `completed` flag, ISO timestamps,
  per-severity tallies) and the agent twin of the history screen — `GET /api/runs/<corpus>/<service>`
  (list, most-recent-first) and `GET /api/runs/<corpus>/<service>/current` (the open run, or `null`).
  Mounted via `server::mount_api_with`; capability test in `tests/runs_test.rs`. This drains the
  binary's legacy `history` route toward the library (symmetry contract).
- **Arm 7 — run comparison API + a robustness fix:** `GET /api/runs/<corpus>/<service>/diff?previous=&current=`
  exposes `summary_task_diffs` as a typed `RunDiffDto` (the status-transition matrix between two saved
  snapshots — what regressed/improved between runs), the agent twin of the diff-summary screen. The
  legacy HTML diff route `.unwrap()`s the date query param and **panics on malformed input**; the twin
  returns **`400`** instead (recorded as KNOWN_ISSUES F-1). Test covers the JSON shape + the 400 guard.
- **Arm 7 — per-task run drill-down:** `GET /api/runs/<corpus>/<service>/tasks?previous=&current=&previous_status=&current_status=&offset=&page_size=`
  exposes `list_task_diffs` as `Vec<TaskDiffDto>` — *which documents* regressed/improved between two
  snapshots (the actionable drill-down behind the matrix), **paginated** (default 100), with graceful
  param parsing (unknown status/date → `400`, empty → no filter). Completes the runs **read** triad
  (list · current · diff-summary · per-task). Test: shape + the bad-status 400 guard.
  *Next:* the runs HTML twin; then run **actions** (rerun exists via `mark_rerun`); or pivot to a
  backend-robustness item (e.g. D-2 worker-metadata upsert).
- **D-2 — worker-metadata race fixed (backend robustness):** the dispatcher's metadata writer did
  find-then-update and **silently dropped** the return count when the sink outran the ventilator's
  insert; with no uniqueness, concurrent inserts could also duplicate rows. Rewrote both writers as
  **`ON CONFLICT (name, service_id) DO UPDATE` upserts** (synchronous `upsert_dispatched`/
  `upsert_received` helpers behind the off-thread spawn), added migration `20260613160000`
  (`UNIQUE(name, service_id)` after a one-time dedupe), and replaced the silent `.unwrap_or(0)` with
  `eprintln!`. Unit tests: out-of-order (received-before-dispatched isn't dropped) + accumulation in a
  single row. Full dispatcher round-trip (`echo_roundtrip`) still green. (D-1 thread-per-event spawn
  remains.) Migration reversibility verified.
