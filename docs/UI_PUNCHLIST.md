# Frontend / UX punchlist

Owner-reported UI/UX/deploy polish, tracked here as the durable record (the in-session task list is
ephemeral). Resilience/correctness gaps live separately in [`KNOWN_ISSUES.md`](KNOWN_ISSUES.md); the
sprint roadmap is [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md).

Status: 🔴 open · 🟡 in progress · 🟢 done (kept for history).

## Open / in progress

| # | St | Item |
|---|----|------|
| U-1 | 🟡 | **DRY the tabular reporting.** Many report tables (jobs/audit/runs/sessions/services/reports) use ad-hoc, divergent scaffolds (the `.row`/`.col-md-*` Bootstrap classes are undefined no-ops). Converge on ONE HTML scaffold + class vocabulary + CSS design tokens; extract a shared Tera partial/macro (no components, so partials). **Step 1 DONE (2b2f67d):** spacing design tokens (`--space-*`) in `:root`, `.gap-*` rewired to them, real `.report-layout` centered-column class defined, `.table` centred. **Next:** apply `.report-layout` + a shared `report-table`/page-header partial across the table templates, one at a time, verifying each renders. |
| U-2 | 🔴 | **API docs nav sidebar is messy** (`/api/docs/index.html`). The RapiDoc left-nav uses each endpoint's long `///` doc-comment as its description → unreadable in a nav setting. Give each `#[openapi]` route a short **summary** (one terse line) distinct from the long description, so the nav shows the summary. (rocket_okapi derives summary+description from the doc comment — first line = summary; check it's splitting as intended, else shorten.) |
| U-3 | 🔴 | **Edge Anubis scope (owner directive).** For the public edge (corpora.latexml.rs) exempt ONLY the **health** endpoint(s) from Anubis (so outside agents can check uptime); guard everything else incl. `/api/*` and `/entry/*` (don't want bots crawling resources). Main agentic use is **localhost** (unguarded — "friendly bots are already our guests"). Future: maybe an external enterprise MCP, but not soon. This is an EDGE config change (Caddy/Anubis bypass list, in the ar5iv-editor repo / on the edge VM — not cortex app code) + updating `deploy/` docs; closes [`KNOWN_ISSUES.md`](KNOWN_ISSUES.md) **X-4** (un-walled `/api`). Keep `/public/*` exempt so walled pages still load their assets. |
| U-4 | 🔴 | **Progress-bar inline widths → CSS custom properties.** The last inline `style=` in the frontend is the data-driven `style="width: {{pct}}%"` on the report/history progress bars. Migrate to `style="--bar: {{pct}}%"` + a `.bar { width: var(--bar) }` class — the "design tokens" completion the owner deferred. |
| U-5 | 🔴 | **`cortex deploy` helper.** Codify the recurring deploy as `scripts/deploy.sh` (build release → migrate online via `cortex init` → repoint/restart → verify `/healthz`), since a self-rebuilding binary can't cleanly do build+restart. The one-time DB rename is NOT part of it. |
| U-6 | 🔴 | **Tracked `.env` carries a real DB password** (secret-in-repo, pre-existing). Gitignore `.env`, track a `.env.example` template instead, and rotate the password. (After the cortex_load→cortex rename, `.env` DATABASE_URL points dev at `cortex_dev`.) |

## Done (this session)

- 🟢 **Production cutover** — `cortex_load` renamed → `cortex` (main DB), `productize-2026` deployed (Arm 3 migrations applied online, zero downtime), owner-verified. See the deployment memory.
- 🟢 **Entry downloads named by document** — `/entry/import/39135` → `0811.0417.zip` (`Content-Disposition` from `helpers::entry_document_name`). Deployed + verified live.
- 🟢 **/jobs table centering** — `.table { margin-inline: auto }` (c58b848).
- 🟢 **Admin stat-card widening** — large corpus counts no longer break mid-number (2b2f67d).
