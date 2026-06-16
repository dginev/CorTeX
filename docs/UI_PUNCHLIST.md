# Frontend / UX punchlist

Owner-reported UI/UX/deploy polish, tracked here as the durable record (the in-session task list is
ephemeral). Resilience/correctness gaps live separately in [`KNOWN_ISSUES.md`](KNOWN_ISSUES.md); the
sprint roadmap is [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md).

Status: 🔴 open · 🟡 in progress · 🟢 done (kept for history).

## Open / in progress

| # | St | Item |
|---|----|------|
| U-1 | 🟡 | **DRY the tabular reporting.** Many report tables (jobs/audit/runs/sessions/services/reports) used ad-hoc, divergent scaffolds (the `.row`/`.col-md-*` Bootstrap classes are undefined no-ops). **VISUAL/CSS DRY DONE:** spacing + type design tokens in `:root`, `.gap-*` rewired to them, `.table { margin-inline: auto }` centring, and a canonical **85% centered content column** (owner's pick) on `.report-layout` + the transitional `.col-md-10` alias (empty `.col-md-1` spacers hidden) — every tabular page now shares the same layout. **Remaining (source hygiene, no visual change, deferred):** migrate the templates from `.col-md-*` to `.report-layout` (then drop the alias); extract a shared page-header/table Tera partial; drain the static `style="width:N%"` column widths in history/document-report. |
| U-7 | 🟢 | **Drop "view as JSON" from public pages** (owner: "not for people to view") — DONE. Removed from the report ladder (report/severity/category) and the public overview (`/`). Admin-gated screens keep their JSON twin (intended discoverability). |
| U-3 | 🟡 | **Edge Anubis scope (owner directive).** Exempt ONLY `/healthz` (+ page assets) from Anubis; guard HTML **and** `/api/*` **and** `/entry/*` (don't let bots crawl the API or archives). Agents use **localhost** (unguarded). **Config DONE** in `deploy/edge/corpora.caddy` + `deploy/README.md` + `KNOWN_ISSUES.md` X-4 → 🟡. **Remaining (owner, on the Vultr edge VM):** append the updated block to the edge `/etc/caddy/Caddyfile` and `caddy validate && systemctl reload caddy`. Then X-4 → 🟢. |
| U-4 | 🟢 | **Progress-bar inline widths → CSS custom properties — DONE.** The data-driven bars now pass `style="--bar-width: {{pct}}%"` with `.bar { width: var(--bar-width, 0) }` in cortex.css (report/severity/category templates). Only the data value stays inline (the idiomatic custom-property pattern). **Follow-up (fold into U-1):** static `style="width:N%"` table-column widths remain in `history.html.tera` (`<th>`) + `document-report.html.tera` (`<col>`) — drain into the shared table scaffold. |
| U-5 | 🟢 | **`cortex deploy` helper — DONE.** `scripts/deploy.sh` codifies the recurring deploy: build release → migrate online (`cortex init`, backward-compatible so zero-downtime) → restart → verify `/healthz`. `--no-migrate` for code/CSS-only deploys. (A script, not a CLI subcommand — a binary can't cleanly rebuild+restart itself.) |
| U-6 | 🟡 | **Tracked `.env` carries a real DB password** (secret-in-repo, pre-existing). **Repo-side DONE (2026-06-16):** `.env` is now **untracked** (`git rm --cached`, working copy kept) + **gitignored**, with a committed `.env.example` placeholder template; production already uses the systemd `EnvironmentFile`, not the repo `.env`, so nothing breaks. **Owner action still required:** the old password is in git **history** (untracking doesn't scrub it) — **rotate the `cortex` DB password** to fully invalidate the exposed credential. Until rotated this stays 🟡. |

## Done (this session)

- 🟢 **Production cutover** — `cortex_load` renamed → `cortex` (main DB), `productize-2026` deployed (Arm 3 migrations applied online, zero downtime), owner-verified. See the deployment memory.
- 🟢 **Entry downloads named by document** — `/entry/import/39135` → `0811.0417.zip` (`Content-Disposition` from `helpers::entry_document_name`). Deployed + verified live.
- 🟢 **/jobs table centering** — `.table { margin-inline: auto }` (c58b848).
- 🟢 **Admin stat-card widening** — large corpus counts no longer break mid-number (2b2f67d).
- 🟢 **U-2: API-docs nav summaries** — short per-operation summary derived in `apidoc::mount` so the RapiDoc left-nav is readable (7c15f7c).
- 🟢 **U-7: "view as JSON" off the public report ladder** — removed from report/severity/category (public overview flagged in U-7 above).
- 🟢 **U-4: progress-bar widths → `--bar-width` custom property** (c698c11, deployed).
- 🟢 **U-5: `scripts/deploy.sh`** — the recurring deploy (build → migrate online → restart → verify).
- 🟢 **"Look up an article" fixes** (4882e6f, deployed): real placeholder example (`1308.3966`), and the human `/document/…/<name>` renders a friendly "no such article + back to report" page on a miss instead of the bare 404 catcher (agent twin keeps its plain 404).
- 🟢 **Forensics info-messages accordion** (672d32a, deployed): the collapsed `<details>` summary is now link-coloured + bold with a large rotating ▸ chevron, so it reads as expandable.
