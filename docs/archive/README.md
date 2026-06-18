# docs/archive — relocated records (mostly completed-work history)

Most of these describe work that has **landed**; they are kept here to keep the top-level `docs/` tidy.
The one exception is [`PROGRESS_LOG.md`](PROGRESS_LOG.md) — it lives here for tidiness but is **still
the live, append-only trail** (new increments are appended to it). Live *state* lives one level up:
the plan + current-state map in [`../PRODUCTIZING_PLAN.md`](../PRODUCTIZING_PLAN.md) and the resilience
ledger in [`../KNOWN_ISSUES.md`](../KNOWN_ISSUES.md).

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
| [`DISPATCHER_RATIONALIZATION.md`](DISPATCHER_RATIONALIZATION.md) | The lock-free / fanned-out dispatcher arm (owner directive 2026-06-14). Phases 0–4 + leveled logging + keepalive **all landed**; the phase-5 transport question is **decided** (keep libzmq ventilator + zmq.rs sink). The canonical architecture rationale, still cited from `src/dispatcher/`, `src/config.rs`, and the `examples/zmq_*` spikes. |
| [`DISPATCHER_5B_ROOT_CAUSE.md`](DISPATCHER_5B_ROOT_CAUSE.md) | **Closing report** of the phase-5b investigation: why the async `zeromq` ROUTER ventilator throughput collapses at scale and both pure-Rust ØMQ crates cap at ~3000–4400/s vs libzmq's ~8500/s (decision: keep libzmq ventilator). The two working notes it supersedes — [`DISPATCHER_5B_PERF_AUDIT.md`](DISPATCHER_5B_PERF_AUDIT.md) (the audit handoff) and [`DISPATCHER_5B_CODEX_FINDINGS.md`](DISPATCHER_5B_CODEX_FINDINGS.md) (the second-engineer findings) — are kept beside it for the evidence trail. |
| [`SANDBOX_CORPORA.md`](SANDBOX_CORPORA.md) | Filtered-sandbox-corpora design (Arm 5) — server-side carve + rerun-output isolation (the former KNOWN_ISSUES F-6). All landed; shipped as `src/backend/sandbox.rs`. |
| [`RESOURCE_RATIONALIZATION.md`](RESOURCE_RATIONALIZATION.md) | Plan Arm 14 design notes — the six resource/perf mini-choices + the measurement-spike evidence. The implemented ones (#4 pooled metadata, #6 report rollups) landed; the live Arm-14 entry stays in [`../PRODUCTIZING_PLAN.md`](../PRODUCTIZING_PLAN.md). |

> These design docs are still cited from source-code comments (paths updated to `docs/archive/…`); the
> code is the live artifact, these are the rationale kept for history.
