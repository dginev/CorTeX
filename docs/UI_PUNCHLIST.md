# Frontend / UX punchlist

Owner-reported UI/UX/deploy polish, tracked here as the durable record (the in-session task list is
ephemeral). Resilience/correctness gaps live separately in [`KNOWN_ISSUES.md`](KNOWN_ISSUES.md); the
sprint roadmap is [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md).

Status: 🔴 open · 🟡 in progress · 🟢 done (kept for history).

## Open / in progress

| # | St | Item |
|---|----|------|
| U-1 | 🟡 | **DRY the tabular reporting.** Many report tables (jobs/audit/runs/sessions/services/reports) use ad-hoc, divergent scaffolds (the `.row`/`.col-md-*` Bootstrap classes are undefined no-ops). Converge on ONE HTML scaffold + class vocabulary + CSS design tokens; extract a shared Tera partial/macro (no components, so partials). **Step 1 DONE (2b2f67d):** spacing design tokens (`--space-*`) in `:root`, `.gap-*` rewired to them, real `.report-layout` centered-column class defined, `.table` centred. **Next:** apply `.report-layout` + a shared `report-table`/page-header partial across the table templates, one at a time, verifying each renders. |
| U-7 | 🟡 | **Drop "view as JSON" from public report pages** (owner: "not for people to view"). DONE for the report ladder (report/severity/category templates). Admin-gated screens keep their JSON twin (intended discoverability). **Flagged:** the public overview (`/`, overview.html.tera) still links "view as JSON" → `/api/corpora` — same principle; confirm whether to remove. |
| U-3 | 🟡 | **Edge Anubis scope (owner directive).** Exempt ONLY `/healthz` (+ page assets) from Anubis; guard HTML **and** `/api/*` **and** `/entry/*` (don't let bots crawl the API or archives). Agents use **localhost** (unguarded). **Config DONE** in `deploy/edge/corpora.caddy` + `deploy/README.md` + `KNOWN_ISSUES.md` X-4 → 🟡. **Remaining (owner, on the Vultr edge VM):** append the updated block to the edge `/etc/caddy/Caddyfile` and `caddy validate && systemctl reload caddy`. Then X-4 → 🟢. |
| U-4 | 🟢 | **Progress-bar inline widths → CSS custom properties — DONE.** The data-driven bars now pass `style="--bar-width: {{pct}}%"` with `.bar { width: var(--bar-width, 0) }` in cortex.css (report/severity/category templates). Only the data value stays inline (the idiomatic custom-property pattern). **Follow-up (fold into U-1):** static `style="width:N%"` table-column widths remain in `history.html.tera` (`<th>`) + `document-report.html.tera` (`<col>`) — drain into the shared table scaffold. |
| U-5 | 🟢 | **`cortex deploy` helper — DONE.** `scripts/deploy.sh` codifies the recurring deploy: build release → migrate online (`cortex init`, backward-compatible so zero-downtime) → restart → verify `/healthz`. `--no-migrate` for code/CSS-only deploys. (A script, not a CLI subcommand — a binary can't cleanly rebuild+restart itself.) |
| U-6 | 🔴 | **Tracked `.env` carries a real DB password** (secret-in-repo, pre-existing). Gitignore `.env`, track a `.env.example` template instead, and rotate the password. (After the cortex_load→cortex rename, `.env` DATABASE_URL points dev at `cortex_dev`.) |

## Done (this session)

- 🟢 **Production cutover** — `cortex_load` renamed → `cortex` (main DB), `productize-2026` deployed (Arm 3 migrations applied online, zero downtime), owner-verified. See the deployment memory.
- 🟢 **Entry downloads named by document** — `/entry/import/39135` → `0811.0417.zip` (`Content-Disposition` from `helpers::entry_document_name`). Deployed + verified live.
- 🟢 **/jobs table centering** — `.table { margin-inline: auto }` (c58b848).
- 🟢 **Admin stat-card widening** — large corpus counts no longer break mid-number (2b2f67d).
- 🟢 **U-2: API-docs nav summaries** — short per-operation summary derived in `apidoc::mount` so the RapiDoc left-nav is readable (7c15f7c).
- 🟢 **U-7: "view as JSON" off the public report ladder** — removed from report/severity/category (public overview flagged in U-7 above).
