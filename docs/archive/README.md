# docs/archive — frozen records of completed work

These documents describe work that has **landed**; they are kept for history, not active tracking.
Live state lives one level up: the plan + current-state map in
[`../PRODUCTIZING_PLAN.md`](../PRODUCTIZING_PLAN.md) and the resilience ledger in
[`../KNOWN_ISSUES.md`](../KNOWN_ISSUES.md).

| Item | What it is |
|---|---|
| [`PROGRESS_LOG.md`](PROGRESS_LOG.md) | Append-only, dated log of every productization increment. |
| [`api-spike/`](api-spike/) | The `rocket_okapi` vs `utoipa` head-to-head (okapi chosen). The `api_doc_spike_okapi` example + `scripts/render_api_spike.py` still regenerate the spec/pages here. |
| [`AAA_DESIGN.md`](AAA_DESIGN.md) | Authentication/authorization/accounting design — token→owner identity, uniform authz, the `audit_log` pillar (all landed). |
| [`WEBAUTHN_DESIGN.md`](WEBAUTHN_DESIGN.md) | Passkey (WebAuthn) sign-in design — foundation → sessions → enrollment → sign-in (all landed). |
| [`JOB_MODEL.md`](JOB_MODEL.md) | Background-job mechanism design (in-process threads + `jobs` table); shipped as `src/jobs.rs`. |
| [`REPORT_FRESHNESS.md`](REPORT_FRESHNESS.md) | The two-tier `report_summary` rollup refresh model (on-drain + at-least-daily). |
| [`ARCHIVE_RATIONALIZATION.md`](ARCHIVE_RATIONALIZATION.md) | Archive-handling rationalization — pure-Rust `flate2`/`tar`/`zip`/`infer` (Path A shipped, libarchive removed). |
| [`LOAD_TESTING.md`](LOAD_TESTING.md) | Live-backup seeding / load-test lifecycle — superseded once `cortex_load` became the persistent public showcase DB. The evergreen migration-fidelity check lives on as `scripts/verify_migrations.sh`. |

> These design docs are still cited from source-code comments (paths updated to `docs/archive/…`); the
> code is the live artifact, these are the rationale kept for history.
