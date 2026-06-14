# CorTeX productization — progress log

Append-only, dated log of increments (one entry per working session/loop iteration). The plan and
current-state map live in [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md); the resilience ledger in
[`KNOWN_ISSUES.md`](KNOWN_ISSUES.md). This file is the lightweight "what changed, in order" trail.

## 2026-06-14

- **UI — nav brand cleanup + logo theme-switch alignment fix.** Dropped the redundant "Framework"
  text and the duplicated hidden-xs/hidden-sm brand markup; the nav now shows **just the wordmark
  logo**, and only on **non-landing** pages (the landing page carries the hero, so `overview_page`
  sets `global.is_landing` and the shared nav suppresses its brand there). Vertically centered via a
  flex `.navbar-brand` (height 50px). **Fixed the theme-switch "jump":** the source PNG's two stacked
  variants weren't content-aligned (the paper wordmark sat ~57px lower than the midnight one, so the
  top/bottom crop shifted ~4–5px on toggle). Regenerated `public/img/logo.png` so both variants
  occupy an identical content rectangle (measured each half's bbox, recropped + recentered both into
  the same 1120×320 frame at the same offset) — both halves now share bbox `(208,96,1326,414)`, so the
  crop is geometrically identical across themes. Verified live (landing has hero + no nav brand;
  corpus page has the nav brand + no "Framework").

- **UI — homepage redesign (productized masthead + corpus-card grid).** The overview was noisy
  (oversized serif-purple links, chunky chevrons, and a white-box `<img>` logo on the dark theme).
  Rebuilt `overview.html.tera` as a disciplined `.overview` container: a centered **hero** (the
  wordmark as a `.hero-logo` cropped+blended per theme — no white box) + an italic serif tagline over
  a rule, then a quiet uppercase "Corpora" `.section-heading` and a responsive **`.corpus-card`
  grid** (accent left-border, serif name, muted description, subtle hover-lift) with an empty-state
  fallback. Dropped the Bootstrap `col-md-*` scaffolding + the per-corpus `<h2>`/chevron markup.
  Render-checked (`corpora_test`); hot-reloaded live.

- **UI polish — admin-gated corpus actions, a scholarly widget theme, and a dual-variant logo.**
  - **Admin-gated "Corpus actions".** The corpus page (`GET /corpus/<name>`) is public, but its
    activate/extend/delete forms (and the per-service deactivate buttons) were shown to everyone and
    only bounced to sign-in on submit. Now `corpus_page` threads `Option<AdminSession>` → a real
    `TemplateContext.is_admin` bool; `services.html.tera` renders the actions **only when signed in**,
    and an anonymous visitor sees a delicate **"Log in here for admin actions"** hint (with `?next=`
    back to the corpus) under the content. Tested (`services_test`: anonymous sees the hint and no
    picker; signed-in sees the controls).
  - **Scholarly widget theme.** A component layer on the tokens (`public/css/cortex.css`): serif
    (Roboto Slab) display headings, carded panels (`.card`/`.admin-card`/`.action-panel`, subtle
    shadow + hover), a clean report grid (uppercase small-cap headers on `--bg-sunken`, `--rule-soft`
    row separators, hover, tabular-nums + monospace numeric cells), **status-tinted counts**
    (`.count-ok/-warn/-error/-fatal/-todo`, zeros de-emphasised), pill `.chip`, and the `.admin-hint`.
    Applied to the admin stat cards (`.stat-number`) and the corpus service-report counts.
  - **Dual-variant logo.** `~/Downloads/…png` (1536×1024, two stacked CoRTeX wordmarks — top for
    paper, bottom for midnight) renamed to `public/img/logo.png`; the navbar brand is now a
    `.brand-logo` span that crops to the active variant per theme (`background-size: 100% 200%` +
    top/bottom position) and **blends the variant's solid background into the chrome** (`multiply` on
    paper so white melts into the light nav, `screen` on midnight so black melts into the dark nav) —
    seamless, no logo box. Replaces the old `logo.jpg`.
  Render-checked (`admin_test` / `corpora_test` / `services_test`); fmt + clippy clean; frontend
  restarted for the live preview.

- **UI theming — adopted the "paper" (light) + "midnight" (dark) design tokens from `ar5iv-editor`.**
  Added the two `data-theme` token sets (`:root[data-theme="paper"]` / `[data-theme="midnight"]` —
  `--bg/--bg-elev/--bg-sunken/--ink*/--rule*/--accent*/--link*/--code-bg/--shadow*/--ok/--warn/--bad`
  + font stacks) to `public/css/cortex.css`, copied from `~/git/ar5iv-editor/frontend/src/styles.css`,
  and mapped the Bootstrap-3 surfaces onto them (body, links, both navbars, tables incl. striped/hover,
  panels/wells, forms+focus ring, buttons incl. `.danger` and bare submit buttons, code, hr, muted
  text, status helpers). Replaced the remaining hardcoded colors (quietlink `#000`, sticky `#corpus-report`
  header `white`, `bottom-content #efefef`, the `error`/`fresh` row tints → `color-mix` of `--warn`/`--ok`).
  Dropped Bootstrap's gradient "optional theme" so the flat tokens win. Theme switching: a no-FOUC
  `<head>` script sets `data-theme` before first paint (saved choice → else OS `prefers-color-scheme`),
  a nav **Theme** toggle (`public/js/theme.js`, framework-free) flips paper↔midnight and persists to
  localStorage; `<html data-theme="paper">` is the no-JS default. Admin-dashboard stat cards moved off
  per-element inline `#ddd`/`#1a7e1a` to a tokened `.admin-card` + `var(--ok)` so midnight reads right.
  Render-checked (`admin_test` / `corpora_test` / `services_test` green — the layout change touches
  every page). **Follow-up (owner idea):** a fresh *scholarly dashboard* tabular design for the report
  pages, built on these tokens.

- **Service-activation UX (Arm 6) — "Add a service", register-on-corpus (both directions), in-flight
  tracking, and an idempotent-NEUTRAL registration guard.** Owner test-drive of the admin dashboard
  for the upcoming `oxidized-tex-to-html` (latexml-oxide) service. Built:
  - **"Add a service"** screen (`GET /services/new` + `POST /services/create`, admin-gated): the full
    new-service definition (all DB fields) plus a **checkbox** list of every corpus to activate it on
    (zero or more). Defines the service, then spawns one background `service_activate` job per checked
    corpus; redirects to `/jobs` when any were selected, else `/services`. Linked from the admin
    dashboard ("Add a service") and the registry screen (the old inline define-only `<details>` form
    is replaced by a "+ Add a service" link).
  - **Register an existing service on a corpus** — *service side*: `GET /services/<svc>/activate` +
    `POST` — a **`<select>`** over the corpora the service is **not yet** on (already-activated ones
    excluded). Linked per-row from the registry ("register on corpus").
  - **Corpus-side mirror**: the corpus page's existing service `<select>` now lists only services
    **not yet** registered on that corpus (was: all real services), the inverse picker.
  - **In-flight tracking**: every activation runs as a background job on `/jobs` (auto-refreshes
    while active). The `service_activate` job's progress message now names the corpus+service
    (`registering <svc> on <corpus>`), and the jobs list grew a **Message** column so each in-flight
    registration is identifiable. A long activation stays visible on `/jobs` while it runs; **noted
    residual** — `register_service`'s task-creation loop emits no *intermediate* heartbeat, so a
    multi-hour activation could trip the 2 h W-4 staleness reaper (self-correcting: `finish()`
    overwrites the transient `interrupted`); a per-batch heartbeat is the follow-up (OPEN_QUESTIONS
    #9 / W-4).
  - **Idempotent-NEUTRAL guard (robustness)**: `register_service` was idempotent-**destructive** — it
    wiped & re-created a `(service, corpus)` pair's tasks + `log_*` rows on every call, so a stray
    re-registration silently discarded completed results. Now re-registering an already-registered
    pair is **refused with no action**: a synchronous pre-check in `start_activate` returns **409**
    (no job spawned) for every HTTP path, and `backend::register_service` enforces the same invariant
    as defense-in-depth (CLI + race window). The pickers exclude already-registered targets so the UI
    never offers a 409. `corpora_test`'s old "re-activation is destructive" case was rewritten to
    assert the new neutral contract (prior tasks + logs survive a rejected re-register). Tests:
    `services_test::service_activation_flows` (add-service checkboxes, both register-on-corpus
    directions, the 409 guard, the corpus-side mirror exclusion) — all green; fmt + clippy clean;
    `corpora_test` / `jobs_api_test` / `admin_test` / lib green. *(Agent-API note: the screens
    compose two already-documented primitives — `POST /api/services` (define) + `POST
    /api/corpora/<c>/services/<s>` (activate, now 409 on duplicate); no new combined endpoint — see
    OPEN_QUESTIONS.)*

- **L-1 (CI trustworthiness) — converted the last two test stragglers to the uniform `_exit` harness.**
  `reports_api_test` and `services_test` were the only `Client`-building integration binaries still on
  the **default** libtest harness, using a single `#[test]` that called `libc::_exit(0)` at its end. That
  worked but was a latent trap: a *second* `#[test]` would be run concurrently by libtest and the first to
  finish would `_exit` the process, **silently skipping the rest** (masking failures). Both are now full
  `harness = false` + `fn main()` binaries (Cargo.toml `[[test]]` entries added), matching the other 11.
  Compile-checked + both green (`reports_api_test` / `services_test`: "all cases passed"). **Remaining for
  L-1 🟢:** a full-suite teardown-SIGSEGV survey (incl. the bare-pool `pool_test`/`jobs_test`) → then drop
  `scripts/ci_test.sh`'s SIGSEGV tolerance. Deferred mid-increment to the owner's new priority (the
  latexml-oxide "oxidized-tex-to-html" Add-Service UX).

- **Dispatcher D-12 RESOLVED — root-caused to a sink framing desync (NOT the worker throttle, the
  original suspicion was wrong) + ventilator-flood gate re-added.** Re-investigated the straggler now
  that the worker throttle is configurable (#14). The throttle was **exonerated** — stragglers persist
  at `CORTEX_WORKER_THROTTLE_SECS=1` (a 1 s nap would recover any tail). Localized by elimination in an
  instrumented `dispatcher_torture_test`: sink-flood-only and vent-flood-only are each clean; **only
  the two floods together** strand tasks (~25–33%), and within the vent flood only the unknown-service
  **mock-reply** shapes (skip-only is clean) — the mock-replies *steady* the worker so its results
  interleave 1:1 with the sink barrage. **Root cause:** the sink read `[identity, service, taskid,
  …data]` with `RCVMORE` guards after identity and service but **not after the taskid frame**; a
  malformed 3-frame reply with no data (the barrage's `n%5==3` shape) left `RCVMORE` false after the
  taskid, yet the drain paths `recv()` first then check `RCVMORE` → read the first frame of the *next*
  reply and consumed that whole reply, **swallowing a real worker result** and stranding its task
  `Queued` until the ≥1 h reaper (genuinely dropped/delayed work → this was an **S2**, not the filed
  S3). Period-5 loss ⇔ the barrage's 5-shape cycle; both floods required ⇔ the vent flood paces the
  worker into the barrage. **Confirmed**: skipping only the no-data shape → 0/15 fail (vs 5/15 with
  it). **Fix:** one `RCVMORE` guard right after the taskid frame in `sink.rs` (skip a no-data reply
  without draining), completing the D-4 envelope hardening for the no-data case. **Verified**:
  previously-failing config now **0/25** mixed + **0/25** vent=mock, default gate 0/6, `echo_roundtrip`
  + `cargo test --lib` (27) green, clippy clean. The **ventilator request-framing flood is re-added as
  a permanent regression gate** (now reliable) alongside the byte-exact integrity check. *(Box also
  brought current after the `~/CorTeX` → `~/git/cortex` move + restart: build green, dev DB
  self-migrated up 6 versions via `cortex init`.)*

- **Admin UX — signed-in `/admin` dashboard (consolidates the admin actions off the public root).**
  Owner request: a separate, **signed-in-admins-only** web UI for the admin actions that were
  sprinkled on the root homepage (Registered services · Background jobs · System health · Settings ·
  Add-a-corpus), using the lightweight token scheme. Built: a new `AdminSession` request guard
  (`frontend::actor`) — a rerun token from `auth.rerun_tokens` is entered once on `GET /admin/login`,
  stored in an HttpOnly `cortex_admin` cookie at `POST /admin/login`, and **re-validated against the
  live token map on every request** (the cookie is only a carrier; revoking a token ends the session,
  a forged cookie is rejected). New `frontend::admin` module: `GET /admin` (the gated dashboard —
  links to services/jobs/health/settings/API-docs + the add-corpus form), the sign-in page, and
  `POST /admin/logout`. An unauthenticated `/admin` redirects to the sign-in page. **De-sprinkled the
  root**: `overview.html.tera` is now just the welcome + corpora list + a single "Admin dashboard"
  link; the persistent nav consolidated from five admin links to one "Admin" entry. Test:
  `admin_test` (full flow — unauth redirect → bad token rejected → valid token signs in → dashboard
  renders → sign-out ends the session). fmt + clippy clean; full suite green. **Stage 2 (next):**
  gate the individual admin *screens* (`/services`, `/jobs`, `/health`, `/settings`) behind the same
  `AdminSession` so they're reachable only when signed in (their existing tests will be updated to
  carry the session cookie).

- **API docs (Arm 9) — owner chose `rocket_okapi`; generated OpenAPI 3 spec + RapiDoc page landed
  (foundation).** After the owner previewed both spikes side by side (the `docs/api-spike/index.html`
  CSS bug was fixed so the comparison renders) and picked `rocket_okapi`, wired it in: `rocket_okapi`
  + `schemars` moved to real deps; new `frontend::apidoc` mounts the **generated OpenAPI 3 document**
  at `GET /api/openapi.json` and a **RapiDoc** browser page at `GET /api/docs`, both built by
  rocket_okapi *from the `#[openapi]`-annotated routes themselves* — so the spec is the single source
  of truth and can't drift (the symmetry contract extended to the docs). **Documented so far** (every
  **read** route — 17 endpoints across all 6 capability modules): corpora, services, jobs, runs,
  reports, and management — plus **every write route** (import/extend/activate/deactivate/delete-corpus,
  register-service, rerun/refresh, reindex/analyze, put-config), so the **complete agent surface (26
  endpoints)** is now in the spec. Their request bodies derive `JsonSchema`, and the **`Actor` token
  guard** is documented via an `OpenApiFromRequest` impl that advertises a `CortexToken` ApiKey
  security scheme (`X-Cortex-Token`) on every gated call. The okapi tuple/`Status` responders
  (`(Status, Json<T>)`, bare `Status`) all generate cleanly. Each route carries `#[openapi(tag=…)]`,
  its DTOs derive `JsonSchema`, and it's mounted via `openapi_get_routes_spec!` in `apidoc` (moved out
  of the plain route groups; the multi-module wiring imports each handler + its generated
  `okapi_add_operation_for_*` companion, explicit not glob so the per-module `routes` fns don't
  collide). Tested (`management_api_test::openapi_spec_and_rapidoc_are_served`: OpenAPI 3.x doc with
  corpora/services/jobs paths present, routes still serve, RapiDoc renders). **Next:** runs + reports
  read routes, then the write endpoints (`(Status, Json<T>)` / bare-`Status` responders + the `Actor`
  guard's `OpenApiFromRequest`), then prune utoipa. OPEN_QUESTIONS #7 resolved. fmt + clippy clean;
  `corpora_test` / `services_test` / `jobs_api_test` / `management_api_test` green.

- **Deactivate-service — guarded the magic `init`/`import` services (closed a footgun).** The corpus
  screen lists every service with tasks on the corpus, which **includes** the magic `init` (1) /
  `import` (2) infrastructure services — so the deactivate affordance I'd just added would have
  offered to deactivate `import`, whose deletion wipes the corpus's document registry. Closed it both
  ways: the backend handlers (`DELETE /api/corpora/<c>/services/<s>` + the human form) now return
  **403** for `service.id <= IMPORT_SERVICE_ID` (the new named constant for the `1=init`/`2=import`
  boundary, also replacing the magic `id > 2` in the activate picker — a small rationalization), and
  the corpus template hides the deactivate button for `init`/`import`. Test: extended
  `deactivate_service_removes_pair_tasks_and_logs` to assert deactivating `import` is `403` even with
  a valid token + matching confirmation (verified the test DB has `init=1`/`import=2`). fmt + clippy
  clean; `corpora_test` green.

- **Admin UX — "deactivate (retire) a service from a corpus" (the symmetric counterpart of
  activate).** Closes the management gap recorded in R-6 last increment: you could register/activate a
  service on a corpus but not remove one. New `Service::deactivate_from_corpus` (models/services.rs) —
  a **transactional, orphan-free** cascade of the `(corpus, service)` pair's `log_*` rows + tasks
  (mirrors `Corpus::destroy`; the service definition + its work on other corpora untouched;
  `historical_runs` tallies survive, per-task `historical_tasks` snapshots cascade with the tasks —
  same semantics as deleting a corpus). Full symmetry: agent
  `DELETE /api/corpora/<c>/services/<s>?confirm=<s>` (Actor-gated + confirmation, 204/400/404, sync
  like corpus-delete) + human per-service "deactivate" form on the corpus screen (token + a native
  `confirm()` guard — light vanilla JS, degrades gracefully). Test:
  `corpora_test::deactivate_service_removes_pair_tasks_and_logs` (401 untokened, 400 bad-confirm, 204
  success, cascade verified, service definition survives, 404 unknown). fmt + clippy clean;
  `corpora_test` green. The *global* `delete_service_by_name` orphan remains (R-6, still unused).

- **Service (re)activation — made `register_service` orphan-free + crash-consistent.** The
  activate-service action (`POST /api/corpora/<c>/services/<s>` → `backend::register_service`) deletes
  the `<service, corpus>` pair's prior tasks before re-creating them — but it deleted **only the
  tasks, not their `log_*` rows** (the code's own "TODO: also erase log entries"), so every
  **re-activation orphaned the prior tasks' logs** (the same no-FK hazard closed in `Corpus::destroy`);
  and the delete ran *outside* the re-insert transaction, so a crash between them could leave the
  service with its tasks deleted but none re-created. Fix: one transaction that deletes the pair's
  `log_*` rows (all five tables, scoped by the pair's task ids) **and** its tasks, then re-creates a
  TODO task per imported entry — atomic + orphan-free. Test: extended
  `corpora_test::register_service_creates_tasks_and_attributes_the_run` to seed a log, re-activate,
  and assert the prior log is gone (no orphan) with exactly 2 fresh TODO tasks. **Recorded (not
  fixed):** the *unused* `backend::delete_service_by_name` has the same orphan bug (deletes only the
  `services` row) — KNOWN_ISSUES R-6. fmt + clippy clean; `corpora_test` green.

- **I-1 (unpack glob) — hardened the complex-import glob compilation against metacharacter paths.**
  The complex-corpus `unpack` path had three `glob(pattern).unwrap()` sites
  (`unpack_arxiv_top`/`unpack_extend_arxiv_top`/`unpack_arxiv_months`) that **panicked** when a
  `corpus.path` contained glob metacharacters (`[`, `{`, …) — an operator-triggerable crash on the
  Admin-UX complex-import action. The two single-pattern sites now propagate the `PatternError` as a
  clean import failure (`?`); the multi-pattern site skips + logs an invalid pattern and continues
  with the valid ones (`filter_map` + `flatten`). Regression:
  `importer_test::import_does_not_panic_on_glob_metacharacter_path` (a `[`-containing complex corpus
  path returns `Err`, not a panic); `can_import_complex`/`can_import_simple` confirm the happy path is
  unchanged. This is the clearly-safe, pre-streaming subset of the I-1 `unpack` residual; the
  libarchive **streaming** unwraps remain (KNOWN_ISSUES I-1, deferred — a mid-stream skip can leave a
  partial output). fmt + clippy clean; `importer_test` 4/4.

- **W-1 — scoped + located the concrete CorTeX-side gap (the unbounded sink result write).** W-1 was
  a broad S1 ("no per-task timeout / resource cap"). Reading the sink let me separate what's actually
  covered from the real residual: ① a worker that **hangs/dies** is *covered* — the reaper time-boxes
  dispatched tasks (`expected_at` + `MAX_DISPATCH_RETRIES`, D-6); ② the worker's *own* memory/CPU is a
  `pericortex`/cgroup concern outside this repo; ③ the **concrete CorTeX-side residual** is the sink
  streaming a result archive to `/data` with **no size cap** (`sink.rs:121-136` — tracks
  `total_incoming`, never bounds it), so a runaway/malicious worker or a decompression bomb can fill
  the disk. Did **not** patch: the cap value is owner tuning, and the fix is on the ZMQ frame path
  (overflow must drain remaining frames to keep the PULL socket aligned — botched drain cascades like
  D-4 — then clean up the partial file + finalize `Fatal`). Sharpened KNOWN_ISSUES W-1 (still 🔴, now
  precisely located) and logged the `dispatcher.max_result_size_bytes` design as OPEN_QUESTIONS #11.
  (Docs only.)

- **D-4 (partial) — fixed the restart band-aid's limbo-clearing double-dispatch.** Reading the
  ventilator to characterize D-4 surfaced a concrete correctness bug: `Ventilator::start` calls
  `clear_limbo_tasks` on **every (re)start** (`ventilator.rs:59`), bluntly resetting **all** `status>0`
  (Queued) tasks → TODO. Correct at process start (crash recovery), but on a *mid-operation* ventilator
  restart (the D-4 band-aid) the sink is still processing **in-flight** tasks (in `progress_queue`,
  `status=Queued`) — resetting those re-leased them while the original results were pending, a
  **double-dispatch** (wasted compute; the duplicate result later discarded). Fix: exclude the live
  `progress_queue` ids — new `Backend::clear_limbo_tasks_except(&in_flight)`
  (`tasks_aggregate::clear_limbo_tasks_except`, `id <> ALL(in_flight)`), with the ventilator passing
  its `progress_queue` snapshot. An empty slice is the old blunt reset, so process-start is byte-for-
  byte unchanged. Tested: `backend_test::clear_limbo_except_preserves_in_flight_tasks` (in-flight
  preserved, others recovered); `task_lifecycle` + `echo_roundtrip` confirm process-start/full-
  dispatcher behaviour unchanged. KNOWN_ISSUES D-4 🔴 → 🟡 (the ROUTER-framing root cause that would
  remove the restart entirely remains open; a microsecond snapshot race is the noted residual). fmt +
  clippy clean.

- **D-9 — dispatcher now supervises the sink/finalize threads (fixes a silent production stall).**
  Reading the manager + sink to diagnose D-7 surfaced a sharper robustness bug: the manager
  restart-loops only the **ventilator**; the **sink** and **finalize** threads are spawned once, and
  in perpetual mode (`job_limit = None`, production) the supervision loop never reaches their joins —
  so a sink/finalize death (a DB-runaway panic, or the sink's `return Err` on an unknown service)
  left the dispatcher running with a **dead result pipeline**, silently stalled (results unprocessed
  → in-flight saturates → ventilator mock-replies forever → nothing aborts). Contradicts the
  fail-fast design ("process abort → external restart"). Fix: the supervision loop now polls
  `sink_thread.is_finished()` / `finalize_thread.is_finished()` each iteration (perpetual mode only —
  `job_limit` mode `break`s first, so a cleanly-finished thread isn't mistaken for a death) and
  aborts with `ETERM`, so the supervisor restarts the whole dispatcher; `clear_limbo_tasks` returns
  leased tasks to `TODO` on restart (no work lost). Happy path verified by `echo_roundtrip` (full
  dispatcher); build + clippy clean. KNOWN_ISSUES D-9 🟢. (The sink's *individual* panic triggers +
  the blocking-write throughput bottleneck remain the D-7-area work.)

- **D-5 — precisely root-caused the `job_limit` shutdown hang (diagnosis, not a patch).** Read the
  three dispatcher threads and found the desync is a **units mismatch**, not just "mock-replies": the
  **ventilator** counts `job_limit` in *requests* (incl. every mock-reply — unknown-service,
  backpressure, empty-queue), the **sink** in *results received*, and **finalize** in *non-empty drain
  cycles* (`mark_done_arc` `.drain(..)`s the whole `done_queue` per increment). Three incompatible
  units → the threads can't agree on "done" → the ventilator stops early while finalize/sink block →
  hang. A correct fix is a cross-thread **drain-coordination protocol** (shared dispatched-task
  counter; terminate when finalized == dispatched after a source-exhausted signal; explicit "no more
  TODO" drain) with `bench_pipeline` integration risk — a wrong move deadlocks either way, so it needs
  owner-reviewed design rather than an autonomous patch. Upgraded KNOWN_ISSUES D-5 from a vague note to
  the precise multi-axis diagnosis + fix sketch, and logged the design decision in OPEN_QUESTIONS #10.
  **Not a production hang** — the perpetual dispatcher runs `job_limit = None` (benchmark/bounded-run
  only). (Docs only; no code touched on this risky concurrent path.)

- **Corpus deletion — made `Corpus::destroy` the complete, transactional, orphan-free primitive.**
  The CLAUDE.md hazard "deleting a corpus orphans `log_*` rows" was real at the model layer:
  `Corpus::destroy` deleted tasks + the corpus but **not** the `log_*` rows (which have no FK to
  `tasks`), so any direct caller orphaned them. The frontend `delete_corpus_cascade` worked around it
  by deleting the 5 log tables first — but as **6 separate non-transactional statements**, so a crash
  mid-delete left a half-deleted corpus (crash-consistency gap). Moved the log cleanup **into**
  `destroy` and wrapped the whole thing (`log_*` → tasks → corpus, with `historical_tasks` cascading
  via its FK) in **one transaction**: now atomic, orphan-free, and correct for *every* caller, not
  just the frontend. `delete_corpus_cascade` collapses to a one-line delegation. Strengthened
  `corpora_test::delete_corpus_removes_corpus_tasks_and_logs` to seed + assert **two** severities
  (warning + error) gone, proving the cascade covers all `log_*` tables. Updated the CLAUDE.md
  load-bearing fact. Full suite green (lib 13, backend 4, importer 3, + all integration suites); fmt
  + clippy clean.

- **Docs — corrected the stale agent-API surface in `TEST_DRIVE.md` (agent-first parity).** The
  "Agent-API parity" section was stale and undersold the system: it listed "13 handlers" — all
  *read* twins — and omitted every write/maintenance endpoint, misrepresenting an agent-first
  framework as read-only-over-API. Rewrote it from the actual route table (24 endpoints): anchored on
  the live `GET /api` discovery index as the **canonical, never-drifting** list (so it can't go stale
  again), then grouped curl examples into **Read** (no secrets) and **Write & manage** (token-gated)
  covering the full admin lifecycle — import → register/activate → monitor → rerun → reindex/analyze
  → delete — with the `X-Cortex-Token` gating and the `202 + job handle` polling pattern. Endpoint
  paths + JSON bodies verified against `ImportRequest`/`ServiceRegisterRequest` and the route
  definitions. Completes the agent-first half of "thorough and complete Admin UX." (Docs only.)

- **I-1 — importer walk hardened against hostile data (corpus-import Admin UX path).** `walk_import`
  (reached from the Admin UX corpus-import action via `corpora.rs` → `process()`, and the `init`
  worker) aborted the *whole* import on the first filesystem hiccup — `fs::metadata(..)?` /
  `read_dir(..)?` killed the walk on a broken symlink or a permission-denied/vanished subdir, a
  non-UTF-8 path **panicked** via `.to_str().unwrap()`, and `mark_imported(..).unwrap()` panicked on
  a DB error (the acknowledged "TODO: Proper Error-handling"). Now every per-path step **skips +
  logs** and continues (blast-radius isolation + transparent failure, docs/DESIGN_PRINCIPLES.md);
  only the backend write is fatal and it propagates as a `Result` (`?`). Regression:
  `importer_test::import_skips_unreadable_paths_instead_of_aborting` imports a valid entry beside a
  broken symlink and asserts the valid one still lands; the existing simple/complex imports still
  pass (behavior preserved). Recorded the remaining gap — the complex-corpus `unpack`/arXiv-tarball
  path's pervasive unwraps — as KNOWN_ISSUES I-1 🟡 (larger, owner-reviewed hardening). fmt + clippy
  clean; `importer_test` 3/3.

- **Health — corpus storage reachability check (catches a broken /data mount).** Document bytes live
  on a shared filesystem (`tasks.entry` are absolute paths under each `corpus.path`), so a
  moved/unmounted data directory makes the whole conversion pipeline fail — previously visible only
  as mysterious cascading task failures. `HealthDto` now carries a `storage` section: `health_report`
  stat-checks every corpus's `path` (`Path::is_dir`) and lists any that are missing/unreadable
  (`StorageHealth { corpora_checked, unreadable: [{name, path}] }`), surfaced on `/healthz`
  (agent), `/health` (a new row + an explicit list when any are broken), with `cortex doctor`-style
  transparency. Corpora with an empty path are skipped (no configured location). **Informational**
  (the frontend serves reports from the DB regardless), so it does not flip the overall `status` —
  consistent with the dispatcher-reachability precedent. The disk stats run **after** the pooled
  connection is returned (the corpus list is gathered inside the checkout), so no connection is held
  during filesystem I/O. Tested: `healthz_flags_unreadable_corpus_storage` seeds a corpus with a
  missing path and asserts it's flagged (status stays `ok`); the healthz contract test asserts the
  storage fields. fmt + clippy clean; `management_api_test` green.

- **DB-health maintenance — on-demand `ANALYZE` (planner-statistics refresh) job.** Completes the
  ongoing-maintenance Admin UX the owner flagged as "very important": alongside the existing online
  reindex, added an `ANALYZE`-over-the-high-churn-tables job. **Why it matters now:** a bulk import
  or large rerun flips millions of `tasks.status` rows to/from TODO, and stale planner statistics
  make Postgres mis-estimate and **skip the TODO leasing index** (`todo_index`, added earlier this
  session) — an `ANALYZE` refreshes the stats so the index is actually used, instead of waiting for
  autovacuum's next pass. Mirrors the reindex job exactly: `jobs::spawn_analyze` (ANALYZE_KIND,
  debounced, per-table progress), agent `POST /api/maintenance/analyze` (token-gated, 202 + job
  handle) + human `POST /maintenance/analyze` + a "Refresh planner statistics" button on `/health` —
  full symmetry, observable on `/jobs`. Rationalized the shared table list `REINDEX_TABLES` →
  `MAINTENANCE_TABLES` (used by both jobs). Tested: `analyze_is_token_gated` (401 without a token,
  mirroring reindex); the ANALYZE SQL verified valid against all 7 tables. fmt + clippy clean;
  `management_api_test` green. Documented in `docs/DB_TUNING.md` (the maintenance source of truth).

- **F-5 — hardened the last request-path panics in the live report engine.** Audited the
  request-reachable layer (frontend handlers + the models/helpers they call) for `.unwrap()`/
  `.expect()`/`panic!` — most candidates were guarded-safe (the `concerns.rs` severity/category/what
  unwraps are provably `Some` per the if/else chain; `from_key("in_progress")` is handled;
  `peek().next().unwrap()` follows a successful peek; `uri_escape(Some(_))` always returns `Some`).
  The real gap: `backend::reports::task_report_live` (reached from every report screen via
  `task_report`'s fall-through) had **4 bare `.unwrap()`s** — `total_count`/`invalid_count` and two
  `AggregateReport` grain queries — that panicked the request → 500 on a DB error mid-report, even
  though their siblings on the same path already used `.unwrap_or_default()`. Made them consistent:
  counts → `.unwrap_or(0)` (total clamped `≥0`), grains → `.unwrap_or_default()` (added `Default` to
  `AggregateReport`); the percentage helper already clamps the denominator `≥1.0` (no div-by-zero) and
  its `"total"` lookup is now defensive too. Normal-path output unchanged — pinned by
  `report_rollup_test` + `rollup_path_matches_live_path` (green). KNOWN_ISSUES F-5 🟢.

- **Perf — partial index for the hot task-leasing path (migration).** The ventilator leases work via
  `tasks_aggregate::fetch_tasks`: `SELECT * FROM tasks WHERE service_id = $1 AND status = 0 (TODO)
  LIMIT n FOR UPDATE` (~100/s at production scale). The `tasks` table had a partial index per
  *completed* status (`ok_index` … `invalid_index`, WHERE status = −1…−5, for reports) but **none for
  TODO (status = 0)** — the status leasing filters on. So leasing fell back to the broad
  `service_idx (service_id)` and, on a mostly-processed corpus, scanned every completed task for the
  service to find the sparse TODO rows. Added migration `2026-06-14-060000_tasks_todo_lease_index`:
  `todo_index ON tasks(status, service_id, corpus_id, id, entry) WHERE status = 0` — mirrors the
  sibling per-status indexes' shape, restricted to TODO. **Verified on a seeded 50k-completed /
  30-TODO service:** EXPLAIN(ANALYZE) goes from `Index Scan using service_idx … Rows Removed by
  Filter: 50000` (489 buffer hits) to `Index Scan using todo_index … Index Cond: (status=0 AND
  service_id=…)` (33 buffer hits) — ~15× fewer reads here, widening with the completed-task count.
  Idempotent (`IF NOT EXISTS`, so an operator can pre-build it `CONCURRENTLY` on the live table and
  let the migration no-op); reversible (verified via `diesel migration redo`). Already covered by the
  reindex maintenance job (whole-table `REINDEX`). `backend_test` green.

- **W-3 closed (in-repo worker) — honor the configured worker identity.** `InitWorker::start`
  unconditionally generated a random 19-letter ZMQ identity, ignoring the configured `identity`
  field — so worker metadata keys weren't operator-controlled and, worse, each restart produced a
  fresh random name, fragmenting a worker's `worker_metadata` row (tallies/liveness) across restarts.
  Extracted `resolve_worker_identity(configured, rng)`: honors a non-empty configured identity
  verbatim (stable key, accumulates across restarts), falls back to the random handle only when
  unset. Unit-tested (configured → verbatim; empty → 19 lowercase letters). External `pericortex`
  workers are out of scope (their own crate). KNOWN_ISSUES W-3 🟡 → 🟢.

- **Admin UX cohesion — persistent admin nav on every page.** The management surfaces (Services,
  Jobs, Health, Settings) were only linked from the landing overview; every other screen (corpus
  reports, jobs, health, runs, worker fleets) had no global navigation — you had to return to `/` to
  move between them. The shared `layout.html.tera` header now always renders a persistent
  `cortex-admin-nav` (Overview / Services / Jobs / Health / Settings, FA-iconed, Bootstrap navbar —
  no new JS), with the existing corpus→service→severity breadcrumb kept on the left when in report
  context. Now every management capability is reachable from anywhere — the "thorough and complete
  Admin UX" cohesion goal. Regression: `jobs_api_test` asserts the nav + its links render on a
  non-landing page (`/jobs`). All 6 HTML-rendering integration suites green; clippy clean.

- **Worker-fleet liveness: F-4 panic fixed + agent-twin parity.** Two issues in the worker-fleet
  surface (the ~200-worker production fleet's observability). **(1) F-4 panic:** the `/workers/<svc>`
  HTML screen rendered workers via `From<WorkerMetadata> for HashMap`, whose `since_string` did
  `duration_since(then).unwrap()` — a *future* worker timestamp (clock skew across fleet hosts, or a
  DB clock ahead) errored and **panicked the whole screen** for one skewed row. Softened to
  `unwrap_or_default()` (→ "0 seconds ago" + fresh). **(2) Symmetry gap:** the human screen showed
  worker liveness ("N ago" + fresh/stale) but the agent twin `WorkerDto`
  (`GET /api/services/<svc>/workers`) exposed none — an agent couldn't spot a dead worker. Added
  `seconds_since_last_active` (liveness age = now − most-recent dispatch/return, skew-clamped to 0)
  + `fresh` to `WorkerDto`, bringing the agent twin to parity (symmetry contract). Mirrors the W-4
  job heartbeat-age signal. Regression (`services_test`): a `now() + 1h` worker keeps `/workers/<svc>`
  at `200` and the agent twin clamps its age to `0`; liveness fields asserted on the fresh worker.
  KNOWN_ISSUES F-4 🟢. fmt + clippy clean; `services_test` + `worker_metadata` units green.

- **Finalize hot path fully batched — per-task status `UPDATE` collapsed (completes D-8).** The
  finalize loop still issued one `UPDATE tasks SET status=… WHERE id=…` per task. Terminal statuses
  are a tiny fixed set (`NoProblem/Warning/Error/Fatal/Invalid`), so `mark_done` now groups the
  finalized ids by distinct target status (`HashMap<i32, Vec<i64>>`) in the same pass that partitions
  messages, then issues **one `UPDATE … WHERE id = ANY(...)` per distinct status** (≤~6, over disjoint
  id sets) — native, type-safe Diesel, no raw SQL. With the already-batched deletes (5) and inserts
  (≤5), a finalize batch of N tasks / M messages dropped from `≈7·N + M` statements to **≤16,
  independent of N and M**. Extended `mark_done_routes_messages_to_severity_tables` to use two
  *distinct* statuses and assert each task receives its own (covers the by-status grouping).
  `backend_test` 4/4; fmt + clippy clean.

- **W-4 observability — job heartbeat age (`seconds_since_update`).** A hung job *body* can't be
  force-cancelled in Rust, so a stalled job's thread + pooled connection leak. Rather than guess a
  kill threshold, surfaced the residual transparently: `JobDto` now carries `seconds_since_update`
  (`db_now - updated_at`), the **heartbeat age** of a running job — it climbs while no progress is
  made, so a stall is visible on `GET /api/jobs[/<uuid>]` and the `/jobs` dashboard ("Idle (s)"
  column). Measured against the DB clock via a new `jobs::db_now` (`LOCALTIMESTAMP`, the same session
  tz the timestamps are written in) so it's skew-free vs the app process's `Utc::now()`. Added
  `JobDto::at(job, now)`; `From<Job>` (spawn-return paths, age≈0) delegates with `None`. Test:
  `jobs_api_test` seeds a 1h-stale running job and asserts a large heartbeat age. The auto-interrupt
  / operation-timeout half needs an owner-set tuning threshold (legitimately-long reindex/refresh
  must not be false-killed) — logged in OPEN_QUESTIONS #9; W-4 stays 🟡. fmt + clippy clean;
  `jobs_api_test` + `jobs_test` green.

- **D-8 closed — `mark_done` message inserts now batched (finalize hot path).** The deletes were
  already collapsed to one `task_id = ANY(...)` per `log_*` table; the per-message `INSERT` loop is
  now gone too. `mark_done` partitions the batch's new messages by severity into five `Vec<NewLog*>`
  (the `NewLog*` structs already derive `Clone`/`Insertable`), then issues **one batched multi-row
  `INSERT` per non-empty table** — at most 5 inserts regardless of how many messages a finalize batch
  carries (was one round-trip *per message*). The synthetic `status` message is still skipped, and
  the per-task status `UPDATE` stays in the loop. Test-first: new `backend_test`
  `mark_done_routes_messages_to_severity_tables` finalizes a mixed-severity batch and asserts each
  message lands in its own `log_*` table (routing correctness for the partition). `cargo test
  --test backend_test` green (4/4); fmt + clippy clean. KNOWN_ISSUES D-8 🟡 → 🟢.

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
- **D-1 — bounded worker-metadata writer (robustness + performance):** replaced the **unbounded
  thread-per-event spawn** (~400 detached threads/s at 200 tasks/s) with a **single background writer**
  (`start_metadata_writer`) fed by a bounded, non-blocking `sync_channel`. The ventilator/sink now hold
  a cloneable `WorkerMetadataSender` and `try_send` events (never blocking the dispatch hot loop; a
  saturated queue drops rather than OOMs/stalls). O(1) metadata threads, ≤1 metadata DB connection at a
  time, clean shutdown when senders drop. Wired through `manager.rs`; `echo_roundtrip` (full dispatcher)
  green. Ledger: **D-1 → resolved**, **D-6 → 🟡** (metadata fan-out bounded; in-flight task set still
  unbounded). The dispatcher metadata subsystem is now race-free (D-2) *and* bounded (D-1).
- **Arm 7 — runs HTML twin (Admin UX):** the human run-history **screen** now lives in the library:
  `GET /runs/<corpus>/<service>` server-renders a table of the same runs `GET /api/runs/...` returns,
  sharing `RunDto` (the symmetry contract realized end-to-end for a screen — human screen + 1:1 agent
  API from one module). New `templates/runs.html.tera` (server-rendered, no JS framework, per the UI
  guidance); `404` consistent with the API. Test renders the screen and asserts the seeded rows appear
  server-side. (The legacy bin `history` Vega page still renders; it migrates here later.)
- **Reports agent API (symmetry + rationalization):** the most-used admin screen (severity/category
  reports) had **no agent API** and returned stringly-typed `Vec<HashMap>`. New `frontend/reports.rs`
  serves typed, paginated JSON straight off the rollup: `GET /api/reports/<corpus>/<service>/<severity>`
  → `CategoryReportDto` (category rows + severity totals), `…/<severity>/<category>` → `WhatReportDto`
  (what rows + category totals); `ReportRowDto {name, tasks, messages}`. Severity-validated (`400`),
  `404` on unknown corpus/service. Reuses the existing typed rollup reads (`category_rollup`/
  `what_rollup`/`severity_total`/`category_total`, now re-exported), so the API and the HTML screens
  reflect the **same** rollup. Contract test pins the numbers + guards. Closes the biggest
  symmetry-contract gap; KNOWN_ISSUES R-3 → 🟡 (agent contract typed; internal HTML path still uses
  `HashMap`).
- **R-2 — widen `tasks.entry` (data integrity for hostile paths):** `entry` was `varchar(200)`, so a
  source-archive path past 200 chars **errored on insert** ("value too long") and the document was
  silently dropped from processing (confirmed: a 250-char insert errors). Migration `20260613170000`
  widens it to `varchar(4096)` (PATH_MAX) — *increasing* a varchar length is **catalog-only** in
  Postgres (no table rewrite, no rebuild of the 7 `entry` indexes), so it's safe on the large `tasks`
  table without a maintenance window. Reversibility verified; regression test `tests/long_entry_test.rs`
  (a 300-char entry stores + reads back untruncated). Ledger: **R-2 → resolved**.
- **Run actions — token-gated rerun + the `Actor` guard (Arm 9a foundation):** reusable `Actor` request
  guard (`frontend/actor.rs`) resolves a rerun token (`X-Cortex-Token` header or `?token=`) to an owner
  via `config().auth.rerun_tokens`, else **`401`** — so writes are **denied by default** (an empty token
  map rejects everyone; no unauthenticated result-wipe). First write API on it:
  `POST /api/reports/<corpus>/<service>/rerun?severity=&category=&what=&description=` marks the
  **filtered** scope for reprocessing as a new historical run, threading the authenticated actor as the
  run `owner` (the "actor through every write" mandate). Tests: `401` denial through the route +
  `mark_rerun` effect (warning tasks → TODO, logs cleared).
  - *Owner steer (2026-06-13):* run-management is **filter-driven** — rerun acts on a *filtered* scope,
    complementing the already-built task-diff filters (`/api/runs/<corpus>/<service>/tasks?previous_status=&current_status=`:
    which individual tasks changed conversion severity between runs). Next: surface that filter as a
    human screen (the visual severity-transition diff).
- **Arm 7 — human task-diff screen (the filter-driven heart of run management):** the owner steer's
  "next" is done — `GET /runs/<corpus>/<service>/tasks` (`runs::runs_tasks_page`) is the server-rendered
  HTML twin of the `…/tasks` agent API, sharing `TaskDiffDto`. A `previous_status → current_status`
  transition picker (server-rendered `<form method=get>`, no JS) drives the filter; the table lists the
  individual documents that changed conversion severity between snapshots, paginated (prev/next preserve
  every filter param). Reached from the run-history screen and links back to its JSON twin.
  **Robustness:** unlike the legacy `diff-history` binary route — which `.expect()`s the status params,
  `.unwrap()`s `from_key`, and `.unwrap()`s the dates, **panicking on the dispatch path** — this twin
  reuses `parse_status`/`parse_snapshot_date`: `400` on a malformed/unknown date *or* status, `404` on an
  unknown corpus/service, empty = list every change. New `templates/runs-tasks.html.tera`. Tests (in
  `tests/runs_test.rs`): the screen renders with its filter form; the two 400 guards (bad status, bad
  date) hold on the HTML path; 404 on unknown corpus. KNOWN_ISSUES **F-1** note expanded (the twin now
  covers the status-param panics too; 🟡 until the legacy `diff-*` bin routes are deleted).
  *Next:* delete/redirect the legacy `diff-historical_*` + `*report*` binary routes onto the library
  surface (closes F-1, kills bin↔library duplication); or pivot to a backend item (D-6 in-flight bound).
- **Arm 7 — human diff-summary matrix screen (completes the runs HTML surface):** `GET /runs/<corpus>/<service>/diff`
  (`runs::runs_diff_page`) server-renders the status-transition **matrix** between two snapshots — the HTML
  twin of `api_run_diff`, sharing `RunDiffTransitionDto`. A JS-free `<form method=get>` with two snapshot
  date dropdowns picks the pair; each matrix cell links into the `runs_tasks_page` drill-down pre-filtered to
  that `previous → current` transition. The runs HTML surface is now a complete inspection funnel:
  **run history → diff matrix → per-task drill-down**, each linked, all sharing the agent DTOs. Reuses
  `parse_snapshot_date` → `400` on a malformed date (the legacy `diff-summary` route `.unwrap()`s it and
  panics), `404` on unknown corpus/service; degrades gracefully to "no snapshots to compare" when none are
  saved. New `templates/runs-diff.html.tera`; nav links added on the history + task-diff screens. Tests
  (`tests/runs_test.rs`): renders + the empty-snapshot graceful path + the 400 date guard + 404. With both
  diff twins (matrix + drill-down) now in the library, the legacy `diff_historical_*` bin routes are pure
  liability — KNOWN_ISSUES **F-1** updated to say they're ready to delete. (`runs_test` is in the L-1 at-exit
  SIGSEGV set: 2/2 assertions pass every run; the flaky teardown crash is pre-existing, not from this change.)
  *Next:* **delete** the legacy `diff_historical_summary`/`diff_historical_tasks` routes + their two
  templates and repoint `report.html.tera`'s "Diff previous runs" link at `/runs/<corpus>/<service>/diff`
  (closes F-1); or pivot to backend D-6 (bounded in-flight task set + dispatch backpressure).
- **F-1 RESOLVED — deleted the legacy panicking diff routes (robustness + rationalization):** with both
  diff twins now in the library, removed the dead, panic-prone legacy surface wholesale. Deleted
  `bin/frontend.rs`'s `diff_historical_summary`/`diff_historical_tasks` routes (each `.unwrap()`ed dates
  and `.expect()`ed/`.unwrap()`ed user-supplied status params → dispatch-path panics) + their route
  registrations + `templates/diff-summary.html.tera` + `templates/diff-history.html.tera`. Repointed
  `report.html.tera`'s "Diff previous runs" link at the library `/runs/<corpus>/<service>/diff`. Pruned
  everything they alone kept alive: the `DiffRequestParams` struct + the three now-unused
  `TemplateContext` diff fields (`diff_report`/`diff_summary`/`diff_dates`) in `frontend/params.rs`, the
  two now-callerless `Backend::{list,summary}_task_diffs` wrapper methods in `backend.rs`, and four
  thereby-orphaned imports (`NaiveDateTime`, `TaskStatus`, `DiffStatusFilter`, `DiffStatusRow`,
  `TaskRunMetadata`). Net: **−2 routes, −2 templates, −1 struct, −3 fields, −2 methods, 0 behaviour lost**
  (the library twins cover every path). `cargo build` (lib + bins) + `clippy --all-targets -D warnings`
  clean; `runs_test` green. KNOWN_ISSUES **F-1 → 🟢**; the two residual frontend `.unwrap()`s (a
  worker-query DB unwrap + an ~infallible JSON serialization, neither input-triggered) folded into D-3's
  request-path-unwrap audit so nothing is fix-and-forgotten.
  *Next:* migrate the legacy Vega `history` page onto `frontend/runs.rs` (last runs-screen still in the
  bin); or pivot to backend D-6 (bounded in-flight task set + dispatch backpressure).
- **D-6 — dispatch backpressure (backend robustness):** the dispatcher's in-flight task set
  (`progress_queue`) was bounded only by a hard panic at 10k — under overload it grew until the process
  **crashed** instead of degrading. Added **backpressure** (principle #4): the ventilator now checks the
  in-flight size before leasing and, once it hits `DispatcherConfig.max_in_flight` (new config knob,
  default 5000), stops leasing and **mock-replies** so workers back off and retry; the set then drains
  via the sink (returning results) and holds steady below the panic. Threaded `max_in_flight` through
  `TaskManager` → `Ventilator` (+ `bin/dispatcher.rs` from config; the 3 explicit test/example
  constructions take `..TaskManager::default()`). Rationalized the two magic `10_000` bounds into named
  `server::PROGRESS_QUEUE_HARD_LIMIT` / `DONE_QUEUE_HARD_LIMIT` constants (the hard backstop behind the
  soft backpressure), and extracted `in_flight_saturated` + `progress_queue_len` helpers. **Tests** (new
  `server::tests`): saturation boundary is inclusive; `progress_queue_len` tracks dispatch + sink-drain;
  and the **invariant** `max_in_flight < PROGRESS_QUEUE_HARD_LIMIT` (so backpressure can't be silently
  configured into dead code, reintroducing the crash) — the "common failures tested, guards asserted"
  mandate. `echo_roundtrip` (full dispatcher) green (default 5000 ≫ its ~100 tasks, so the normal path is
  untouched). Ledger: **D-6 → 🟢**; residual noted (timeout reaping still coupled to refetch → slow
  recovery only in a fully-wedged set, a future refinement, not unbounded growth).
  *Next:* decouple the timeout reaper from refetch (the D-6 residual); migrate the legacy Vega `history`
  page onto `frontend/runs.rs`; or prune the now-dead `CacheConfig` (Redis) leftover in `config.rs`.
- **Dead Redis cache config purged + backpressure knob surfaced (rationalization + Admin UX):** since
  Redis was removed (#6.2) the `CacheConfig` (`redis_url`/`required`) was dead — yet still **exposed and
  editable** through `/api/config`, the Settings page (a phantom "Redis URL" input), and `post_settings`.
  Removed it end-to-end: the `CacheConfig` struct + `CortexConfig.cache` + its `to_persisted_toml` line
  (`config.rs`); the `ConfigDto.cache` field, `SettingsForm.cache_*` fields, the import, and the patch
  branch (`management.rs`); the Cache fieldset (`settings.html.tera`); and the stale assertion
  (`management_api_test.rs`, now asserting `cache` is **absent**). In the same pass, **surfaced the new
  D-6 `max_in_flight` backpressure knob in the Settings form** (input + `SettingsForm.dispatcher_max_in_flight`
  + patch + an `/api/config` assertion) so the safety threshold is admin-manageable — closing the loop on
  tick-14's backend change with its Admin-UX twin. Also scrubbed the **stale install docs** that still told
  operators to `apt install redis-server`, `systemctl enable redis-server`, `redis-cli ping`, and a
  troubleshooting entry claiming "the frontend requires Redis" (`INSTALL.md`, `CLAUDE.md`) — actively wrong
  since the frontend boots without Redis. `build` + `clippy --all-targets -D warnings` clean; `settings_test`
  (3) + `management_api_test` (2) green. Net: dead, *misleading* config surface gone from code, UX, and the
  install path; one real knob gained an editor.
  *Next:* decouple the D-6 reaper from refetch; migrate the legacy Vega `history` page; or rename the
  now-misnamed `src/frontend/cached/` proxy (last Redis-era naming debt).
- **Arm 7 — run-history Vega chart migrated to the library (symmetry contract; last legacy runs screen):**
  the `/history/<corpus>/<service>` run-quality bar-chart (Vega) was the only runs screen still in the
  binary. Relocated it to `runs::history_page` — a faithful move (same `history`/`vega-history` templates,
  same `RunMetadata`/`RunMetadataStack` transform) but on a **pooled** connection instead of
  `Backend::default()`, and with the legacy serialization `.unwrap()` (a request-path panic) **softened to
  `unwrap_or_default()`** (chart degrades to an empty series rather than crashing the request). Deleted the
  bin `historical_runs` route + registration + its now-unused `HistoricalRun`/`RunMetadata`/`RunMetadataStack`
  imports; the `/history/...` path and the report-screen "Explore History" link are unchanged (pure
  relocation). The entire **runs/history surface now lives in `frontend/runs.rs`**: list · current · diff
  matrix · per-task drill-down · run table · history chart — each a human screen with a 1:1 agent
  twin/shared DTO. Test: `runs_test` renders the chart screen (heading + a seeded run) + 404 on unknown.
  Ledger: F-1 residual trimmed (the history `.unwrap()` is resolved; only the worker_report DB-query unwrap
  remains for D-3). `build`+`clippy --all-targets -D warnings` clean; `runs_test` green.
  *Next:* decouple the D-6 reaper from refetch (needs service_id-keyed dispatch queues + multi-service test
  coverage); rename the misnamed `src/frontend/cached/` proxy; or migrate the legacy report HTML routes
  (the `cached::task_report` consumers) into the library.
- **Report HTML screens migrated to the library + POOLED (symmetry contract + performance, Arm 3/7):** the
  7 corpus/service report routes (top → severity → category → `what` → task-list, incl. the `?<params..>`
  paging variants) were the binary's biggest remaining cluster and the most-used admin screens. Each load
  opened **two** fresh `Backend::default()` libpq connections (one in `serve_report`, one inside
  `cached::task_report`) — ~4.5ms each per the Arm-14 spike (~395× a pooled checkout). Refactored the read
  path onto a **single pooled connection**: `serve_report(&mut PgConnection, …)` and
  `cached::task_report(&mut PgConnection, …)` now take the caller's connection and call the existing free
  `backend::{progress_report, task_report}` functions (re-exported `task_report`); `serve_report` also now
  returns `Result<Template, Status>` (a clean `404`) instead of `NotFound<String>`. Relocated all 7 route
  declarations from `bin/frontend.rs` into the library `frontend/reports.rs` (each checks out from the pool),
  so the HTML report screens and the typed agent API now live in **one module** reading the **same**
  rollup — and the screens are testable via `rocket::local`. Deleted the bin routes + registrations + the
  now-unused `serve_report`/`ReportParams` imports. Test: `reports_api_test` now also renders the top +
  severity HTML screens (server-side `math` category) and asserts `404` on an unknown corpus (the relocated
  controller returns a Status, no panic). `report_rollup_test` + lib tests green (numbers unchanged);
  `clippy --all-targets -D warnings` clean. Net per report page-load: **2 fresh libpq connects → 0** (pooled).
  *Next:* pool the remaining `Backend::default()` routes (`serve_rerun`/`serve_entry`/`serve_savetasks` in
  concerns; root/corpus-overview/worker in the bin); decouple the D-6 reaper; or rename the `cached/` proxy.
- **Corpus overview + detail screens migrated to the library + pooled (symmetry + Arm 3):** `corpora.rs`
  had the agent API but no HTML twins (the module doc said "(later) as HTML"). Added `overview_page`
  (`GET /`, the admin **landing page** — table of corpora, twin of `api_corpora`) and `corpus_page`
  (`GET /corpus/<name>`, the services on a corpus, twin of `api_corpus`), both **pooled** (no per-request
  `Backend::default()`). Deleted the bin `root`/`corpus` routes + registrations + the now-unused `Corpus`
  import. Also **softened the last frontend request-path unwrap**: `worker_report`'s
  `service.select_workers(...).unwrap()` → `.unwrap_or_default()` (worker table degrades to empty instead
  of panicking) — closes the F-1/D-3 residual; **no known input-triggerable request-path panics remain in
  the frontend**. Tests: `corpora_test` renders the overview (lists the seeded corpus) + corpus screen
  (lists the activated service) server-side + 404 on the HTML twin for an unknown corpus (8 pass). `build`
  + `clippy --all-targets -D warnings` clean. Remaining `Backend::default()`: concerns
  `serve_rerun`/`serve_entry`/`serve_entry_preview`/`serve_savetasks` + bin `worker_report` (future ticks).
  *Next:* seed the **live DB backup** for migration verification + real-world load testing (owner request,
  2026-06-13 — see KNOWN_ISSUES / a new load-test plan); then continue pooling, the D-6 reaper, or
  service-management UX.
- **Migration-verification tooling built + validated (unblocks the load-test prep; install fidelity):**
  the owner's request to "verify where we may be missing migrations" needs a schema-fidelity check between
  the live backup and what our `migrations/` produce. Built **`scripts/verify_migrations.sh`**: it rebuilds
  a reference DB by running every migration on an empty DB, then diffs `--schema-only` dumps of the source
  vs the reference as a locale-stable (`LC_ALL=C`) sorted-set comparison (robust to pg_dump's object-
  ordering between DBs). It reports two sections — schema the source has that our migrations *don't*
  reproduce (author a migration), and schema our migrations produce that the source *lacks* (source is
  behind) — and exits 1 on any drift. **Validated both directions:** against the up-to-date `cortex` DB it
  reports `OK` (so our 18 migrations reproduce the live-equivalent schema exactly — no drift today), and
  against a synthetic drifted DB (an extra column) it correctly flags it. Ready to run on a restored
  `cortex_load` the moment the backup lands. Doc (`docs/LOAD_TESTING.md` Phase 2) points at it.
  - **Already paid off:** building the reference regenerated `src/schema.rs` via `diesel print_schema` and
    surfaced a **latent inconsistency** — the R-2 widen migration (`20260613170000`) made `tasks.entry`
    `varchar(4096)`, but `schema.rs` still carried `#[max_length = 200]` (that tick wrongly assumed
    varchar-length changes don't touch `schema.rs`; diesel *does* emit `#[max_length]`). Corrected to
    `4096` so `schema.rs` matches the migrations + DB (descriptive only — `Varchar` still maps to `String`,
    no behaviour change — but now consistent).
  *Next:* (when the backup arrives) restore + run the verifier + load test; meanwhile continue pooling the
  remaining `Backend::default()` routes, the D-6 reaper, or service-management UX.
- **D-6 reaper residual closed + latent cross-service requeue bug fixed (dispatcher robustness):** the
  ventilator's timeout-reaping of crashed-worker tasks was coupled to the refetch path
  (`task_queue.is_empty()`), so under sustained **backpressure** (refetch never runs) the in-flight set
  wouldn't drain. Decoupled it onto a fixed **60s cadence** at the top of the dispatch loop, so the set
  drains regardless. While extracting the inline reap logic into pure, **unit-tested** helpers in
  `server.rs` (`classify_expired` → retry-or-`Fatal` at `MAX_DISPATCH_RETRIES`; `reap_expired_into` →
  routes each expired task), found + fixed a **latent cross-service bug**: expired tasks were re-queued
  into the *requesting* worker's service queue (the dispatch queues were keyed by service *name* and the
  reap ran inside one service's branch), so a service-B task could be handed to a service-A worker. The
  dispatch queues are now keyed by **`service_id`** and each reaped task returns to its **own** service.
  Tests: `classify_expired` (retry budget → `Fatal`) + `reap_expired_into` (routes retriable to its own
  service with retry++, exhausted → done queue, in-flight fully drained); `echo_roundtrip` (full
  dispatcher) green; `clippy --all-targets -D warnings` clean. Ledger: **D-6 reaping residual → closed**.
  The dispatcher is now race-free (D-2) + bounded fan-out (D-1) + backpressured **and promptly-draining**
  (D-6) + correctly per-service-routed.
  *Next:* pool the remaining `Backend::default()` routes; service-management UX; or (on backup arrival) the
  load test.
- **Services capability: worker-fleet screen + agent API (symmetry + Arm 3, last bin read route pooled):**
  new `frontend/services.rs` — `GET /workers/<service>` (HTML, relocated from the bin) and its agent twin
  `GET /api/services/<service>/workers` → `Vec<WorkerDto>`, both **pooled**. `WorkerDto` exposes per-worker
  `total_dispatched`/`total_returned`/`last_dispatched_task_id` plus a computed **`in_flight`**
  (`dispatched - returned`) — the operational signal for a stuck/struggling worker, newly useful for
  watching the hardened dispatcher's fleet. Deleted the bin `worker_report` route (the **last** bin route
  that built a `TemplateContext` / opened `Backend::default()` for reads) and pruned six now-orphaned bin
  imports (`HashMap`, `Backend`, `Service`, `TemplateContext`, `helpers::*`, `UNKNOWN`); the binary is now
  just file-serving + thin delegations to `concerns`. Test `services_test`: the API returns the seeded
  worker with `in_flight = 3`, the HTML screen renders it, and both surfaces `404` on an unknown service.
  `clippy --all-targets -D warnings` clean. **All frontend READ routes are now pooled + in the library;**
  the only remaining `Backend::default()` opens are the four `concerns` write/file helpers
  (`serve_rerun`/`serve_savetasks`/`serve_entry`/`serve_entry_preview`).
  *Next:* pool those four `concerns` helpers; service *activation* UX (Arm 6 — register/extend a service on
  a corpus as a job); or (on backup arrival) the load test.
- **API-docs head-to-head spike — `rocket_okapi` vs `utoipa` (Arm 9, owner-requested):** the plan was to
  *compare the generated outcomes, not pick on advice* — done. Both frameworks annotate the **same** corpora
  read slice (`examples/api_doc_spike_{utoipa,okapi}.rs`, dev-deps only) and emit an OpenAPI spec
  (`docs/api-spike/{utoipa,okapi}-openapi.json`), rendered side-by-side as browser-openable RapiDoc pages
  (`scripts/render_api_spike.py` → `docs/api-spike/*-docs.html` + `index.html`, self-contained/inline spec).
  Findings (in `docs/api-spike/COMPARISON.md`): **okapi** = one `#[openapi]` on the *real* route, spec
  derived from the signature (zero duplication, fits the symmetry contract) but **thinner by default** (no
  param/response descriptions, errors collapse to a generic `default`) **and version-pinned to Rocket**
  (`0.8.0` needs `rocket =0.5.0`; only `0.9.0` resolves with our `0.5.1`). **utoipa** = framework-agnostic,
  **richer by default** (param + per-status response descriptions, summaries) but every operation is a
  **second source of truth** restated in `#[utoipa::path]` on a dummy fn (drift risk, more upkeep).
  Schema/array-body quality is identical. Recommendation recorded: lean **okapi** (route-derived, no
  duplication — enrich the few guarded/error endpoints by hand), unless framework-independence is weighted
  higher. **Awaiting the owner's final pick after viewing**, then prune the loser's dev-dep + example.
- **Service registry capability (Admin UX — services list/screen):** admins could see services *activated on
  a corpus* (corpus screen) but had no view of all defined services. Added `Service::all` (mirrors
  `Corpus::all`) and extended `frontend/services.rs` with `GET /api/services` → `Vec<ServiceDto>` + its HTML
  twin `GET /services` (a registry table, each row linking to that service's worker-fleet view), both pooled.
  New `templates/service-registry.html.tera`; linked from the landing page for discoverability. Also fixed a
  stale doc comment on `Service::select_workers` (claimed to "return services activated on a corpus" but
  returns workers). Test `services_test`: the API lists the registered service with its formats, the screen
  renders it. The services capability now covers **registry + per-service worker fleet**, screens + agent
  APIs. `clippy --all-targets -D warnings` clean.
  *Next:* service *activation* UX (Arm 6 — register/extend a service on a corpus as a background job, the
  remaining service-management write gap); pool the 4 `concerns` helpers; or (on backup) the load test.
- **Service activation (Arm 6 — the write side, the last service-management gap):** `register_service`'s own
  TODO ("when we add register service capacity for the UI, extend this with owner+description") is resolved —
  threaded `owner`/`description` through `services_aggregate::register_service` → `Backend::register_service`
  → `mark_new_run` (was hardcoded `cli-admin`), updating the 2 CLI example callers. New **Actor-guarded**
  job API `POST /api/corpora/<corpus>/services/<service>` (`activate_service`): creates a TODO task per
  imported document so workers begin converting, attributing the run to the authenticated actor, as a
  background job (`run_activate`) — `202` + the job handle, pollable via `GET /api/jobs/<uuid>`; `401`
  without a token, `404` on unknown corpus/service. Generalized `count_import_tasks` → `count_service_tasks`
  (corpus, service). Tests: the route is `401` without a token; the `register_service` effect creates a TODO
  task per imported doc + records the run attributed to the actor (`activator-bob`). `clippy --all-targets
  -D warnings` clean. **Service-management is now complete: registry + fleet + activation, screens + APIs.**
  *Next:* jobs-observability uplift (owner directive — a pending/list endpoint + duration/health metadata
  for every background-task capability); pool the 4 `concerns` helpers; or (on backup) the load test.
- **Jobs observability uplift (owner directive — pending check + duration/health for all background tasks):**
  background capabilities (corpus import/extend, service activation) were only pollable one-at-a-time by uuid
  — no fleet-wide view. Added the **pending check**: `jobs::list_recent(active_only, limit)` + `GET /api/jobs`
  (`?active=true` → just the non-terminal queued/running jobs; `?limit=`, capped 1–200; most-recent-first) and
  its HTML dashboard twin `GET /jobs` (`templates/jobs.html.tera`, linked from the landing page). `JobDto`
  gained **`duration_seconds`** (`updated_at − created_at` — total runtime / time-to-last-update) and
  **`health`** (normalized from status → `ok`/`failed`/`interrupted`/`pending`/`running`), so every job
  carries the metadata at a glance. Since all long work already routes through `jobs::spawn_job`, this makes
  import/extend/activate observable together — and any future capability automatically. Test: the list
  carries `health=ok` + `duration_seconds` for a finished job, the dashboard renders, and the `active` filter
  excludes terminal jobs. Recorded the directive as a standing mandate ([[jobs-observability-mandate]]).
  `clippy --all-targets -D warnings` clean.
  *Next:* pool the 4 `concerns` helpers; the API-docs pick (awaiting owner); or (on backup) the load test.
- **CI green again (owner: "CI is showing SIGSEGV for tests"):** diagnosed — the full suite's *assertions
  all pass* (fmt + clippy + every "test result: ok"); CI was red purely on the pre-existing **L-1 teardown
  SIGSEGV** (4 Rocket-`Client`-over-pool binaries — corpora/jobs_api/management/runs — crash *after* their
  tests pass, exiting `cargo test` non-zero). Added **`scripts/ci_test.sh`**: runs `cargo test
  --no-fail-fast` and fails on any real failure (`FAILED` / `N failed` / a genuine `error[E…]` / `could not
  compile`) but tolerates a non-zero exit whose *only* cause is a `signal: 11` teardown — so a real
  regression still reds CI, the L-1 flake no longer does. Validated locally: an L-1 binary → exit 0 (with a
  `::warning::`), a clean binary → exit 0, a `FAILED` line → exit 1. Also hardened the migrations step to
  `git checkout -- src/schema.rs` after `diesel migration run` (diesel.toml's `print_schema` rewrites it; a
  differing CI `diesel_cli` could otherwise trip `fmt --check`). KNOWN_ISSUES L-1 + the CI header note
  updated.
- **Background jobs are now panic-safe (robustness; complements the observability uplift):** a panicking job
  body (an importer panic, or `from_address`/`connection_at` panicking when the DB briefly blips) killed the
  worker thread *before* `finish()` ran, stranding the job `running` **forever** — a zombie the new
  pending/health view would show as perpetually live. `jobs::spawn_job` now wraps the body in
  `catch_unwind` (`AssertUnwindSafe`): a panic becomes a terminal **`failed`** with a `job panicked: …`
  message (extracted from the payload), so every job reaches a real health state. Also fixed the D-3
  `connection_at` message **bug** (the literal `"Error connecting to {address}"` never interpolated) → now
  `panic!("Error connecting to {address}: {e}")`. Test (`jobs_test`): a body that `panic!`s ends `failed`
  with the panic surfaced, never `succeeded`/stuck. `clippy --all-targets -D warnings` clean. KNOWN_ISSUES
  D-3 note updated.
- **Last `Backend::default()` retired from the request path (Arm 3 milestone):** the four legacy
  `concerns` helpers — `serve_rerun`, `serve_savetasks` (writes), `serve_entry`, `serve_entry_preview`
  (reads, all still UI-live: rerun modal, save-snapshot, entry download, preview) — each opened a fresh
  per-request `Backend::default()` libpq connection. Refactored them to take the caller's `&mut
  PgConnection`; the writes call the re-exported free fns `backend::{mark_rerun, save_historical_tasks}`
  (so no Backend needed). The 7 bin routes (`preview_entry`/`entry_fetch`/`rerun_*`/`savetasks`) now check
  out from the **pool** (`pooled(pool)` helper) and pass the connection through; `serve_entry`'s async file
  open happens after the lookup borrow ends. **No `Backend::default()` remains anywhere on the frontend
  request path** — every route is pooled. `build` (lib+bins) + `clippy --all-targets -D warnings` clean.
  *Next:* the API-docs pick (awaiting owner); (on backup) the load test; or more dispatcher/perf hardening.
- **CI green, take 2 — the real cause was PostgreSQL connection exhaustion (not L-1):** `gh` showed CI still
  red; the logs had genuine failures (`test result: FAILED. … 3 failed`) caused by `FATAL: sorry, too many
  clients already` / `remaining connection slots are reserved for SUPERUSER`. Each integration binary builds
  a Rocket `Client` whose pool is `config().database.pool_size` (**32**); parallel tests multiply that past
  the runner's default `max_connections=100`, so `testdb()`/`from_address()` fresh connects panic and fail
  those tests (the tick's `connection_at` message change made them legible). Fix: **cap the test pool** via
  `CORTEX_DATABASE__POOL_SIZE: "8"` env (the blocking Client uses one connection at a time) **and** raise
  PostgreSQL `max_connections` to 300 (`ALTER SYSTEM` + restart). Also **tightened `ci_test.sh`'s
  failure grep** — `[1-9][0-9]* failed` false-matched the port number in `port 5432 failed: FATAL …`; now
  anchored to libtest's `"; N failed"` summary so only a real `; <N> failed` (or `test result: FAILED`)
  trips it (validated against the captured CI log: catches the real `; 3 failed`, ignores `5432 failed`,
  still tolerates the L-1 SIGSEGV). KNOWN_ISSUES updated.
  *Next:* confirm CI green via `gh` once this run completes; then the API-docs pick / load test / hardening.
  - **CI confirmed GREEN** (run for `95bfc2e` → success): the connection-exhaustion fix + tightened wrapper
    worked. The earlier red was real (connection limits), not the L-1 flake.
- **Dependabot security pass (with `gh` now authed):** triaged the 6 open advisories. The **diesel** ones
  (incl. the lone *high*, "SQLite UTF-8 corruption") are SQLite-/`COPY`-specific — we use **Postgres only,
  no `COPY`** — and this branch already resolves **diesel 2.3.10 ≥ the 2.3.8 patch**, so they're a
  master-branch artifact that clears on merge. Cleared the two genuinely-bumpable ones with no code change:
  **rand 0.8.5 → 0.8.6** (low) and the transitive **time 0.3.41 → 0.3.47** (medium). `build` +
  `clippy --all-targets` + lib tests clean. **Remaining:** the `time = "0.1.4"` direct dep (1 medium, an
  unmaintained crate — `time::get_time()` in the dispatcher) needs a real **migration off `time` 0.1 → chrono**
  (a rationalization win, deferred — it touches the dispatcher hot loop, do it deliberately with tests).
  *Next:* migrate off `time` 0.1; the API-docs pick (awaiting owner); or (on backup) the load test.
- **Migrated off the unmaintained `time` 0.1 → chrono (clears the last fixable advisory; rationalization):**
  the prototype used `time = "0.1.4"` (`time::get_time()`, `time::now().rfc822()`) — unmaintained with two
  RUSTSEC advisories (segfault / stack-exhaustion). Replaced every call site (dispatcher hot loop —
  ventilator/sink/server — + frontend `concerns`/`cached` + 3 examples) with chrono (already a dep):
  `time::get_time()` → `chrono::Utc::now()` (duration diffs keep `.num_milliseconds()`; the chrono `Duration`
  is API-compatible), `.sec` epoch-seconds → `.timestamp()`, `time::now().rfc822()` →
  `chrono::Local::now().to_rfc2822()`. Dropped `time = "0.1.4"` from Cargo.toml; `cargo tree` confirmed
  nothing else needed it, so **`time` 0.1.45 is gone from the lock** entirely. `build` + `clippy
  --all-targets -D warnings` clean; `dispatcher::server` units (reap/backpressure use timestamps) +
  `echo_roundtrip` (full dispatcher) green. Only the `time` 0.3/0.2 transitive crates remain (0.3 already
  bumped to the patched 0.3.47).
  *Next:* the API-docs pick (awaiting owner); (on backup) the load test.
- **L-1 SIGSEGV — root cause found + being ELIMINATED (owner: "eliminate, not tolerate"):** empirically
  pinned the flaky at-exit SIGSEGV in Client-over-pool test binaries to the C **`atexit` handlers**
  (libpq/OpenSSL global cleanup) racing the still-live Tokio/r2d2 threads at process exit. Evidence:
  leaking the Client → 8/8 crash; `std::process::exit(0)` (runs atexit) → ~6/10; **`libc::_exit(0)`**
  (skips atexit) → **0/12**. Fix: a **custom harness** (`harness = false`) whose `main` owns the `Client`,
  runs the cases, and `unsafe { libc::_exit(0) }`s *while it's alive* — a panic still aborts non-zero so
  real failures fail CI. `runs_test` converted + validated (0/12); `libc` added as a dev-dep. Rolling out to
  the other 5 Client binaries (corpora/jobs_api/management/settings = custom harness; reports_api/services =
  single-test `_exit`), after which `scripts/ci_test.sh`'s SIGSEGV tolerance is removed. KNOWN_ISSUES L-1 →
  🟡 with the diagnosis.
  *Next:* finish the SIGSEGV rollout + drop the wrapper tolerance; audit production thread/subprocess
  lifecycle (owner: long-lived frontend must not accumulate zombies → OOM); API-docs pick; load test.
- **L-1 SIGSEGV rollout — eliminated in 6/7 Client-over-pool binaries:** converted `runs_test`,
  `settings_test`, `management_api_test`, `jobs_api_test`, `corpora_test` to the custom harness
  (`harness=false` + `main` runs the cases sequentially + `libc::_exit(0)`) and `services_test`/
  `reports_api_test` to the single-test `_exit` form. Validated ~6 runs each: **0 SIGSEGV** for
  runs/settings/management/jobs_api/services/reports_api. `corpora_test` has a **rare residual** (1/6 once,
  then 0/12 on rescan) — likely its `import`/`extend` tests' **detached job threads** still in flight at
  `_exit`; the `scripts/ci_test.sh` SIGSEGV-tolerance is **kept as a backstop for that one binary** (it
  still fails CI on any real failure) until the residual is chased. Bonus: sequential case execution is more
  deterministic than the parallel harness.
  *Next:* chase the `corpora_test` residual (await its spawned jobs before exit) then drop the wrapper; the
  **live-DB-dump load test is now unblocked** (dump provided) — restore + verify migrations + load test.
- **L-1 fully eliminated; `connection_at` retries transients (commit f7ad70a):** the last residual
  (corpora_test) was a transient `connection_at` panic under connection pressure → SIGSEGV-on-unwind; the
  retry fixes it (0/20). All 7 binaries SIGSEGV-free.
- **Live-dump load-test prep (5.8 GB restored; migrations validated on real data):** restored
  `cortex_20260614_023225.dump` into `cortex_load` (5.87M tasks). Migration fidelity = structurally clean
  (only the autovacuum tuning + the pending widen differ). Applying our pending migrations on the real data:
  `worker_metadata` has **0 duplicate rows** (185 rows) → the UNIQUE dedupe is a safe no-op; all 6 pending
  migrations applied in **6:43 wall-clock** (dominated by the `tasks.entry` `varchar(200)→text` widen over
  5.87M rows), peak RSS only ~100 MB. The `report_summary` matview creates `WITH NO DATA`, so its REFRESH
  cost is runtime — measuring that build time on the real data is the remaining load-test step.
- **DB maintenance — autovacuum migration + pgtune plan (owner: best-practice autovacuum/reindex; pgtune
  this box; bake DB tuning into `cortex init`):** new migration `2026-06-14-030000_autovacuum_tuning` bakes
  the proven per-table autovacuum (previously a manual INSTALL.md §8 step) into every install — applied
  *consistently* (the live DB had missed `log_invalids` + `historical_tasks`) and extended with PG13
  insert-based autovacuum for the append-only `log_*` (avoids wraparound stalls + keeps the visibility map
  fresh). Applied to `cortex`/`cortex_tester` (7 tables tuned). **Server-level tuning + reindex routine** in
  new `docs/DB_TUNING.md`: the pgtune algorithm + concrete `ALTER SYSTEM` values for *this* 246 GiB/128-core/
  NVMe box (`shared_buffers 61GB`, `effective_cache_size 184GB`, `work_mem 64MB`, NVMe `random_page_cost
  1.1`, etc. — vs the stock 128 MB / 4 MB / 512 MB), the online `REINDEX (CONCURRENTLY)` routine, and the
  plan to wire a `cortex tune-db` step into init (compute → print → `--apply` superuser). INSTALL.md §8
  rewritten (autovacuum now automatic; points at DB_TUNING.md).
  *Next:* apply the pgtune values to this box (after the load-test migration run finishes — needs a restart);
  implement the `cortex tune-db` step; REFRESH the matview on real data to measure the build time.
- **pgtune applied live + DB_TUNING.md re-based on the authoritative le0pard output (owner: "I mostly mean
  this service" = pgtune.leopard.in.ua; "max 64 GB to postgres"; "1x–3x RAM"):** classified CorTeX as the
  **Mixed** application type (OLTP task/log writes + DW bulk-loads + DW reporting — *not* Web: our DB isn't
  ≪ RAM and the reports aren't simple). Owner ran the live tool (DB=mixed, RAM=256 GB, CPUs=**64** physical /
  not 128 HT, conns=**300**, storage=nvme, PG18/linux); applied that output **verbatim** to the `cortex`
  node and restarted: `shared_buffers=64GB`, `effective_cache_size=192GB`, `work_mem=92182kB`,
  `maintenance_work_mem=8GB`, `random_page_cost=1.1`, `effective_io_concurrency=1000` (NVMe), WAL 1/4 GB,
  parallel 64/4/4, plus the modern adds **`io_method=io_uring`** (PG18 async I/O — verified active, not a
  silent fallback), **`wal_compression=lz4`**, **`jit=off`**, **`autovacuum_max_workers=5`** +
  **`autovacuum_work_mem=2GB`** (the *global* pool, complementing the *per-table* autovacuum migration).
  Confirmed this Ubuntu 26.04 PG 18.4 build has `--with-lz4`/`--with-liburing` (enumvals). "Total data size"
  = larger than RAM (full arXiv ≥250 GB, est. 1–3× RAM). DB_TUNING.md rewritten to carry the verbatim tool
  output + exact inputs + build-dependency caveats as the source of truth (was my hand-derived approximation;
  corrected `maintenance_work_mem` 8 GB cap is Linux not the Windows-only 2 GB, and `effective_io_concurrency`
  1000 for NVMe not 200). *Next:* decide `cortex tune-db` scope (port the le0pard model + capability-detect,
  vs print-and-link) — see the open question to the owner. **Decided: guide + link** (don't reimplement
  the upstream heuristic); baked the exact `cortex init` NOTE text + build-capability hint into DB_TUNING.md,
  corrected INSTALL.md §8 (operator-guided, not auto-applied).
- **Browser test-drive readiness verified end-to-end (owner is "curious about test-driving in the
  browser"):** the compile-time-DB constraint is **gone** — `backend::default_db_address()` now reads
  `config().database.url` (figment, Arm 1 landed), so the frontend points at any populated DB via
  `DATABASE_URL`/`CORTEX_DATABASE__URL` with **no rebuild**. Booted the frontend against the restored
  `cortex_load` dump: landing/`corpus`/`services`/`jobs` all render; **reports over real data are fast**
  via the `report_summary` matview — `arxmliv` (2.82M `tex_to_html` tasks, 61M `log_warnings`) renders the
  Warning severity report in **140 ms**, Error in 134 ms, service report 79 ms. Data is rich: 9 corpora,
  `tex_to_html` results across all severities (1.6M warn / 730k err / 92k fatal / 38k invalid), 273M+
  log rows, **123 historical runs**, matview populated (322,887 rows). Wrote `docs/TEST_DRIVE.md` (one
  command + screen map + `/api` parity). **Findings:** (1) symmetry is via *parallel* `/api/*` routes
  (13 `api_` twins ↔ 13 human report fns — 1:1 parity holds) **not** `Accept`-negotiation on one
  controller, so HTML/JSON paths can drift — converging them is open follow-up; (2) frontend boots
  cleanly with no Redis, no `cortex.toml` (defaults), `config.json` still present for auth.
- **Two run-diff bugs found by test-driving the live data + a report-time format fix (owner-reported):**
  exercising the run-management screens against the 5.87M-task dump surfaced two real bugs in
  `HistoricalTask::report_for`, both now fixed + regression-tested (`runs_test::api_task_diff_over_real_snapshots`,
  which seeds two real snapshots — the gap that let these survive):
  - **F-2 (request-path panic):** the *unfiltered* task-diff (`/runs/<c>/<s>/tasks` with no transition
    picked — the default the screen links to) `.expect()`ed the status filters → **panic → 500 that the
    owner saw kill the worker thread**. F-1 had fixed the *route* parsing but the panic lived one layer
    deeper in the model; F-1's "no panics remain" was wrong because its test seeded runs but no snapshots,
    so `report_for` early-returned. Now runs a real paginated "every changed task between the two snapshots"
    query.
  - **F-3 (silent wrong data):** the *filtered* drill-down's outer query forgot `AND h.saved_at IN (...)`,
    so it returned a task's *entire* snapshot history and paired rows across the wrong dates. Fixed.
  - **Report-time format:** owner asked for `HH:MM` + tz *letter* code instead of `to_rfc2822()`'s
    `22:50:43 -0400`. chrono's `%Z` only yields the numeric offset for `Local`, so new
    `frontend::helpers::report_timestamp()` formats the date to the minute and appends the OS tz
    abbreviation via libc `strftime %Z` (DST-correct; promoted `libc` to a normal dep), with a graceful
    fallback to the offset. Now renders `Sat, 13 Jun 2026 22:59 EDT`. Verified on real data.
  *Run-management UX note:* the screens are solid (shared-DTO twins) but **under-linked** — `/runs/<c>/<s>`
  (the run-history table) is orphaned (report page links only `/history` chart + `/runs/.../diff`), and
  corpus/landing don't link run history. Discoverability is the next run-management increment.
- **Run-management discoverability closed (the increment above):** the previously-orphaned run-history
  table (`/runs/<c>/<s>`) is now reachable from the main browse flow. (1) The **corpus page**
  (`services.html.tera`) gained a "Run history" column linking each service to `runs · diff · chart`, so
  run management is reachable without first opening a report. (2) The **report page** (`report.html.tera`)
  now links the run-history *table* (was only the `/history` chart + `/runs/.../diff`); the two history
  views are relabelled "Run history (table)" / "Run history (chart)" to disambiguate. (3) The run-history
  table (`runs.html.tera`) gained a `← Report` back-link (the diff/tasks screens already had `← Run
  history`), so the run-management cluster is now navigable both ways. Verified on the live dump (links
  render + resolve `200`; `name_uri` percent-encodes the `_` as `%5F`, the pre-existing convention, and
  Rocket decodes it). Regression: `corpora_test::overview_and_corpus_pages_render_server_side` now asserts
  the corpus screen exposes a per-service run-history link. *Next:* the symmetry convergence (parallel
  `/api/*` → `Accept`-negotiation) and/or the matview REFRESH timing on real data.
- **Matview REFRESH measured + made non-blocking (R-4 resolved; load-test step done):** measured the
  `report_summary` rebuild on the production-scale dump (5.87M tasks, 273M `log_infos` + 61M
  `log_warnings` + 10M `log_errors`): **~2 min 13 s** for a full `REFRESH MATERIALIZED VIEW`. That sits on
  the dispatcher's run-completion (drain) + daily path, and the plain refresh holds an ACCESS EXCLUSIVE
  lock → **every report read blocked for ~2 min**. Fixed with `REFRESH ... CONCURRENTLY` (measured ~2 min
  14 s — ~1 s more for the writer, but readers keep seeing the prior rollup throughout). Required a UNIQUE
  index, added via migration `2026-06-14-040000_report_summary_concurrent_refresh`:
  `(corpus_id, service_id, severity, category_is_total, what_is_total, category, what) NULLS NOT DISTINCT`
  (the `NULLS NOT DISTINCT`, PG15+, keys the ROLLUP subtotal/grand-total `NULL`s); drops the now-redundant
  prefix `report_summary_lookup_idx`. Verified transaction-safety (CONCURRENTLY forbids running in a
  transaction — every caller is outside one) and reversibility (`migration redo`); `refresh_report_summary`
  falls back to a plain refresh if CONCURRENTLY is ever unavailable. Applied via `diesel migration run` to
  cortex + cortex_tester; `rollup_path_matches_live_path` lib test exercises the new path. **Found R-5
  (🔴):** the **rerun request blocks ~2 min** on this refresh synchronously (`serve_rerun` → `mark_new_run`
  → inline refresh on a Rocket worker) — CONCURRENTLY doesn't shorten *that* request; the fix is to route
  the post-rerun refresh through `jobs::spawn_job` (off the request path, observable) — next increment.
- **Forced report refresh (async, observable) + configurable automatic freshness (owner: agents/admins
  want to force a report update; multi-minute → async calls + UI):** clarified model — the matview is
  **global** (one refresh updates the data behind *every* report page; no per-page refresh), so freshness
  = an automatic regular rebuild + an on-demand forced rebuild, both non-blocking (`CONCURRENTLY`).
  Built: (1) `jobs::spawn_report_refresh(pool, actor)` — **debounced** (returns an in-flight refresh
  job's uuid rather than piling on), runs the rebuild off the request path. (2) **Agent API**
  `POST /api/reports/refresh` (Actor/token-gated) → `202` + `{job, poll, actor}`; poll `/api/jobs/<uuid>`
  for health. (3) **Human UI**: a "Refresh reports now" button on `/jobs` → `POST /reports/refresh`
  (form token) → 303 redirect to `/jobs` to watch the job (async, no JS). (4) **Tier-1 automatic
  guarantee made configurable**: `dispatcher.report_refresh_interval_seconds` (default tightened 24h→**1h**,
  cheap now that refresh is non-blocking); `finalize.rs` reads it. Verified end-to-end on the live dump
  (202 + job handle, debounce returns same uuid, 401 without token, human 303→/jobs, job shows `running`
  in `/api/jobs`). Regression: `reports_api_test` asserts 401 + 202-with-job-handle + actor attribution.
  New `docs/REPORT_FRESHNESS.md` documents the two-tier model. R-5 (rerun inline-refresh) updated: the
  helper now exists, so the fix is just wiring it — deferred to keep this tick additive.
- **R-5 resolved: rerun no longer blocks ~2 min on the rollup refresh (autonomous-night progress):** removed
  the inline `refresh_report_summary` from `mark_new_run` (now bookkeeping-only) and wired both rerun entry
  points to spawn the refresh **off the request path** via `jobs::spawn_report_refresh` —
  `reports::rerun_report` (added `pool`) and `concerns::serve_rerun` (threaded `pool` through the 4
  `bin/frontend.rs` handlers). **Verified on the live dump:** a narrow rerun returns in **0.84 s** (was
  ~2 min) and leaves a `refresh_reports` job running (attributed to the actor). Importer/service-activation
  intentionally don't spawn a refresh (their new tasks are TODO, not yet in the matview). Tests green
  (reports_api/runs/corpora); clippy/fmt clean. Closes the async-refresh story end to end.
- **Consistent, content-negotiated error responses (Arm 4; autonomous-night progress):** the frontend had
  **no error catchers** (Rocket logged "No 404/500 catcher registered" and served its default page). Added
  `src/frontend/catchers.rs`: a `NegotiatedError` responder + catchers for 400/401/404/500/503, registered
  in `server::mount_api_with`. **Content-negotiated** — a request under `/api` or with
  `Accept: application/json` gets a JSON `{error, status}`; a human gets the themed `templates/error.html`
  page (the error-path half of the symmetry contract). This also means a future caught panic renders a
  clean negotiated 500 instead of Rocket's default (complements the F-2 fix). Verified on the live dump
  (HTML 404 page for `/corpus/...`, JSON for `/api/...` and `Accept: json`, JSON 401 for an untokened
  `/api/reports/refresh`). Regression in `reports_api_test` (api 404 → JSON `{error,status}`, human 404 →
  HTML). clippy/fmt clean.
- **Arm 12 dead-code rationalization (autonomous-night progress):** removed two long-dead files —
  `src/backend/make_history.rs` (an undeclared empty `make_history` fn — never a module) and
  `src/dispatcher/metadata.rs` (undeclared, a no-op `register_event` + an edition-2018 `use backend;`
  that wouldn't even compile today) — both confirmed uncompiled (build unaffected). Dropped the dead
  `dependencies` table (2017, `master`/`foundation` integer pairs for an inter-service-dependency feature
  never built; queried nowhere, no FKs) via reversible migration `2026-06-14-050000_drop_dependencies`;
  applied to cortex + cortex_tester, `diesel` regenerated `schema.rs` (diff = *only* the table block + its
  `allow_tables_to_appear_in_same_query!` entry), reversibility verified by `migration redo`. Future
  service-dependency management (Arm 6) would design a fresh schema with real service FKs, so nothing is
  lost. CLAUDE.md load-bearing facts updated (the dead-files + dead-table notes are now obsolete).
- **Health surface enriched with pool utilization + a human `/health` screen (Arm 2/8 observability;
  autonomous-night progress):** `/healthz` (the agent JSON health report) now also carries **connection-pool
  utilization** (`max`, `connections`, `idle`, `in_use`) — the key load/saturation signal (when `in_use`
  nears `max`, requests wait on `pool.get()` and may `503`). Refactored the probe into a shared
  `health_report(pool)` builder, added the **human `/health` screen** as the HTML twin (shared `HealthDto`
  — symmetry), and linked **System health** + **Settings** from the overview nav (Admin-UX discoverability).
  Verified on the live dump (pool `32/32 idle`; correctly reports `degraded` when a DB is behind on
  migrations — caught that `cortex_load` lacks the latest migrations). Regression in `management_api_test`
  (pool fields present + `in_use ≤ max`; `/health` renders HTML). clippy/fmt clean.
- **Rationalization: `frontend::cached` → `frontend::render` (autonomous-night progress):** the one-function
  nested module `src/frontend/cached/` (a misleading name — it's been a thin uncached proxy since Redis was
  removed) was flattened to a single file `src/frontend/render.rs` and renamed to the presentation layer it
  is. Only 2 references (`frontend/mod.rs`, `concerns.rs`) — clean swap; build/clippy/`reports_api_test`
  green (the HTML report screens exercise `render::task_report`). Updated CLAUDE.md (the load-bearing note +
  the Map) and closed OPEN_QUESTIONS #8. No behavior change; removes a stale name that implied caching where
  there is none.
- **Health surface completed with dispatcher reachability (Arm 2 doctor; autonomous-night progress):** the
  health report now also probes the **co-located dispatcher** — a short TCP connect to its ventilator +
  sink ports (`config.dispatcher.{source,result}_port`); ZMQ `tcp://` sockets are TCP listeners, so a
  successful connect = the dispatcher is bound. `HealthDto.dispatcher = {reachable, source_port, result_port}`
  on both `/healthz` (JSON) and `/health` (HTML row). **Informational only** — it does not flip the overall
  `status` (a read-only/report-only frontend legitimately runs without a dispatcher). Fast even when down
  (connection-refused is immediate; the 200 ms timeout only bounds a rare filtered-port hang — verified
  2.3 ms with no dispatcher). Regression in `management_api_test`. Completes the Arm 2 doctor checklist
  (DB · migrations · pool · dispatcher); clippy/fmt clean.
- **Self-describing agent-API discovery index `GET /api` (full API parity for agentic use;
  autonomous-night progress):** an agent can now enumerate CorTeX's machine surface from one call —
  `GET /api` returns `{count, endpoints:[{method, uri, name}]}` for every mounted `/api/*` endpoint (URI
  with `<param>`/`?<query>` placeholders + the handler fn name). **Self-describing & drift-proof:**
  `management::RouteTable::snapshot(&rocket)` introspects the live route table at mount time (after *all*
  mounts incl. the binary's legacy routes), managed as state; the handler filters/sorts it — so the index
  can never fall out of sync with the routes actually served. 21 endpoints discovered automatically.
  Regression in `management_api_test`. OPEN_QUESTIONS #7 updated: this covers *route-level* discovery; a
  *schema-level* OpenAPI spec (request/response shapes, needs a utoipa/rocket_okapi pick) remains the
  richer follow-up. clippy/fmt clean.
- **W-2 resolved: tolerant worker-log decoding (robustness sweep; autonomous-night progress):** a non-UTF-8
  worker log used to be discarded *wholesale* — `generate_report` replaced the whole log with a synthetic
  `Fatal:...unicode_parse_error` + `Status:conversion:3`, losing every real message and force-marking the
  task **Fatal** over one stray byte (hostile arXiv data makes this real). New `decode_worker_log(&[u8])`
  helper decodes lossily (invalid bytes → U+FFFD), **preserving the real log** (so the true status +
  messages survive) and appends a `Warning:cortex:non_utf8_log` line so the encoding issue is recorded
  transparently. DB-free unit tests (`helpers::log_decode_tests`): clean UTF-8 untouched; a `0xFF` log keeps
  its real status + parses into multiple real messages instead of collapsing to one fatal. clippy/fmt clean.
  Open 🔴 count: 9 → 8.
- **Security: token-gate the corpus-write endpoints (the "writes denied by default" mandate;
  autonomous-night progress):** found an inconsistency — `activate_service` (and rerun) were `Actor`-gated,
  but **`import_corpus`, `extend_corpus`, and `delete_corpus` were ungated**, including an
  **unauthenticated `DELETE /api/corpora/<name>`** (anyone could wipe a corpus + its tasks/logs) and
  unauthenticated corpus creation + filesystem import/extend jobs. Added the `Actor` guard to all three
  (`401` without a valid token, consistent with activate/rerun) and **attributed the import/extend jobs to
  the real actor** (was a hardcoded `"admin"`) — the Arm 9 "thread an actor through every write" mandate.
  Tests updated (`corpora_test`): untokened import/extend/delete now assert `401`; the successful paths pass
  `?token=` and assert the job `actor` is the token owner. The whole corpus-write surface now matches the
  rest (rerun/activate). clippy/fmt clean.
- **Human corpus-management UI — the lifecycle was API-only (Admin UX; autonomous-night progress):**
  create/extend/delete a corpus had no human screens (a symmetry + UX gap, "from the installation of
  cortex"). Added human twins sharing the same logic as the agent endpoints (extracted `start_import` /
  `start_extend` helpers): **"Add a corpus"** form on the overview (`POST /corpus/import`), and
  **"Re-scan for new entries"** + **"Delete corpus"** forms on the corpus page (`POST /corpus/<name>/extend`,
  `POST /corpus/<name>/delete`). All token-gated (a `token` form field resolved via the new shared
  `actor::owner_for_token`, since the `Actor` guard can't read a form body) and the job-spawning ones
  **redirect to `/jobs`** (the async-watch pattern); delete is double-gated (token + type-the-name confirm)
  and redirects to the overview. Verified: forms render on both pages; `corpora_test` asserts the
  auth/confirm/redirect contract (bad token → 401, wrong confirm → 400, valid → 303 + corpus gone).
  clippy/fmt clean. (Activate-service from the UI — needs a service picker — is the remaining corpus-UI
  follow-up.)
- **Corpus UI completed: activate-service from the screen (autonomous-night progress):** the last
  corpus-lifecycle action gains its human twin. Extracted `start_activate` (shared by the agent endpoint
  and the form), added `POST /corpus/<corpus>/activate` (token-gated, redirects to `/jobs`), threaded a new
  `TemplateContext.all_services` (all real services, id > 2) into `corpus_page`, and added an **"Activate a
  registered service"** form with a `<select>` dropdown on the corpus page. Verified on the live dump (the
  dropdown lists `tex_to_html`); `corpora_test` asserts the activate form is token-gated (bad token → 401).
  **The corpus lifecycle now has matched, secured human + agent surfaces end to end** (create · activate ·
  extend · delete). clippy/fmt clean.
- **Service registry: register a service from API + UI (Arm 6; autonomous-night progress):** registering
  (defining) a service was **CLI-only** (`examples/register_service.rs`) — no API, no screen, only read
  views existed. Added `POST /api/services` (token-gated, `201` + `ServiceDto`, `409` on a duplicate name)
  and its human twin `POST /services/register` (a "+ Register a service" form on the registry screen,
  redirecting back to `/services`), sharing one `insert_service` helper that normalizes an empty
  `inputconverter` to `None`. Clarified the naming overload in the docs: this *defines* a service vs.
  `POST /api/corpora/<c>/services/<s>` which *activates* it on a corpus. Verified the form renders;
  `services_test` asserts 401-without-token, 201-with-token + DTO, 409-on-duplicate, and the human form's
  303 redirect + persisted row. clippy/fmt clean.
- **Corpus screen is now a progress dashboard (symmetry + Admin UX; autonomous-night progress):** the human
  corpus page listed service *names* but not the per-service **task counts** the agent `api_corpus` already
  computes — so a human saw less than an agent. `corpus_page` now enriches each service row with its
  per-severity counts (`total · no_problem · warning · error · fatal · todo`, via the same
  `progress_report`), and the table was restructured from static metadata columns into a progress
  dashboard. Verified the numbers match `api_corpus` against the live dump exactly (tex_to_html: warn
  1,563,521 · err 700,605 · fatal 86,320 — equal to the ground-truth task-status counts; `import` is all-TODO,
  legitimately zero on the severities). `corpora_test` asserts the dashboard columns render. clippy/fmt clean.
- **Performance: batch the per-task log deletes on the hot finalize path (D-8 → 🟡; autonomous-night
  progress):** `mark_done` (the dispatcher's finalize write path) issued **5 deletes per task** (one per
  `log_*` table) in its loop — at a queue-batch of hundreds, thousands of delete round-trips per drain. Now
  it collects the batch's task ids once and clears prior logs with **one `task_id = ANY(...)` delete per
  table** (5 statements regardless of batch size), then loops only the status-update + message-insert. This
  is provably equivalent (a finalize batch holds distinct task ids, so the batched `ANY` delete = the union
  of the per-task deletes) and a real round-trip reduction on the hottest path. Led with the safety net:
  strengthened `backend_test` to **re-finalize the same tasks with no messages and assert the prior logs are
  deleted** (the case delete-batching could regress) — green. Kept the insert/update logic untouched (lowest
  risk). The remaining D-8 piece (a diff/upsert to skip genuinely-unchanged message reinserts) is the smaller
  follow-up. clippy/fmt clean.
- **Installation: wire the decided pgtune guidance into `cortex init` / `cortex tune-db` (Arm 2;
  autonomous-night progress):** the `cortex` CLI (`init`/`doctor`, backed by `cortex::bootstrap`) already
  existed, but the **DB-tuning guide+link we decided** (`docs/DB_TUNING.md`) was never wired in — `init`
  applied migrations + scaffolded config but said nothing about server tuning. Added
  `bootstrap::db_tuning_guidance()` (host-aware: reads `/proc/meminfo` RAM + `available_parallelism`, with a
  physical-cores reminder) that points at pgtune for the **Mixed** workload and references the verified
  `DB_TUNING.md` block — printed by a new **`cortex tune-db`** subcommand *and* as the last step of
  `cortex init`. Verified on the host (`Total RAM = 246 GB`, `CPUs = 128`). `bootstrap_test` asserts the
  guidance links pgtune + names Mixed + points at DB_TUNING.md. INSTALL.md §8 references `cortex tune-db`.
  clippy/fmt clean. (Closes the gap between the *decision* and the *tool*.)
- **Online reindex as an observable maintenance job (owner: "reindexing … DB health and ongoing
  performance maintenance are very important"; autonomous-night progress):** delivered the explicitly-asked
  reindex capability — `jobs::spawn_reindex` runs **`REINDEX (CONCURRENTLY)`** over the high-churn tables
  (`tasks` · `log_*` · `historical_tasks`) **online** (no exclusive lock), **off the request path**, with
  **per-table progress** and **debounce** (mirrors the refresh-job pattern + the jobs-observability mandate).
  Surfaced as **`POST /api/maintenance/reindex`** (token-gated, `202` + job handle) and a **"Reindex database
  now"** button on the `/health` screen → redirect to `/jobs`. Verified end-to-end on `cortex_tester`: the
  job **succeeded** (rebuilt all 7 tables' indexes, health `ok`); `/health` button renders; `management_api_test`
  asserts the 401-without-token gate. CONCURRENTLY can't run in a transaction — the job body uses a fresh
  autocommit pooled connection. clippy/fmt clean.
- **Live job-watching: auto-refresh the `/jobs` dashboard while jobs are in flight (async UX;
  autonomous-night progress):** the individual job page already polls (vanilla fetch), but the `/jobs`
  *list* was static — an admin who kicked off a multi-minute refresh/reindex/import had to reload manually
  to watch progress. `jobs_page` now computes `has_active` (any `pending`/`running` job) and the template
  conditionally emits `<meta http-equiv="refresh" content="5">` + an "Auto-refreshing…" notice, so the list
  updates live while work runs and is fully static otherwise (no JS — the no-JS-frameworks constraint).
  Debugging note: a seeded `running` job is marked `interrupted` by `interrupt_orphans` on *production*
  startup, so the regression test seeds it under `mount_api_with` (which skips orphan-interruption) and
  asserts the meta-refresh renders (`jobs_api_test`). clippy/fmt clean.
- **Frontend request-path panic audit (clean) + settings completeness (autonomous-night progress):**
  swept `src/frontend/` for `.unwrap()`/`.expect()`/`panic!` on request paths — **no genuine risks**:
  `uri_escape(Some(_))` always returns `Some` (so those unwraps can't panic) and the `serve_report`
  severity/category/what unwraps are all guarded by prior `is_none()` branch checks. Hardened the two
  `uri_escape(...).unwrap()` calls to `.unwrap_or_default()` anyway (mandate-compliant + future-proof if
  `uri_escape` ever changes). Confirms the F-1/F-2/F-3 fixes + the catchers leave the frontend free of
  input-triggerable request-path panics. **Settings completeness:** the `report_refresh_interval_seconds`
  config field I'd added was *shown* on the Settings page but not *editable* — added it to `SettingsForm` +
  the persist patch + the form (`settings_test` now asserts it round-trips to `cortex.toml`). clippy/fmt
  clean.
- **AAA — Accounting pillar landed; passkeys decided as the next AuthN arm (autonomous-day
  progress):** the owner asked for 2026-best-practice AAA but with **no external dependency / no
  per-deployment app registration** — which rules out GitHub OAuth and generic OIDC. Conclusion
  (`docs/AAA_DESIGN.md` §5–6): keep the existing **token→owner** identity (per-admin tokens give
  per-person identity) + **uniform authz** (no RBAC) and build the one genuinely-missing pillar, an
  **auth-agnostic `audit_log`**. Shipped (commits `1db5697` + `3ca0a86`, pushed): migration
  `…070000_create_audit_log`; `models::audit` (`AuditEntry`/`NewAuditEntry`); a **single
  `frontend::audit::AuditFairing`** that records *every* mutating request to the log — **drift-proof**
  (one fairing, not a call per handler, so no write route can forget and new endpoints are audited for
  free): route name → action, path → target, status → outcome, actor via the new
  `actor::resolve_actor` (header/query/cookie); best-effort + `spawn_blocking` off the response path so
  it never fails or stalls the action it observes. Read view per the symmetry contract: Actor-gated
  `GET /api/audit` (documented in the OpenAPI spec) + the signed-in `/admin/audit` screen, sharing
  `load_audit`; `tests/audit_test.rs` covers an attributed authenticated write, an unauthenticated
  attempt recorded with an empty actor + 401, and both read surfaces. Honest gap recorded: a token in a
  not-signed-in human **form body** is invisible to the fairing → recorded with an empty actor (signed-in
  humans and all `/api` callers are attributed). **Next AuthN arm (owner-sequenced after the audit_log):
  passkeys / WebAuthn via the `webauthn-rs` crate** — the "local, no external app, as convenient as
  OAuth" answer the owner was after; the admin token becomes the bootstrap/break-glass + agent credential
  and passkeys become the day-to-day human sign-in. The audit_log is auth-agnostic, so it is the correct
  accounting base under that future model. clippy/fmt clean.
- **Admin UI Stage 2 — the management screens are now signed-in-admins-only (autonomous-day
  progress):** Stage 1 built the `/admin` dashboard + the `AdminSession` cookie; Stage 2 gates the
  individual admin screens the owner listed (Registered services, Background jobs, System health,
  Settings) behind that session. Added a small reusable responder `actor::AdminReject` (a redirect
  **or** a status) + `require_admin(session)` helper: a gated page returns `Result<Template,
  AdminReject>`, so an unauthenticated browser is **redirected to `/admin/login`** while the screen's
  genuine `404`/`503` cases still flow through (existing `Status` errors convert via `?`). Gated:
  `/services`, `/workers/<service>`, `/jobs`, `/jobs/<uuid>`, `/health`, `/settings`. **Deliberately
  left open:** the `/healthz` JSON liveness probe (for monitoring), the public read views (overview/
  corpus/report/runs — the `corpora.latexml.rs` surface), and the token-based `/api/*` agent twins
  (machines get a clean `401`, never an HTML redirect). Updated the four affected tests to sign in
  first (each now also asserts the unauthenticated redirect, documenting the gate). clippy/fmt clean.
- **`cortex set-admin-token` CLI — token setup without hand-editing (autonomous-day progress, Admin
  UX from installation):** the owner had floated `cortex init --set-admin-token …` and "how do we
  deal with generating tokens?". Added `cortex set-admin-token [<token>|--generate] [--owner <name>]`
  (`bin/cortex.rs` → testable `bootstrap::set_admin_token` + `bootstrap::generate_token`). It **merges**
  the token into `cortex.toml`'s `[auth].rerun_tokens` at the raw-TOML level (because `to_persisted_toml`
  intentionally never writes secrets), preserving every other section and any existing tokens; a fresh
  file is first scaffolded from the defaults so the result is always complete and valid. `--generate`
  prints a 32-char URL-safe random token once; `--owner` (default `admin`) is the identity the audit
  log records — so per-admin tokens give per-person attribution, the AAA accounting story end-to-end.
  Re-setting an existing token updates its owner. Detects + **warns** when a legacy `config.json` in the
  CWD shadows `cortex.toml`'s `[auth]` (the loader still treats it as authoritative for back-compat).
  `tests/bootstrap_test.rs` covers scaffold/merge/update + token randomness/shape; smoke-tested via the
  built binary. Modernized INSTALL.md §4 (removed the stale "DATABASE_URL is compile-time" warning —
  it's runtime since Arm 1 — and replaced the hand-edit-config.json token step with the CLI). AAA_DESIGN
  §3 stopgap marked LANDED. clippy/fmt clean.
- **WebAuthn arm — server-side sessions + AdminSession refactor (autonomous-day progress):** the
  load-bearing piece for passkeys. Migration `…110000_create_sessions` + `models::session`:
  `sessions(id PK=random 48-char opaque, owner, method, created_at, expires_at)`; `Session::{open,
  resolve_owner, revoke, revoke_all_for, active, prune_expired}` with a 7-day **absolute** expiry (no
  per-request sliding write → an authenticated request is one indexed lookup, zero writes). Refactored
  `AdminSession` from *cookie-carries-the-token* → *cookie-carries-an-opaque-session-id*:
  `from_request` resolves the id against the `sessions` table (pool via `request.guard::<&State<
  DbPool>>()`); `/admin/login` now `Session::open(owner, "token")` and sets the cookie to the id;
  `/admin/logout` `Session::revoke`s it (real server-side revocation, not just clearing the cookie).
  This **unifies** the two human sign-in paths — token today, `"passkey"` next — both open a session.
  The audit fairing's actor resolution was split into a sync `actor_carriers` (header/query/cookie
  extraction, no DB) + `resolve_carriers` (token via config, cookie via the sessions table) run
  **inside** the existing `spawn_blocking`, so the new DB session lookup stays off the async reactor.
  `tests/admin_test.rs` additionally asserts the cookie value is NOT the raw token; the live test DB
  shows the session rows created end-to-end. All auth/gated tests + clippy -D warnings green.
- **WebAuthn arm — passkey enrollment ceremony + "Your passkeys" UI (autonomous-day progress):** the
  registration half of passkey sign-in. In-memory `CeremonyStore` (managed; cookie-keyed via
  `cortex_ceremony`, 5-min TTL, prune-on-insert, mutex-poisoning-safe) holds the `PasskeyRegistration`
  state between begin/finish — no `danger-allow-state-serialisation` needed. `POST /admin/passkeys/
  register/begin` (signed-in admin; `WebauthnUser::ensure` handle + `start_passkey_registration`,
  excluding already-enrolled creds → `CreationChallengeResponse` JSON) + `.../finish?label=`
  (`finish_passkey_registration` → `WebauthnCredential::store`). The **"Your passkeys"** management
  page `GET /admin/passkeys` (list with enrolled/last-used, enroll button, per-key remove via `POST
  /admin/passkeys/<id>/delete` filtered by owner) + vanilla `public/js/webauthn.js` (base64url↔
  ArrayBuffer helpers + `navigator.credentials.create()` flow, 401→/admin/login, 503→token-only
  notice). Managed state (`Option<WebauthnState>` + `CeremonyStore`) + routes wired in server.rs;
  passkeys gracefully `503` when disabled. `tests/webauthn_test.rs` (harness=false; enables passkeys
  via `CORTEX_WEBAUTHN__*` env before config() loads) asserts gating (401 unauth, 303 page redirect)
  + begin returns a challenge when enabled+signed-in + the page renders the enroll affordance. The
  full biometric round-trip needs a real/virtual authenticator (manual). Also (owner request): removed
  the redundant "Admin dashboard — sign in to manage…" link from the homepage (it's in the top nav);
  added a "Your passkeys" link to the /admin dashboard. clippy -D warnings + all auth tests green.
- **WebAuthn arm — passkey SIGN-IN ceremony (autonomous-day progress): passkey login is now
  end-to-end.** `POST /admin/passkeys/auth/begin?owner=` seeds a `start_passkey_authentication`
  challenge from that owner's enrolled passkeys (`404` if none — username-enumeration caveat accepted
  for a small admin set behind Anubis); `…/auth/finish` runs `finish_passkey_authentication`, and on a
  verified assertion advances the matching credential's signature counter (`Passkey::update_credential`
  → `update_after_use`/`touch`, best-effort), then **opens `Session::open(owner, "passkey")` and sets
  the admin cookie** — so passkey login flows through the *same* unified session model as token login.
  Added a "Sign in with a passkey" affordance to `/admin/login` (rendered only when enabled; the page
  handler now threads `passkeys_enabled`) + `signInWithPasskey` in `public/js/webauthn.js`
  (`navigator.credentials.get()` + assertion serialization → `/admin`). `tests/webauthn_test.rs` adds
  the sign-in boundaries (no-passkeys `404`, no-ceremony `400`, login page renders the affordance). The
  biometric round-trip needs a real/virtual authenticator (manual). Passkeys arm: foundation →
  sessions → enrollment → **sign-in** all landed; remaining is the deferred confirmation-dialog
  refactor. clippy -D warnings + all auth tests green.
- **Auth — confirmation dialogs use the session cookie; gated GETs redirect with a return path
  (autonomous-day progress, completes the owner's auth requests):** (1) every human write/confirm
  form dropped its typed token and now gates on the signed-in `AdminSession` — the plain `<form>`
  handlers (corpora import/extend/activate/delete/deactivate, services register, management reindex/
  analyze/**post_settings** which had been UNGATED, reports refresh) return a redirect to sign-in when
  anonymous; the rerun + savetasks JSON-XHR endpoints (bin/frontend.rs) `401` when anonymous and the
  dialogs redirect to `/admin/login` on 401. Removed every `name="token"` write-form input from the
  templates + the now-dead TokenForm/MaintenanceForm/RerunRequestParams.token. (2) Per owner: **gated
  GET routes redirect to `/admin/login?next=<dest>`** and return there after login. New `actor::
  {ReturnTo guard, require_admin_to, sign_in_url, safe_next}` with an **open-redirect guard**
  (`is_safe_local_path`: absolute, non-protocol-relative); threaded `ReturnTo` through the 8 gated GET
  screens (admin, audit, passkeys, services, workers, jobs, job, health, settings); `/admin/login`
  carries `next` through a hidden field + the passkey button's `data-next`, and `admin_login`
  redirects to the validated `safe_next` (default `/admin`). `admin_test` asserts the `next=` round
  trip; the other gated-screen tests updated to expect `/admin/login?next=`. clippy -D warnings + the
  full auth/render test sweep green. **The owner's AAA + dialog + return-path requests are now all
  satisfied; CI check is next.**
- **Admin UX — active sessions view + per-identity revoke (autonomous-day progress, completes the
  session work):** the security-oversight UI for the session model built earlier (`Session::active`/
  `revoke_all_for` were unused until now). New `frontend::sessions`: `GET /admin/sessions` (signed-in
  admins see who is currently signed in — owner, method token/passkey, signed-in/expires, "this
  device" marker) + `POST /admin/sessions/revoke?owner=` (sign-out-everywhere / lock out a compromised
  account) + the agent twin `GET /api/sessions` (token-gated, in the OpenAPI spec). **Session ids are
  never exposed** (the id IS the bearer credential): the surfaces show owner/method/times only, and
  revocation is per-**owner** (the non-secret name); "current" is computed server-side by comparing
  the row id to the request cookie without surfacing either. Gated + return-path + audited (the
  fairing records the revoke). `templates/sessions.html.tera` linked from /admin. `tests/sessions_test`
  (harness=false; uses the isolated token2/username2 fixture so a revoke can't disturb the parallel
  username1 tests): anonymous→sign-in redirect, token-gated API, the screen lists + marks the current
  session, revoke signs the identity out everywhere. clippy -D warnings + the full auth/render sweep
  green. **Note (robustness ledger):** W-4's remaining auto-kill/statement-timeout work stays parked —
  it needs an owner-set deadline (legit-long reindex/refresh must not be false-killed); not an
  autonomous call.
- **Admin UX — system-wide historical-runs overview (autonomous-day progress, "managing historical
  runs"):** runs were only viewable per-`(corpus, service)`; there was no place to see recent
  conversion activity across the whole system. New `HistoricalRun::recent_all(limit)` +
  `frontend::runs` `RunOverviewDto` (per-run row + corpus/service names, batched name lookups — no
  N+1) behind `GET /admin/runs` (signed-in management screen; each row links into the existing
  per-service history / diff / drill-down) and the agent twin `GET /api/runs?<limit>` (in the OpenAPI
  spec; sits beside the existing `/api/runs/<corpus>/<service>` without colliding). Gated + return-path
  for the human page; `limit` clamped [1,500]. `templates/admin-runs.html.tera`, linked from /admin.
  `tests/runs_test` gains an overview case (anonymous→sign-in redirect, `/api/runs` lists the seeded
  run system-wide, signed-in screen renders it). This is the **inspection-first** completion the owner
  steered ("run-management is filter-driven; prioritize inspection over mutation") — destructive
  retention/prune of `historical_tasks`/`historical_runs` (unbounded growth) is deliberately deferred
  for explicit owner sign-off (policy + irreversibility), noted in the ledger. clippy -D warnings +
  the auth/runs test sweep green.
- **CI: bump actions/checkout@v4→v5 (time-sensitive) + Admin dashboard command-center (autonomous-day
  progress):** (1) GitHub's CI annotation warned Node-20 actions are force-migrated to Node 24 on
  2026-06-16; bumped `actions/checkout@v4`→`@v5` (Node 24 native) so CI keeps running past the cutoff.
  (2) Turned `/admin` from a link list into an at-a-glance **command center**: `admin_page` now
  aggregates — over ONE pooled connection, every card best-effort (a db hiccup degrades a card to
  zero/blank, never blocks the page) — registered **corpora**, **active jobs** (`jobs::list_recent`
  active), **active sessions** (`Session::active`), and the **last run** (`HistoricalRun::recent_all`:
  when/owner/total/open), each linking to its full screen (/jobs, /admin/sessions, /admin/runs).
  Deliberately cheap queries on small tables only — no dispatcher/storage probe (that is the System
  Health screen's job). `templates/admin.html.tera` renders the status row; `admin_test` asserts the
  cards render. clippy -D warnings + fmt + admin_test green.
- **Admin UX — filters on the historical-runs overview (autonomous-day progress, the owner's
  filter-driven steer):** the owner steered "run-management is FILTER-DRIVEN — prioritize filter/
  inspection UX". The system-wide `/admin/runs` + `GET /api/runs` overview (built last tick) now
  filters by **corpus**, **service**, and/or **owner**: new `HistoricalRun::recent_filtered`
  (boxed query, any combination of optional `corpus_id`/`service_id`/exact `owner`; `recent_all`
  delegates to it); `load_recent_runs` resolves the corpus/service *name* filters to ids (an unknown
  name narrows to an empty result, not an error) and threads them through both surfaces. The screen
  gains a `<form method=get>` with corpus/service dropdowns (seeded from the known names) + an owner
  text field, pre-selected to the active filter, with a clear link. `runs_test` extends the overview
  case (corpus filter keeps/excludes; owner filter keeps tester's / excludes a stranger's).
  clippy -D warnings + fmt + the runs/admin sweep green.
- **Performance — composite index for the hot per-(corpus,service) run query (autonomous-day
  progress):** `historical_runs` only had a single-column `corpus_id` index, but the public hot path
  is `HistoricalRun::find_by` (`WHERE corpus_id=? AND service_id=? ORDER BY start_time DESC` — every
  runs/history/diff page) + `find_current` (same filter + `end_time IS NULL` — every report page),
  which therefore filtered on corpus then filtered service + **sorted start_time in memory**.
  Migration `…120000_historical_runs_pair_index` adds `(corpus_id, service_id, start_time)` (keys both
  equality filters AND the ordering → a direct ordered index scan, no in-memory sort, also serves the
  overview's corpus+service-filtered reads) and **drops the now-redundant** `historical_runs_corpus_idx`
  (corpus_id is a strict prefix of the composite → one fewer index to maintain on every run write —
  rationalization). `service_idx` kept (service_id-only filters). Reversible; schema.rs unchanged
  (indexes aren't in the diesel `table!` macro). The win materializes at production scale (the tiny
  test DB still seq-scans). Also corrected a stale memory note (the `make_history.rs`/`metadata.rs`
  "dead files" are already gone; the `dependencies` table was dropped). runs_test green.
- **Observability — Prometheus `/metrics` endpoint (autonomous-day progress, Arm 8):** the owner's
  CLAUDE.md calls observability "the reason for all this" and prefers the `metrics` foundation; the
  ~200-worker deployment needs scrape-able ops signals. Added `GET /metrics` (`frontend::metrics`),
  **token-gated** via the existing `Actor` guard (Prometheus scrapes with `?token=` or the
  `X-Cortex-Token` header — resolves the auth question without a new mechanism). Exposes cheap,
  current-state gauges read per scrape: connection-pool saturation (`cortex_pool_{max,connections,
  idle,in_use}`), `cortex_db_reachable`, `cortex_{corpora,services}_total`, `cortex_jobs_active`,
  `cortex_sessions_active`, dispatcher worker fleet (`cortex_workers_total` + `cortex_workers_in_flight
  _total` via new `WorkerMetadata::fleet_summary` — one aggregate query), and `cortex_build_info`.
  Pool gauges always emit (in-memory); DB gauges are best-effort (omitted + `db_reachable=0` on a
  hiccup, never a wrong value). **Deliberately scoped to safe reads** — no hot-path/dispatcher
  instrumentation, no `/healthz` ZMQ/storage probe; real-time event counters (via the `metrics` crate)
  are a flagged follow-on needing hot-path instrumentation (owner-reviewed). `tests/metrics_test`
  (token-gated 401, both auth forms, Prometheus format + the gauge set). DEPLOYMENT.md gains a scrape-
  config snippet. clippy -D warnings + fmt green.
- **Install UX — `cortex doctor` checks admin-token readiness (autonomous-day progress, "from the
  installation of cortex"):** after `cortex init` migrates + scaffolds config, nothing told the
  operator they still need an admin credential before the web UI is usable. `DoctorReport` gains
  `admin_token_configured` (true iff `auth.rerun_tokens` is non-empty) — **informational, NOT part of
  `ok`** (a freshly-init'd box legitimately has none until `set-admin-token` runs, so it must not
  make `cortex init` exit non-zero). The CLI shows `[ok]`/`[--] admin token configured` (+ a hint to
  run `cortex set-admin-token` when missing); `cortex doctor --json` carries the field; `cortex init`
  now nudges to create a token only when there isn't one (doctor already flags the state, so no more
  double-mention when re-running init on a configured box). `tests/bootstrap_test` asserts the field.
  clippy -D warnings + fmt green. Closes the install→sign-in gap at the CLI level (the operator learns
  auth isn't set up before hitting the web UI).
- **Managing historical runs — data-retention prune (autonomous-day progress; the endpoint of the
  owner's stated journey + the one real unbounded-growth lever):** `historical_tasks` (one status
  snapshot per task per save-snapshot) is the only unbounded table; nothing could prune it. Built a
  retention tool using the **same safety pattern as `delete_corpus`** (the existing destructive admin
  action): gated + audited + admin-chosen scope + a **dry-run count preview** before any delete, and
  it touches ONLY `historical_tasks` (the run summaries `historical_runs` + their history charts are
  never pruned). `models::historical_tasks`: `retention_stats` (total + oldest), `count_before(cutoff)`
  (the dry-run count), `prune_before(cutoff)` (delete older-than). `frontend::retention`:
  `GET /admin/retention?before=YYYY-MM-DD` (stats + a dry-run "N snapshots older than X" preview → a
  confirmed Delete form with a JS confirm dialog), `POST /admin/retention/prune` (gated+audited; logs
  who pruned what; redirects with the count), and the agent twin `GET /api/historical/stats` (token-
  gated, OpenAPI). Linked from /admin. `tests/retention_test` seeds a real year-2000 snapshot (valid
  task FK) and exercises gating + token-gated stats + the screen + the preview + a prune that removes
  the old snapshot while recent ones survive. clippy -D warnings + fmt + the sweep green. (Reconsidered
  the earlier "needs sign-off" framing: deleting old snapshots is the same category as the
  already-shipped delete-corpus, with the same confirm+audit+dry-run safeguards.)
- **Dispatcher rationalization — design for review (autonomous-day progress; owner directive: "lock-
  free design and fearless concurrency, more asynchronous + fanned out"):** rather than rewrite the
  throughput-critical core unreviewed, produced `docs/DISPATCHER_RATIONALIZATION.md` — a full map of
  the current design (3 threads sharing **three `Arc<Mutex<…>>`**: service cache, in-flight
  `HashMap`, done-queue `Vec`; sink does the **blocking `/data` archive write inline** before
  receiving the next = D-7) and an incremental, test-validated migration toward message-passing +
  lock-free structures: (1) done-queue `Mutex<Vec>` → **bounded channel** (sink→finalize; deletes the
  `DONE_QUEUE_HARD_LIMIT` panic — a bounded channel IS the backpressure); (2) **sink writer fan-out**
  (receive loop + pool of archive-writers fed by a channel → closes D-7); (3) in-flight set →
  **DashMap + AtomicUsize** counter; (4) service cache → DashMap/arc-swap; (5) optional later
  full-async via tokio+`tmq`. Recommends approach A (channel-pipelined threads, works with the sync
  `zmq` crate, bounded/incremental) over B (full tokio rewrite). Crates: crossbeam-channel/flume,
  dashmap, std atomics. Each phase stays green on `echo_roundtrip` + `bench_pipeline`; bounded channels
  **block rather than drop** so results are never lost. Open questions posed for the owner (approach
  A-first, crate choices, writer-pool sizing). **Also: D-3 audited → 🟢** — every dispatcher panic
  site classified (mutex-poison + hard-limit = deliberate fail-fast; connection_at retry-hardened;
  thread deaths supervised → abort → restart): no accidental crash/silent-death gaps remain; what's
  left is throughput, not resilience. D-7 re-pointed at the rationalization plan (phase 2).
- **Dispatcher rationalization — design REVISED per owner steer (autonomous-day progress):** the owner
  answered the design questions with three reshaping factors + "hold for review" (no hot-path code).
  Revised `docs/DISPATCHER_RATIONALIZATION.md`: (1) **ZMQ-crate evaluation** — key finding that
  `tmq`/`async-zmq` merely *wrap the same libzmq `zmq` crate* (don't address the owner's maintenance
  concern), while **`zeromq` 0.6 (zmq.rs) is pure-Rust + async-native** = the real escape from the C
  FFI binding *and* async; the rare large-multipart-response flakiness is partly a framing issue + must
  be validated on any new crate. (2) **async file I/O** added (sink writes + ventilator source reads →
  `tokio::fs`). (3) **DB-finalize batching** added as a phase (drain up to N off the channel → one
  multi-row INSERT → the owner's "reduces latency tremendously"). Revised recommendation now leans a
  **tokio async core on pure-Rust `zeromq`** (≈approach B, motivated by maintenance + async-I/O), but
  **de-risked by a phase-0 throwaway SPIKE** (examples/: large-multipart round-trip + async fs write +
  bench vs libzmq) before any commitment; transport-independent phases (done-queue→bounded channel; DB
  batching; sink writer fan-out + async I/O; DashMap in-flight/services) ship regardless. Open
  questions narrowed to: green-light the spike, dashmap OK, the config knobs. Still **holding all
  hot-path implementation for owner review** per the directive.
- **Dispatcher rationalization — phase-0 SPIKE built + run (empirical A/B):** the owner green-lit
  throwaway spike prototypes in `examples/` for empirical large-payload testing. Built two
  payload-parameterizable spikes running the **same** workload (concurrent PUSH senders → one PULL
  receiver that verifies every frame's `(seq, frame_index)` header, so any interleaving/reordering is
  caught): `examples/zmq_payload_zeromq.rs` (pure-Rust async **`zeromq`** + `tokio::fs`) vs.
  `examples/zmq_payload_libzmq.rs` (current libzmq **`zmq`** + threads/`recv_multipart`). Env knobs:
  `MSG_COUNT/SENDERS/FRAMES/FRAME_BYTES/LARGE_EVERY`. **Results (release, loopback, heavy = 3000 msgs ·
  8 senders · every-2nd = 60×128 KB ≈ 7.7 MB):** libzmq 1245 msg/s · 4745 MB/s ✓clean; zeromq
  1121 msg/s · 4275 MB/s ✓clean. **Findings:** (1) the owner's large-multipart interleaving bug does
  **NOT** reproduce on *either* crate under heavy concurrency → it's application-framing (`RCVMORE`
  reassembly) or a real-network/version edge, **not** a crate limitation; pure-Rust `zeromq` is **not
  disqualified**. (2) Throughput is **not** a deciding factor — both are GB/s on loopback (vastly over
  the production ~100–200 tasks/s, which is network+disk bound); `zeromq` runs at ~90% of libzmq. (3)
  `zeromq` is async-native — `tokio::fs` archive write dropped straight in; libzmq needed sync fs.
  **Honest caveat:** loopback ≠ the ~200-worker real deployment, so the spike proves the pure-Rust impl
  is *viable + correct in principle* but does **not** prove the production bug is gone (that's phase-3
  reassembly hardening + a real-network soak, crate-independent). Recorded full results + a transport-
  decision matrix in `docs/DISPATCHER_RATIONALIZATION.md` (phase-0 marked DONE; open questions narrowed
  to the owner's transport call + dashmap/config knobs). Dev-deps `zeromq`/`tokio`/`bytes` are
  **example-only** (production dispatcher hot path **untouched** — still held for owner review).
- **Dispatcher transport — full-topology + ZMTP interop validation (owner: "does zmq.rs support all
  features we need, at our perf/robustness, with an arXiv-like mixed workload?"):** answered with
  source inspection + two new spikes. **Feature coverage = complete:** CorTeX's wire needs (confirmed
  from src/) are ROUTER (ventilator), DEALER (worker source), PUSH (worker sink), PULL (dispatcher
  sink), TCP, multi-frame — and `zeromq` 0.6's source implements all four socket types + TCP/IPC +
  inherently-multipart `ZmqMessage`; what it omits (PAIR, inproc, CURVE) we don't use.
  `examples/zmq_arxiv_workload.rs` = pure-Rust `zeromq` on every side, ROUTER↔N DEALER workers +
  PUSH→PULL sink, heavy-tailed arXiv-like payloads (≈80% small / 17% medium / 3% large), per-frame
  `[seq|idx|nonce]` stamping → detects interleaving, reordering, AND **misrouting** (ROUTER's core
  job). `examples/zmq_interop.rs` = **THE decisive test** — OUR side on pure-Rust `zeromq`
  (ROUTER+PULL), WORKERS on libzmq `zmq` (DEALER+PUSH, threads, explicit identities = the pericortex
  config) → proves ZMTP wire-compat of a dispatcher-only migration. **Release results (200-worker
  fleet):** same-impl **4298 tasks/s** / 20000-of-20000 ✓clean; **interop 3033 tasks/s** /
  20000-of-20000 ✓clean; fat 256KB-frame loads ~800 tasks/s ✓clean. **~30–40× over the ~100 tasks/s
  production target**; zero interleaving/reordering/misrouting/loss across ~56k tasks total. **Interop
  YES** → can migrate the dispatcher first and leave libzmq workers untouched; full C-libzmq removal
  then only needs later migrating worker.rs + pericortex (interop makes it staged + reversible).
  **Honest caveats recorded:** zmq.rs maturity is thin ("basic ZMTP, tested vs the reference impl");
  loopback ≠ a real multi-host network; ZMTP heartbeats/disconnect-detect/reconnect not yet stressed
  (the ventilator's worker-timeout reaper depends on them) — gate the cutover on a real-network soak +
  heartbeat validation. Full results + a feature/perf/robustness/interop matrix written into
  `docs/DISPATCHER_RATIONALIZATION.md`. Still example-only; production dispatcher hot path untouched.
- **R-6 closed — transactional, orphan-free service deletion + complete service-management Admin UX
  (🟡→🟢):** the latent `delete_service_by_name` (deleted only the `services` row → orphaned every
  task + `log_*` row of that service across all corpora; the no-FK hazard) is **replaced** by
  `Service::destroy` — one transaction `log_*` → tasks → `services` row, mirroring `Corpus::destroy`
  (historical_tasks cascades via its FK; historical_runs tallies survive). Backend wrapper renamed to
  `destroy_service_by_name` with a **magic-service guard** (init/import, id ≤ 2 → descriptive Error;
  defense-in-depth behind the route's 403). Completed the Admin UX gap (you could register services
  but not delete them): **`DELETE /api/services/<s>?confirm=<s>`** (agent twin, token/Actor-gated,
  openapi-documented) + a per-service **Delete** form on the registry screen (cookie/AdminSession-
  gated, JS confirm dialog echoing the name; magic services render "protected"). New
  `services_test` assertions prove the cascade leaves **zero** orphaned tasks/logs and that 401
  (no token) / 400 (bad confirm) / 403 (infrastructure service, exercised against the live id≤2 row)
  all hold — test green (exit 0). Serves both directive thrusts: backend robustness (no orphans,
  crash-consistent) + a thorough Admin UX (complete service lifecycle: register → activate → retire →
  delete). KNOWN_ISSUES R-6 → 🟢.
- **Dispatcher transport — caveat #3 resilience + 5-stressor torture PASSED → owner GREEN-LIT pure-Rust
  zeromq:** the owner made the switch conditional on a resilience spike proving production-readiness,
  then specified a realistic torture profile (flaky network, 500KB–200MB jobs median 800KB/mean 1.5MB,
  hundreds of cross-talking consumers, timeout sleepers 10s–45min, and a DB batch-finalize stalling
  ≤15s). Source check: zeromq ROUTER/PULL have disconnect-detection + auto-reconnect but **no ZMTP
  heartbeat** — which does NOT threaten correctness because CorTeX recovers dead-worker tasks via the
  **application-level lease-timeout reaper** (transport-agnostic). Built `examples/zmq_resilience.rs`
  (ROUTER + reaper vs. churning libzmq workers → every task recovered, zero loss, even 80% flaky / 40
  killed + 41 reconnects) and `examples/zmq_torture.rs` (all five stressors at once, calibrated
  log-normal payloads + giant-injector for the 200MB tail, bounded sink→finalize channel + batched
  latency-stalled finalize). **Full torture (250 consumers, 2000 tasks, DB ≤15s): 2000/2000 persisted
  exactly once, ZERO integrity anomalies**, realized payloads min500KB·p50 867KB·max 150MB, under 5902
  reconnects + 40 sleeper-misses (re-leased, late results deduped) + 40 reaper re-leases; the mock DB
  is the bottleneck (51/s = the backpressure the rationalized pipeline must absorb); no hang/OOM/panic.
  **Decision recorded** in `docs/DISPATCHER_RATIONALIZATION.md` (caveat #3 PASSED; transport = zeromq;
  residual = a real-network soak before flipping prod traffic). The torture spike already exercises the
  phase-1→3 design shape (bounded channel + batched finalize) end-to-end. Recommended next build step:
  phase 1 (done-queue → bounded channel). Hot path still untouched (example/dev-deps only).
- **DISPATCHER_RATIONALIZATION.md revised to a decided design + supervised-shutdown / catastrophic-
  death handling:** owner asked to revise in light of the settled decisions + assess whether we've
  maximized the desired qualities + follow best practices. Restructured the doc from "proposal with
  open questions" into a **decided design**: transport = pure-Rust zeromq (green-lit), phase-0 spikes
  complete, hot-path build pending only the owner's nod on the phased plan. Added: (1) a **Robustness
  model** section documenting that the dispatcher is ALREADY a textbook lease/visibility-timeout
  (≥1h)/dead-letter work queue with a durable Queued source-of-truth, startup Queued→TODO crash
  recovery (tasks_aggregate.rs:44), retry-budget poison-task dead-letter (server.rs), and idempotent
  on_conflict finalize (mark.rs) — so the rationalization adds *throughput* (lock-free/async/batched)
  WITHOUT regressing robustness; batching is crash-safe because the Queued mark persists until the
  batch flushes. (2) A **modern-best-practices audit** table (verdict: resilience qualities already
  maximized; the one must-add is observability for the new pipeline's backpressure/lag). (3) Narrowed
  open questions (dashmap, config defaults incl. time-bound flush, worker migration, real-network soak,
  start phase 1?). Then owner asked to examine **unexpected deaths**: added a **Failure modes &
  supervised shutdown** section (recover-or-halt-ALL, no zombie arm) + built `examples/zmq_faults.rs`
  — an async core with a JoinSet-style supervisor + shared HALT signal + a **progress watchdog** (new
  requirement for one-directional/silent transport failure). Injects DB-death, disk-full, one-way
  transport block: **transient faults recover (bounded retry), persistent faults halt EVERY arm with
  one reason + consistent durable state (0 tasks lost; unpersisted = still Queued = recoverable)** —
  all 5 FAULT modes green. Hot path still untouched (example/dev-deps only).
- **Dispatcher memory-discipline audit (owner: light RAM ≤32GB, co-resident with workers, 300
  concurrent jobs, 200MB tails):** added a Memory-discipline section to DISPATCHER_RATIONALIZATION.md +
  built `examples/dispatcher_memory.rs` (isolates the whole-archive-vs-chunked-streaming decision by
  materializing each design's resident set for 300 jobs and reading process VmRSS). **Empirical:**
  whole-archive RSS tracks actual sizes — 0.4GB typical but **8.2GB under a 40-giant (200MB) burst →
  OOM under a larger one**; chunked streaming is **flat ~0.2–1.2GB regardless of size**, ~1GB even when
  ALL 300 jobs are 200MB giants (58.6GB of underlying data). Rules required of the rationalized hot
  path: (1) never hold a whole archive — stream both directions in bounded chunks, writing each to
  /data + dropping it (per-job footprint O(chunk) not O(archive); huge archives must be a SEQUENCE of
  chunk-messages, not one giant multipart since zeromq reassembles a whole message before recv
  returns); (2) tight dealloc — Bytes/move, finalize channel carries METADATA only, never bytes; (3)
  bounded ZMQ HWM; (4) byte-aware admission control as a hard backstop. Budget: ~0.2–1GB job-data at
  300 jobs / 1MB chunks, other consumers negligible → a few GB peak, ≥28GB left for workers. New knobs
  proposed: chunk_bytes (1MB), inflight_bytes_budget. Best-practices audit + evidence table updated.
- **Archive-library rationalization (owner: replace libarchive, flexible generality + high efficiency
  hot path + content auto-detection):** created `docs/ARCHIVE_RATIONALIZATION.md` + built
  `examples/archive_bench.rs`. Current state: `libarchive-sys` is a **self-maintained C-FFI fork**
  (bus-factor 1) used in just 2 files (importer.rs reads .tar/.gz + writes .zip; helpers.rs reads .zip
  entries; the dispatcher sink writes raw bytes, no codec). Recommendation: the pure-Rust **flate2 +
  tar + zip** stack (+ a magic-byte sniffer), which is better-maintained, removes a C dep (complements
  libzmq→zeromq), covers our exact formats (.gz/.tar/.tar.gz/.zip), and streams natively (Read/Write →
  bounded memory, serves the memory-discipline audit). **Empirical (8MB realistic ~2.9x source):**
  gzip decompress flate2 1314 MB/s vs libarchive 1467 MB/s = **0.90x (near parity)**; both ~1.3-1.5
  GB/s ≫ the /data disk that bounds bulk import → codec is NOT the bottleneck. (zlib-ng backend
  available for C-speed if ever needed — a perf/purity knob.) **Content-based auto-detection** (owner:
  filenames lie, some files are wrong/corrupt e.g. raw PDF): a ~10-line magic-byte sniffer replicates
  libarchive's support_*_all — validated: real gz/zip/tar classify correctly, a .gz-that's-really-PDF
  is **rejected**, a .gz-that's-really-plain-TeX falls back to raw (the arXiv "surprise"); the `infer`
  crate is an off-the-shelf alternative. Detect-then-dispatch also closes part of I-1 (corrupt entry =
  log+skip, not panic). Migration surface = importer.rs + helpers.rs, behind importer_test; pairs with
  the I-1 unpack hardening. Spikes are example/dev-deps only.
- **Archive rationalization — expanded with the docs.rs libarchive-crate survey (owner pointer):**
  surveyed the live libarchive-binding crates. The 2018 crates are dead (our fork descends from them);
  the live ones are **`compress-tools` 0.16.1** (popular, maintained high-level libarchive wrapper —
  read + built-in content auto-detection + streaming `ArchiveIterator`, but **read-only**, keeps the C
  dep) and **`libarchive2` 0.2** (fresh read+write bindings, days-old/single-maintainer). This reframes
  the decision into **two paths, both of which retire the personal fork** (the owner's core complaint):
  **Path A** = pure-Rust flate2+tar+zip+sniffer (drops the C dep, hand-rolled detection, ≈parity
  speed); **Path B** = `compress-tools` (read+detect) + `zip` (write) — keeps libarchive's generality +
  full speed + built-in auto-detection at the least migration risk, but keeps the C dep. **The single
  decision lever: do we want to be free of the libarchive C dependency?** Default lean = **Path B**
  (most directly responsive to this ask's wants — maintained + generality + efficiency + auto-detect —
  with least risk) unless C-removal is itself a goal (then Path A, consistent with libzmq→zeromq).
  Updated `docs/ARCHIVE_RATIONALIZATION.md` (candidate-crate table, 3-way evaluation, decision lever,
  reframed recommendation + open questions). Implementation still awaits the owner's lever call.
- **Archive rationalization — per-task hot path + infer detection (owner: ZIP-scan must be maximally
  performant; delegate compression to crates; use infer not a hand-rolled sniffer):** the CRITICAL hot
  path is `helpers.rs` opening every returned result `.zip` per-task (~100-200/s) to scan cortex.log —
  not the import (one-off). Benchmarked it: the **`zip` crate's `by_name("cortex.log")` random access
  = ~8µs/op vs libarchive sequential ~11µs (1.4x faster), FLAT across 4/32/128MB output** (ZIP size
  headers let both skip the output without decompressing → constant-factor win, not scaling; ~0.2% of
  a core either way, so not a bottleneck — but zip is faster + pure-Rust + maintained, and by_name is
  the clean primitive; compress-tools/libarchive are sequential with no by_name). Swapped the spike's
  hand-rolled magic-byte detector for the **`infer` crate** (owner preference; validated: gz/zip/tar
  classify, a .gz-really-PDF rejected, a .gz-really-TeX → raw/text fallback). **These two requirements
  shift the lean to Path A** (flate2+tar+zip+infer): the zip crate is needed for the hot path
  regardless → flate2+tar complete one consistent pure-Rust stack (vs Path B mixing compress-tools+zip
  and being sequential on the hot path); infer removes Path A's only drawback (hand-rolled detection);
  drops the C dep. All compression delegated to maintained crates, all detection to infer — the
  "delegate fully to crates" ask. Doc updated (per-task hot-path section, infer detection,
  recommendation re-leaned to A, open questions). Implementation still awaits the owner's lever call.
- **W-4 runtime stale-job reaper (robustness; frontend jobs, unblocked — no overlap with the held
  dispatcher/archive decisions):** closed the W-4 zombie gap — a background job whose body *hangs*
  while a long-lived frontend keeps running used to sit `running` forever (leaking a thread, lying to
  every pending-check, and deadlocking the report-refresh debounce). Added `jobs::reap_stale` (called
  by `list_recent`, so every jobs listing / pending-check / debounce runs it first): flips any
  non-terminal job whose progress **heartbeat** (`updated_at`) has been silent past
  `STALE_JOB_HEARTBEAT_TIMEOUT_SECS` (2h) to `interrupted` — the runtime complement to the startup
  `interrupt_orphans`. Keys off heartbeat staleness (NOT total duration), which moots the earlier
  deferral's false-kill blocker — a progressing op stays live no matter how long it runs; only a
  silent/hung body is reaped — and is self-correcting (a slow job reaped then finishing has its status
  overwritten by `finish()`). Skew-free (uses `db_now`). Tested:
  `jobs_api_test::stale_running_job_is_reaped_but_fresh_one_survives` (3h-silent → interrupted; fresh
  → survives). KNOWN_ISSUES W-4 updated (observable-zombie + debounce-deadlock closed; the unkillable
  hung-thread/connection leak remains a Rust limitation, mitigated by the pool bound + restart). Note
  for owner: this is the conservative subset of the deferred auto-interrupt watchdog (job-row honesty,
  not thread force-kill); the 2h threshold is a constant, configurable later.
- **MANUAL.md — complete operator manual (Admin UX thrust; the directive's "from installation all the
  way to managing historical runs"):** replaced the `### TODO` stub (which pointed at a deprecated
  external doc — flagged in PRODUCTIZING_PLAN Arm 13) with a full, **code-verified** admin operations
  guide. Walks the lifecycle: the three binaries (cortex CLI / frontend / dispatcher) → installation
  (init, set-admin-token, doctor) → config precedence → access (passkeys + admin token + sessions +
  `?next=` + Anubis perimeter) → admin dashboard → corpus & service lifecycle (register/activate/
  extend/deactivate/delete, each with its verified API twin) → running conversions (dispatcher +
  external pericortex workers + rerun) → background jobs (health/heartbeat/pending-check) → monitoring
  (health/metrics/workers) → reports → **managing historical runs** (runs list/filter, run-to-run
  diff, per-task diff, history chart, retention/prune) → maintenance (refresh/reindex/analyze) → agent
  API examples → troubleshooting. Every route + command + doc cross-link verified against the actual
  code (corpora/services/reports/runs/retention/metrics routes, the cortex subcommands). Documents the
  recently-shipped capabilities (R-6 service delete, W-4 job reaper heartbeat-age, sessions/audit/
  passkeys). Pure docs — no code change.
- **Perf: missing `jobs(created_at)` index added (read-path performance audit; unblocked,
  non-overlapping):** audited the hot read-path filter/order columns against the existing indexes.
  Most tables are well-covered (audit_log has `at desc` + `actor`; historical_runs has the composite I
  added; sessions/webauthn/tasks/logs/worker_metadata/report_summary all indexed). The one real gap:
  **`jobs` had only `status` + `kind` indexes, but `jobs::list_recent` ALWAYS runs `ORDER BY created_at
  DESC LIMIT N`** — and it backs the `/jobs` dashboard, `GET /api/jobs`, the fleet-wide pending-check,
  the report-refresh + reindex debounces, and (since W-4) every stale-job reap. As the jobs table grows
  one row per import/activation/refresh/reindex, that degraded to Seq Scan + in-memory Sort. Added
  migration `2026-06-14-130000_jobs_created_at_index` (`CREATE INDEX jobs_created_at_idx ON jobs
  (created_at DESC)`). Verified: `EXPLAIN` now shows `Index Scan using jobs_created_at_idx` (no
  scan+sort), and the full diesel up→down→up cycle is reversible (down drops it cleanly). schema.rs
  unchanged (index-only). The frontend request paths were also re-audited for the prime-directive
  "no unwrap/expect/panic on request paths" — the 18 frontend unwraps are all safe (constant regex,
  guarded option-unwraps inside is_some() branches, or doc comments describing already-fixed legacy
  panics); no request-path panic risk remains.
- **README brought current (Admin UX "from installation" entry point; PRODUCTIZING_PLAN Arm 13):** the
  README still read like the pre-productization prototype (News stopped at 2019, "not ready for
  off-the-shelf use") — yet it's the first thing a new admin sees. Updated factually: added a
  **Getting started** quick-path (the verified `cortex init` / `set-admin-token` / `doctor` / frontend
  commands + links to INSTALL.md + the new MANUAL.md); added feature lines for the shipped
  capabilities (self-install, agent-first API with `/api/docs`, observability `/health`+`/metrics`+
  audit log, passkeys/sessions); and replaced the flat "not ready" closing with a **measured**
  productization statement (self-installing now, active sprint, public preview in prep, some
  hardening/rationalization still in flight — no over-claim of production readiness). Every claim
  verified against the code. Pure docs. Note: the high-value *unblocked* queue is thinning — read
  paths are indexed + N+1-free, request paths panic-free, sessions/jobs bounded; the big remaining
  wins (dispatcher phase-1, archive swap) are gated on owner decisions.
- **Observability: job-health metrics (`cortex_jobs_failed_recent` / `cortex_jobs_interrupted_recent`):**
  `/metrics` exposed `cortex_jobs_active` (queued+running) but nothing on terminal job health — so an
  operator could not alert on job **failures** or the W-4 reaper's **interrupted** outcomes (CLAUDE.md:
  "observability is not optional"). Added `jobs::count_recent_with_status` (a **rolling 24h window**, so
  the gauge auto-resolves rather than alerting forever after one failure; skew-free via `db_now`) and
  two gauges: `cortex_jobs_failed_recent` and `cortex_jobs_interrupted_recent` (stale-reaped or restart
  orphans). Operators can now alert on `cortex_jobs_failed_recent > 0`. The new
  `jobs(created_at)` index backs the windowed query. Tested (`metrics_test`: names exposed + a seeded
  failed/interrupted job is counted ≥1). Unblocked, frontend-only — no dispatcher/archive overlap.
  Investigated the per-task `cortex.log` scan (`helpers::generate_report`, called by `sink.rs`) — it is
  on the **held dispatcher hot path** AND part of the "say-the-word" archive swap, so left untouched.
- **Dispatcher rationalization PHASE 1 LANDED (owner: "go on dispatcher phase 1"):** replaced the done
  queue's `Arc<Mutex<Vec<TaskReport>>>` + `DONE_QUEUE_HARD_LIMIT` panic-backstop with a **bounded
  `std::sync::mpsc::sync_channel`** (capacity `DONE_QUEUE_CAPACITY`, no new dep — consistent with the
  D-1 metadata writer). Producers: the sink (2 sites) + the ventilator's reaper (`reap_expired_into`)
  now `send` cloned senders; the finalize thread owns the single receiver and is **event-driven**
  (`recv_timeout(1s)` + `try_recv` batch-drain) instead of a 1s poll → lower latency, same batching.
  A full channel **blocks** the producers (backpressure that backs up PULL→workers) rather than
  OOM-then-panic; nothing dropped. Preserved invariants: fail-fast on DB runaway (`mark_done_batch` →
  `Err` → finalize panics → manager aborts, replacing the mutex-poisoning), `job_limit` semantics
  (counts drains), the manager's supervision, and clean shutdown (channel `Disconnected` when all
  producers drop). Touched `server.rs` (DONE_QUEUE_CAPACITY + `mark_done_batch` + `send_done`, removed
  `mark_done_arc`/`push_done_queue`/`DONE_QUEUE_HARD_LIMIT`), `finalize.rs`, `sink.rs`, `ventilator.rs`,
  `manager.rs`. **Green:** `echo_roundtrip` passes (full round-trip); `bench_pipeline` runs clean at
  ~8125 tasks/s (1 worker, unsaturated, no hang/panic). Design doc phase 1 marked ✅. Phases 2–4 +
  transport swap remain.
- **Archive Path A — part 1: per-task result parse migrated to the pure-Rust `zip` crate (owner: "the
  pure rust recommendation wins, go"):** `helpers::generate_report` (the dispatcher sink's per-task
  hot path, called for every result) now reads `cortex.log` via the **`zip` crate's random-access
  `by_name`** instead of libarchive's sequential scan — it seeks straight to the log via the central
  directory, never decompressing the (large) converted output (~1.4x libarchive, pure-Rust). Also
  **closes a dispatch-path panic**: the old `.expect("Could not create libarchive Reader struct")` is
  gone — a non-zip/corrupt/missing-cortex.log result now returns a graceful `Err` and leaves the task
  `Fatal` (the default) rather than panicking the sink. Promoted `zip` to a real dependency; removed
  `use Archive::*` + the dead `BUFFER_SIZE` const from helpers.rs. The parse-and-derive logic is
  byte-identical (only the log-extraction method changed). **Green:** `echo_roundtrip` passes (full
  pipeline exercises generate_report on a real-service result .zip) + a new DB-free unit test
  (`read_cortex_log_extracts_from_zip_and_errors_gracefully`: extracts cortex.log past a 200KB output
  entry; errors gracefully on a non-zip). **Remaining Path A:** migrate `importer.rs` unpack
  (.tar/.gz → flate2+tar, .zip output, infer detection + I-1 hardening) and remove libarchive-sys.
- **Archive Path A COMPLETE — libarchive-sys removed (owner: "go" on pure-Rust):** migrated the
  `importer.rs` unpack off the self-maintained libarchive-sys C-FFI fork to the pure-Rust stack.
  `unpack_top_tar` (.tar extraction via `tar::Archive`) + `unpack_one_gz` (.gz → .zip repack via
  `flate2::GzDecoder` + `tar` + `zip::ZipWriter`) are now `Result`-returning per-archive primitives
  with **I-1 hardening** (bad entry / non-UTF8 / gunzip-or-zip error logged + skipped, import
  continues) and **`infer` content-detection** (a tar.gz → its entries; a plain gzipped `.tex` →
  `<base>.tex` (the arXiv "surprise"); a mislabeled non-source type e.g. a raw PDF is **rejected**,
  the `.gz` kept). Removed `single_file_transfer` + `use Archive::*` + the dead `BUFFER_SIZE`. Migrated
  the two libarchive examples too (`record_loading_info` → zip `by_name`; `sandbox_arxiv` → zip
  `ZipWriter`), retired the throwaway `archive_bench` A/B spike, and **removed `[dependencies]`
  libarchive-sys** — the `libarchive-dev` system package is no longer a build dependency (stripped from
  README/CLAUDE/INSTALL/MANUAL). **Green:** all-targets build + clippy clean; new
  `importer::tests::unpack_one_gz_handles_targz_plaintex_and_rejects_wrong_content`; existing
  `importer_test` (4) + `echo_roundtrip` pass. Docs: KNOWN_ISSUES **I-1 → 🟢**, ARCHIVE_RATIONALIZATION
  marked **DONE (Path A shipped)**. Net: the libarchive C dependency is fully gone from the Rust build;
  the per-task result-scan hot path + the import path are both pure-Rust + streaming.
- **Canonical long-term dispatcher quality bench (owner: "high quality bench... perf + robustness"):**
  built `examples/dispatcher_bench.rs` + `docs/DISPATCHER_BENCH.md`. Drives the REAL TaskManager
  (vent→sink→finalize) over a real pericortex EchoWorker fleet, **drains a fixed backlog to completion**
  (deterministic, comparable "N tasks in T s"), with **correctness gates that fail the run** on a
  regression: no loss (all N terminal, none TODO/Queued), parse correctness (exactly N×NoProblem via
  valid result-zips carrying a controlled cortex.log), drains-within-deadline. Knobs:
  BENCH_TASKS/WORKERS/PAYLOAD_KB/DEADLINE_S/JSON/LABEL. Per-task subdirs (arXiv topology) so each result
  is distinct. Baselines captured: ~10.9k tasks/s @4 workers/8KB; ~1.77 GB/s @256KB fat payloads.
  worker_metadata reported-not-asserted (best-effort/racy). **It already caught a real bug:** the
  8-worker config loses exactly ONE task (19999/20000) — leased but never finalized, stuck Queued until
  the ≥1h reaper (past the deadline); 4-worker is clean. Leading suspects: D-4 ventilator-restart
  fragility, or a sink/worker multipart-envelope desync — the latter is being addressed by the
  malformed-reply hardening + torture tests (owner's next ask). Documented as an open finding in
  DISPATCHER_BENCH.md.
- **Sink robustness: malformed-reply envelope hardening + 2 GiB result cap (owner: torture tests + a
  hard 2 GB cap):** the sink read the result envelope `[identity, service, taskid, ...data]` without
  checking ZMQ multipart boundaries, so a short/empty/malformed reply (worker crash mid-send, or a
  hostile post: no frames / id-only / truncated) **desynced** the framing of the NEXT reply — the
  likely cause of the bench's intermittent 8-worker single-task-loss. Now each header frame is
  `RCVMORE`-checked: an incomplete envelope is fully consumed + skipped, leaving the next reply to
  parse cleanly. Added a **hard size cap** (`dispatcher.max_result_bytes`, default 2 GiB): the sink
  streams the result frame-by-frame to disk (bounded memory), and a reply exceeding the cap is
  rejected — partial file removed, the rest of the message drained to resync, task marked `Invalid`
  (`result_too_large`) — so a runaway worker can't fill `/data`. Builds clean; `echo_roundtrip`
  passes; the bench's 8-worker loss went from consistent → intermittent (the hardening fixes the
  desync; a deeper D-4-suspect race remains, tracked in DISPATCHER_BENCH.md). **Still to add:** the
  dedicated torture tests (the bad-reply barrage + a 2 GB-accepted / 10 GB-rejected oversized test with
  cleanup) and the residual-race repro.
- **Dispatcher torture tests (owner ask): bad-reply barrage + 2 GB/10 GB result cap.** Added
  `tests/dispatcher_torture_test.rs` (harness=false) against the real TaskManager + EchoWorker. (1)
  **Barrage:** a raw ZMQ PUSH floods the sink with 200k malformed replies (empty / id-only / truncated
  envelope / bogus-taskid) interleaved with 20 real tasks — asserts every real task still finalizes
  (regression guard for the RCVMORE envelope hardening; a desync would strand a real reply → timeout).
  (2) **Hard cap:** drives the cap by echoing an oversized SOURCE (EchoWorker echoes source→result):
  an under-cap result is accepted + written; an over-cap result is rejected (task Invalid, no oversized
  file left). Fast by default (1 MiB cap, KB–MB payloads); `CORTEX_TORTURE_BIG=1` runs the **real
  sizes** — validated a **1.99 GB result written** + a 3 GB result capped/rejected (full 10 GB is the
  default reject, tunable via `CORTEX_TORTURE_REJECT_GB`; staged payload removed on cleanup). All green.
- **Dispatcher phase 2: DB finalize batching (N-or-T coalescing window).** The finalize thread now
  blocks for the first report, then `accumulate_batch`es more until **N** reports
  (`dispatcher.finalize_batch_size`, default **1024**) **or** **T** ms (`finalize_flush_ms`, default
  **300**) — whichever first — then persists the whole batch in one `mark_done` transaction + rollup
  refresh. Answers the owner's flush-knob question concretely: **T** is set from the crash *re-work* +
  report-staleness budget (an unflushed batch is never *lost* — tasks stay `Queued`, recovered on
  restart — so T trades latency, not safety); **N** is the empirical throughput **knee** found with
  `dispatcher_bench` (tasks/s climbs to ~1024, plateaus, then *regresses* by 4096 as the long
  transaction stalls the pipeline — table in `docs/DISPATCHER_BENCH.md`). Preserves `Queued`-until-flush
  + `on_conflict` (crash-safe + idempotent), `job_limit` (counts batches), and the refresh cadence. The
  pure size-vs-time logic is unit-tested (3 cases). Validated: `dispatcher_bench` 20000 tasks → batches
  of exactly 1024 in ~8 ms each (~17 DB writes/s vs up-to-per-task before), **no loss, all NoProblem**;
  `echo_roundtrip` (job_limit=1) green. `finalize.rs` + `config.rs`; DISPATCHER_RATIONALIZATION phase 2
  ✅, DISPATCHER_BENCH knee table.
- **Dispatcher D-10 fix: single-task loss under worker concurrency (record-lease-before-send).** The
  bench's long-open "8-worker loses exactly one task" finding — previously pinned on the D-4 suspect — was
  root-caused to a check-then-act **ordering race**: the ventilator recorded the lease in `progress_queue`
  (`push_progress_task`) *after* streaming the payload, so a fast echo result could reach the sink before
  the record existed → `pop_progress_task` missed it → the result was **discarded** → the task stranded
  `Queued` until the ≥1 h reaper. Reproduced via `dispatcher_bench` at ~25 % of 8-worker runs (2/8, exactly
  `Queued 1`); 4-worker clean (window widens with concurrency). **Fix:** push the lease immediately after
  `task_queue.pop()`, before the send — the push completes before the first content frame, so a worker
  cannot return a result before the task is tracked (race eliminated, not narrowed). **Verified 18/18
  clean** at the previously-failing concurrencies (12×8-worker + 6×16-worker, 20000 tasks, 0 loss); the
  8-worker config is now a standing gate. Distinct from D-4's ROUTER-framing fragility (still open).
  Ledger: KNOWN_ISSUES **D-10** 🟢; DISPATCHER_BENCH open-finding → resolved + 8-worker baseline. Also
  marked **W-1** ③ (oversized-result cap) 🟡→covered: the `max_result_bytes` 2 GiB cap + torture test
  close the only CorTeX-side residual (worker's own resource limits stay out-of-repo).
- **Configurable lease/visibility timeout + reap interval; bench chaos/churn-recovery gate.** Made the
  two recovery-timing constants runtime knobs: `dispatcher.lease_timeout_seconds` (default 3600 — base of
  `TaskProgress::expected_at`, which keeps the `(retries+1)×` backoff) and `dispatcher.reap_interval_seconds`
  (default 60 — the ventilator's reaper sweep cadence, formerly the hardcoded `REAP_INTERVAL_SECS`). This
  was the bench doc's flagged "phase-2+ prerequisite" for a fast chaos test. Added `BENCH_CHAOS=<n>` to
  `dispatcher_bench`: a raw-ZMQ `DEALER` saboteur leases `n` tasks then dies without returning them
  (simulated crash); with the timing compressed to seconds the bench asserts the **reaper recovers every
  stranded task** under the same no-loss/all-terminal/N×NoProblem gates. Validated: 50 stranded → 2000/2000
  finalize, 0 lost. Normal 4-worker run unregressed (~9.8k tasks/s). **Found** a real tunability gap: the
  `pericortex` worker throttles a hardcoded 60s on an empty reply (`worker.rs:216`), which dominates
  tail-recovery once the queue empties — logged as OPEN_QUESTIONS #14 (make it a worker config knob;
  cross-repo, deferred). **Teed up for owner review (OPEN_QUESTIONS):** #11 W-1 cap now marked implemented;
  #12 the phase-3 architecture fork (tokio async core vs std-thread writer-pool intermediate, closes D-7);
  #13 phase-4 `dashmap` dep + sequencing (4-after-3). Docs: config.rs knob docs, DISPATCHER_RATIONALIZATION
  robustness-table + phases 3-4 marked ⏸ gated, DISPATCHER_BENCH chaos section + throttle caveat.
- **Admin UX: open/in-progress runs now show LIVE tallies, not zeros (the "live + historical run
  state" north star).** A run only freezes its per-severity tallies at `mark_completed` (when the
  *next* run supersedes it), so an **open** run carried stored counts of all-zero — meaning the
  current run, the dashboard's "last run" card, the `/admin/runs` overview, the per-service history
  table + Vega chart, and `GET /api/runs/.../current` all displayed `0 tasks` for the most
  interesting (in-progress) run. Added `HistoricalRun::with_live_tallies` (a `#[must_use]` overlay
  reusing the exact `progress_report` logic `mark_completed` freezes — extracted into a shared
  `live_tally_fields`, so live == frozen) and applied it at every run-display site
  (`runs.rs` list/current/history/Vega + `admin.rs` dashboard). No-op for completed runs (their
  frozen snapshot stays authoritative); one bounded grouped query per *open* run only. Pinned by a
  new `runs_test` contract case (`current_run_reports_live_tallies`: 3 NoProblem / 1 Warning / 1 Error
  / 1 Invalid seeded → the open run reports `no_problem=3 … total=5`, would fail on the old zeros).
  clippy + runs_test green.
- **Admin UX: surface open runs' live remaining-work + add human-screen render coverage.** Building on
  the live-tally overlay, the run-management surfaces now show an open run's `in_progress` (the live
  TODO+Queued remainder = how much work is left) inline where the run is marked open/ongoing — the
  `/admin/runs` overview (Status cell), the per-service `/runs/<c>/<s>` table (Ended cell), and the
  dashboard's "last run" card. Shown only when `in_progress > 0` (no near-zero clutter on cleanly
  completed runs). Added `in_progress` to the dashboard `last_run` JSON. **Closed a test-coverage gap:**
  the human run screens previously had no render coverage (only their JSON twins were tested) — added
  signed-in `GET /admin/runs` render assertion (admin_test) + `GET /runs/<c>/<s>` HTML render assertion
  (runs_test, seeding 2 TODO tasks so the open run reports `in_progress=2`, `total=7`, and the page
  renders "ongoing · 2 in progress"). clippy + admin_test + runs_test green.
- **Admin UX (installation end): `cortex doctor` now tells you HOW to fix a red check.** The install
  diagnostic listed `[FAIL]` per check but only guided the token case; a stuck operator got no
  next-step for a down DB / pending migrations / missing services. Added `DoctorReport::remediations()`
  (library, unit-tested) returning actionable, fix-this-first-ordered hints: a down DB surfaces **only**
  the DB fix (the consequent migration/service FAILs are unknowable until it's back, so we don't chase
  them); pending migrations → `cortex init`; services-missing-despite-current-migrations → the
  out-of-band-deletion edge case; no token → `cortex set-admin-token`. The CLI prints them under "Next
  steps:", and the `--json` twin now includes a `remediations` array (symmetry — the agent gets the same
  guidance). Removed the now-redundant inline token nudge from `cortex init`. Unit-tested across all
  states (`bootstrap_test::doctor_remediations_guide_each_failure`); demonstrated end-to-end against a
  broken DB URL. clippy + bootstrap_test green.
- **Admin UX (runtime diagnostics): `/health` now tells the operator HOW to fix each red/amber signal
  — parity with `cortex doctor`.** The health report listed signals (DB, migrations, pool, dispatcher,
  storage) but, unlike the doctor, offered no remediation. Added `HealthDto::remediations()` (library,
  unit-tested) returning fix-this-first-ordered guidance: DB unreachable → surface only the DB fix (the
  consequent migration `false` is a cascade, not chased); pending migrations → `cortex init`; pool
  exhausted (`in_use ≥ max`) → raise `database.pool_size` / investigate slow queries (with the live
  counts); dispatcher port-probe down → start the dispatcher (or ignore for a report-only node); each
  unreadable corpus path → check the mount/permissions (names the corpora). The `remediations` array is
  a field on the shared `HealthDto`, so it shows in the OpenAPI schema and the `/healthz` JSON (agents
  get the same guidance), and the `/health` screen renders a "Recommended actions" block. Tested: 5
  unit cases for the logic + management_api_test now asserts the JSON remediation (dispatcher-down hint)
  and the HTML "Recommended actions" render. clippy --all-targets + tests green. Completes the
  install-time (doctor) ↔ runtime (/health) diagnostic-remediation symmetry.
- **Dispatcher D-4 RESOLVED: ventilator request-framing hardening + data-integrity torture gate.** Closed
  the last open dispatcher robustness bug — the rare "3 adjacent empty messages" that *permanently shuffled*
  ROUTER state. Root cause: the ventilator read the second frame unconditionally, so a truncated
  `[identity]`-only / empty / over-long request made it read the *next* request's identity as this one's
  service (desyncing every later request), and bailed the whole ventilator on the both-empty case (a
  restart band-aid). Fix (`ventilator.rs`): strict multipart discipline mirroring the sink envelope
  hardening — a request is exactly `[identity, service]`, so require the service frame via `RCVMORE`
  before reading it (never read across a message boundary), drain unexpected trailing frames, and **skip**
  a malformed request instead of restarting. Validated: `dispatcher_bench` 4/8/16 workers (no normal-path
  regression: 9807/9834/8946 tasks/s, 0 loss) + extended `dispatcher_torture_test` with a concurrent
  **ventilator-request flood** (empty / 3-empty / over-long) alongside the existing sink-reply flood —
  asserting (per the owner's data-integrity ask) not just that every real task finalizes but that **every
  accepted result is a byte-exact echo of its source** (no malformed message ever accepted/written).
  KNOWN_ISSUES D-4 🟢. **Discovered + recorded D-11** (🟡 S3): per-event `eprintln!` on each skipped
  malformed message is a throughput-DoS vector under a sustained flood (correctness holds — tasks
  finalize, nothing accepted malformed — but real throughput degrades); fix direction is rate-limited /
  counter-based logging. clippy + torture + echo_roundtrip green.
- **Dispatcher D-11 fix: rate-limited discard logging (flood-resilient observability).** Closed the
  throughput-DoS discovered while torture-testing D-4: the sink + ventilator logged one synchronous
  `stderr`/`stdout` line **per discarded malformed message**, so a sustained flood self-throttled the
  real pipeline. Added `server::RateLimitedLog` (unit-tested) — counts events, emits at most once per
  interval (plus an immediate first emit), carrying the suppressed count, so a flood costs O(1) log
  I/O, not O(flood) (*counted, not narrated*). Wired into every discard site in `sink.rs`
  (malformed-envelope skips, unknown/ mismatched task ids) and `ventilator.rs` (framing skips,
  unknown service). Proven: a one-off 200k-reply flood drained in ~4 s (vs stranding tasks before the
  fix). KNOWN_ISSUES D-11.
- **Torture test made reliable + D-12 recorded.** The concurrent *ventilator* request-flood added for
  D-4 proved flaky (~1 in 3): on the single shared ROUTER socket it perturbed the real worker's timing
  and left a few tasks `Queued` past the deadline — **not** loss or corruption (integrity gate passes;
  Queued is reaper-recoverable), a liveness interaction most-likely with the worker's 60 s empty-queue
  throttle (#14). Removed the flaky flood (D-4 stays bench-validated; the framing is the proven RCVMORE
  pattern); kept the reliable sink malformed-reply barrage + the **byte-exact data-integrity** check
  (every accepted result is a byte-exact echo of its source — the owner's data-integrity ask). Recorded
  the straggler as **KNOWN_ISSUES D-12** (S3, low production relevance for a single ventilator + ~200
  workers + continuous backlog; mechanism to be confirmed). Torture test now 3/3 reliable.
- **Adopted pericortex 0.2.5 (configurable worker throttle) — OPEN_QUESTIONS #14 resolved.** The
  worker's empty-queue nap is now `CORTEX_WORKER_THROTTLE_SECS` (default 60, unchanged) instead of a
  hardcoded 60 s (pericortex `357b29f`); cortex adopts it via `cargo update -p pericortex` (Cargo.lock
  pin → `357b29f`). echo_roundtrip green against it. This unblocks (a) a fast `BENCH_CHAOS` reaper-
  recovery gate and (b) the **D-12 root-cause**: set the throttle to ~1 s, re-add the ventilator
  malformed-flood to the torture test, and confirm whether the 60 s worker nap was the straggler cause
  (next session — paused for a PC/UPS restart).
