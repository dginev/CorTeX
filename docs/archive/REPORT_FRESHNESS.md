# Report freshness model

CorTeX serves every aggregate report (the category / `what` drill-downs, severity totals) from the
**`report_summary` materialized view** — an indexed rollup that turns an O(hundreds-of-millions of
log rows) aggregation into a sub-millisecond lookup (`src/backend/rollup.rs`, `src/backend/reports.rs`).
The trade-off a materialized view makes is **freshness vs. cost**: the rollup is only as current as
its last `REFRESH`, and a refresh is **global** — one `REFRESH MATERIALIZED VIEW` rebuilds the data
behind *every* report page at once (there is no per-page refresh; the view is one relation). At
production scale (5.87M tasks, 273M `log_infos`) a full rebuild measured **~2 min**.

So freshness is a two-part guarantee: an **automatic regular rebuild** keeps everything reasonably
fresh cheaply, and an **on-demand forced rebuild** lets an operator/agent pull the latest immediately
when it matters. Both are non-blocking for readers (`REFRESH ... CONCURRENTLY`, R-4) and never block a
request thread.

## 1. Automatic refresh (the baseline guarantee)

The dispatcher's finalize thread (`src/dispatcher/finalize.rs`) refreshes the rollup:

- **on drain** — whenever the finalized-task queue empties, so completed work shows up in reports
  promptly during normal operation; and
- **at a regular interval** — because a single conversion run can take *weeks* without ever draining,
  staleness is bounded by a periodic refresh. The interval is runtime-configurable via
  `dispatcher.report_refresh_interval_seconds` (`cortex.toml` / `CORTEX_DISPATCHER__REPORT_REFRESH_INTERVAL_SECONDS`),
  **default 3600 s (1 h)**.

Because the refresh is now `CONCURRENTLY` (readers keep seeing the prior rollup throughout), a tighter
interval is cheap on the read path — the only cost is one rebuild's DB load per interval (a few
parallel-worker minutes), at predictable times. Tune the interval to trade DB load against staleness.

## 2. Forced refresh (on demand, async)

When you need the rollup current *now* — e.g. an agent that just changed state and wants reports to
reflect it without waiting for the interval — trigger a forced rebuild. Since the rebuild is
multi-minute, it runs as a **background job**, never on the request thread:

| | |
|---|---|
| **Agent API** | `POST /api/reports/refresh` (token-gated via `X-Cortex-Token` / `?token=`). Returns `202` with `{ "job": "<uuid>", "poll": "/api/jobs/<uuid>", "actor": "…" }`. |
| **Human UI** | the **"Refresh reports now"** button on the jobs dashboard (`/jobs`) → redirects back to `/jobs`, where the job's status / health / duration are visible (async, no JS). |
| **Poll** | `GET /api/jobs/<uuid>` → the job's `health` (`pending` → `running` → `ok`/`failed`) and `duration_seconds`. |

**Debounced.** A single global rebuild already refreshes *every* report, so if a refresh job is
already queued or running, the trigger returns that job's uuid instead of starting another (concurrent
rebuilds would only serialize on the matview lock). A flood of triggers collapses to one job.

## Cost & guarantees, at a glance

- **Reads never block** on a refresh (`CONCURRENTLY`) and never recompute live — every report is an
  indexed rollup lookup.
- **Writes never block** on a refresh: the automatic refresh runs on the finalize thread (off the
  request path); the forced refresh runs as a background job.
- **Staleness bound** = `min(time-since-drain, report_refresh_interval_seconds)`, or 0 right after a
  forced refresh completes.

## Known follow-up

- **R-5:** the rerun path (`mark_new_run`) still refreshes **inline and synchronously**, so a rerun
  over a large corpus blocks its HTTP request ~2 min. It should reuse `jobs::spawn_report_refresh`
  (this module) to refresh off the request path. Until then, a rerun's reports also catch up via the
  automatic interval, and the forced-refresh endpoint above is the manual lever. See
  `docs/KNOWN_ISSUES.md` (R-5).
