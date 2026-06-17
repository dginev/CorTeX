# Experience Rationalization — admin UI · CLI · agent API as one capability surface

> Status: **plan (2026-06-15)**, owner-requested. Rationalizes five directions (live ops console,
> design-system polish, smoother workflows, a rich guided CLI, and rich agentic workflows) into one
> coherent program. Cross-ref: [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md) (the master plan),
> [`DESIGN_PRINCIPLES.md`](DESIGN_PRINCIPLES.md) (the symmetry contract). The live ops console
> (direction 1, the dashboard's first facet) has **landed**; the rest is sequenced below.

## 1. The unifying model: capability × magnification × surface

CorTeX is, for an operator or an agent, **a database that tracks conversion outcomes at multiple
levels of magnification**. The product goal is that *answering a question never requires keeping
local notes* — you ask CorTeX, at the zoom level you need, through whichever surface you're in.

The five directions are not five features. They are **the same capabilities** projected onto three
**consumer surfaces**, at four **magnifications**:

**Magnifications (zoom levels):**
- **L0 Macro** — corpus-wide health and **historical trends**: how conversion rates moved across
  runs/time ("how have our development topics historically affected the macro conversion rates of
  this corpus?").
- **L1 Meso** — the report ladder: run → service → **severity → category → what** breakdowns, and
  **run diffs** ("which articles changed severity between these two runs?").
- **L2 Micro** — a single article: its per-service status and **the specific errors + forensic log
  evidence** ("what are the errors of this article?").
- **Mgmt** — the mutations: **rerun/reconvert** a filtered scope, **extend** a corpus, **create a
  sandbox via filter**, register/activate/delete a service, **export** a dataset.

**Surfaces (consumers):**
- **A. Web admin UI** — humans, rich/live/visual (directions 1·2·3).
- **B. CLI** — humans in a terminal: guided install + scriptable management (direction 4).
- **C. Agent API** — machines, the tight agentic loop: forensic drill-down + direct management
  (direction 5). *This is the "notes substitute": one call answers a question that would otherwise
  become a paragraph of local notes.*

## 2. The rationalization principle

**One capability = one backend operation = one DTO, surfaced through all three consumers.** This
generalizes the existing **symmetry contract** (today: web HTML + agent JSON from one controller)
from *two* surfaces to *three* (add the CLI). The corollary that drives sequencing:

> **The agent API is the keystone.** Every capability, expressed once as a typed DTO behind a
> documented endpoint, is then *rendered* by the web UI (HTML) and the CLI (text/table) for nearly
> free. So we complete the **agent-API DTO layer first**, then project it onto CLI + web — we do not
> build three parallel implementations.

## 3. Current coverage (grounded audit, 2026-06-15)

| Capability | Web UI | Agent API | CLI |
|---|---|---|---|
| Reports: severity → category → `what` | ✓ | ✓ JSON | ✓ (`cortex report --severity/--category/--what`) |
| Run list / current / **diff** / changed-tasks | ✓ | ✓ JSON | ✓ list+current (`cortex runs`) + **diff** (`cortex diff`); per-task changed-tasks still web/agent-only |
| **Per-article forensics** ("errors of this article") | ✓ (`/document/<c>/<s>/<name>`) | ✓ **A1 landed** | ✓ (`cortex document`) |
| **Macro history trend** (rate over time) | ✓ Vega chart | ✓ (via `/api/runs/<c>/<s>` tallies) | ✓ (`cortex runs` tallies + deltas) |
| Top-of-service severity summary (`progress_report`) | ✓ | ✓ **A3 landed** (`/api/reports/<c>/<s>`) | ✓ (`cortex report`) |
| Rerun / reconvert (filtered) | ✓ | ✓ | ✓ (`cortex rerun`, dry-run+`--yes`) |
| Extend corpus | ✓ | ✓ | ✓ (`cortex extend`) |
| Sandbox via filter | ✓ | ✓ | ✓ (`cortex sandbox`, dry-run+`--yes`) |
| Service register / activate / delete | ✓ | ✓ | ✓ (`cortex create-service/activate/deactivate/delete-service`) |
| Export dataset | ✓ **landed** (`/export/<c>/<s>` screen + report-page action) | ✓ **landed** (`POST …/services/<s>/export-dataset` → `dataset_export` job) | ✓ (`cortex export-dataset`) |
| Live ops console | ✓ (landed) | `/metrics` + `/api/status` | ✓ (`cortex status`) |
| Init / configure / health | ✓ | ✓ | ✓ (`init`/`doctor`/`set-admin-token`) |

Reading (updated 2026-06-16): the three-surface symmetry is **essentially complete** — every
capability is now on web · agent · CLI. The sole remaining sliver is the **per-task changed-tasks
diff** (which individual entries moved status), still web+agent only (`/runs/<c>/<s>/tasks`); the
CLI has the *summary* matrix via `cortex diff`. What's left is polish (web cohesion C1/C2, guided
`init` TUI D-B2), not capability gaps — the work was *projection + gap-fill*, and it is done.

## 4. The arms (sequenced; agent-API-first)

### Arm A — Agent forensic + trend completeness (direction 5 core; the keystone)
Make the agentic loop tight: transparent overview → drill to forensic detail → act, all as
discoverable JSON DTOs (each also the future HTML/CLI source).
- **A1 — Per-article forensics (L2, highest value). ✅ LANDED.** `GET
  /api/corpus/<c>/<service>/document/<name>` → `DocumentReportDto`: the article's status + every log
  message (`MessageDto`: severity/category/what/details) + result/preview links. Answers "what are the
  errors of this article?" in one call. `<name>` is the document short name (D-A1 resolved, §5).
- **A2 — Macro trend series (L0). ✅ ALREADY COVERED — no new endpoint.** `GET
  /api/runs/<c>/<service>` already returns each run's full per-severity tallies (`RunDto`: total /
  no_problem / warning / error / fatal / invalid / in_progress) + start/end timestamps + description.
  That **is** the historical conversion-rate series ("what was done → resulting rates", per run); an
  agent reads the trend (and the per-run rates, one division off the tallies) straight from it.
- **A3 — Service overview entry point (L1 top). ✅ LANDED.** `GET /api/reports/<c>/<service>` →
  `ServiceOverviewDto` (total + per-status `StatusCountDto{tasks, percent}`), the missing top rung —
  "how is this (corpus, service) doing?" without guessing a severity. The status keys double as the
  `<severity>` drill-down segment. Shares `progress_report` with the HTML top screen (same numbers).
- **A4 — Management ergonomics + discoverability.** Confirm rerun/extend/sandbox are ergonomic and
  documented in the OpenAPI; add a filtered-reconvert shorthand if the report→rerun round-trip is
  clumsy. **Export dataset agent endpoint ✅ LANDED** — `POST
  /api/corpora/<c>/services/<s>/export-dataset` (token-gated) spawns a `dataset_export` background
  job over the shared `export_html_dataset` core (the CLI twin's exact validation: `422` bad
  `group_by`/severity, `404` unknown corpus/service), returning `202` + the job handle. Pinned by
  `corpora_test::export_dataset_endpoint_and_human_form`. The **human web form ✅ LANDED** too — the
  `/export/<c>/<s>` screen (a sibling top-level path like `/runs`, `/history`, so it never collides
  with the report ladder) + an **Export dataset** action in the report-page admin row, over the same
  `start_export` core (a thin twin of `import_corpus_human`: 303 → the job page on success, friendly
  re-render on 404/422). Export is now complete across all three surfaces (web · agent · CLI).

### Arm B — CLI as a first-class surface (direction 4)
- **B1 — Management subcommands.** `cortex report|runs|document|rerun|extend|sandbox …` — thin
  clients over the **same backend ops/DTOs** as Arm A (no new logic; render the DTO as a table/JSON).
  Makes the CLI scriptable for the same questions and mutations. **Read surface DONE** — the CLI
  report ladder is complete across magnifications: `cortex report <c> <s>` (overview, via
  `Backend::progress_report`), `cortex runs <c> <s>` (run-history macro trend, via
  `HistoricalRun::find_by`+`with_live_tallies`), and `cortex document <c> <s> <name>` (per-article
  forensics, via `Task::find_by_name`+`backend::task_messages`) — each the CLI twin of the web/agent
  surface, all sharing one backend so the numbers agree; `--json` mirrors the agent DTOs. **Mutations
  in progress:** `cortex rerun <c> <s> [--severity/--category/--what] [--owner] [--description]` ✅
  LANDED — the CLI twin of the web/agent rerun via the shared `Backend::mark_rerun`, **dry-run by
  default** (prints the filtered scope), `--yes` to execute; validates severity (exit 2) and
  corpus/service (exit 1) before touching the DB. `cortex sandbox <parent> <name> --service <s>
  --severity <sev> [--category/--what]` ✅ LANDED too — the CLI twin of the web/agent sandbox carve
  via the shared `backend::create_sandbox`, **dry-run by default** (prints the would-be scope),
  `--yes` creates the first-class sandbox corpus and reports the captured-entry count; validates
  severity (exit 2), parent/service and name-collision (exit 1) before any write. `cortex
  delete-corpus <name>` ✅ LANDED — the CLI twin of the web/agent `DELETE /api/corpora/<name>` via
  the transactional, orphan-free `Corpus::destroy`, **dry-run by default** (prints the blast radius:
  the task count + which kind, sandbox vs corpus), `--yes` to delete; closes the **sandbox lifecycle**
  (create → iterate → delete) end-to-end on the CLI (previously a sandbox could only be removed via
  the web or raw SQL). Historical run tallies are immutable and survive. Verified end-to-end (CLI
  create → CLI delete → zero orphaned tasks, parent untouched). `extend` ✅ LANDED too (`cortex
  extend <corpus>` — the CLI twin of the corpus screen's Extend + agent `POST
  /api/corpora/<name>/extend`, driving the same `Importer::extend_corpus` + `Backend::extend_service`).
  `cortex diff <c> <s>` ✅ LANDED — the CLI twin of the web `/runs/<c>/<s>/diff` + agent `GET
  /api/runs/<c>/<s>/diff`, over the shared `summary_task_diffs` (now `pub` for the third surface),
  closing the snapshot→rerun→**diff** improvement loop on the terminal. **B1 management surface is
  complete** (the lone remaining sliver is the per-task changed-tasks drill, web/agent-only).
- **B2 — Guided init.** An interactive `cortex init --guided` walking the strategic choices (database,
  admin token, services, dispatcher knobs). **Decision D-B2 (see §5): ratatui rich TUI vs a plain
  guided prompt flow.** Default lazy: ship the plain prompt flow first (no heavy new dep; 90% of the
  value), evaluate ratatui only if the flow earns it.

### Arm C — Web cohesion + smoother workflows (directions 2·3)
- **C1 — Design-system cohesion.** Extract scattered inline styles into the `cortex.css`
  scholarly-widget classes; unify cards/tables/buttons/spacing across every admin screen. (Started
  organically in the live-ops-console card work.)
- **C2 — Smoother workflows.** Corpus add/import, service activation, rerun: inline progress +
  validation + next-step guidance, building on the live-feed pattern from direction 1.
- **C3 — Render the new magnifications.** Web views for A1 (per-article forensics) and A2 (macro
  trend), consuming the Arm-A DTOs — closing the symmetry loop the other way. **Forensic screen +
  entry points DONE:** the `/document/<c>/<s>/<name>` screen renders the `DocumentReportDto`; the
  report entry-list links each row to it; and the **service-overview now carries a "Look up an
  article" shortcut** — the human twin of the agent `GET /api/corpus/<c>/<svc>/document/<name>` +
  `cortex document`, so a human can jump straight to one article's forensics by id instead of
  drilling the severity→category→what ladder. The shortcut degrades without JS (a plain-GET
  `/document/<c>/<s>?name=<id>` → **303** to the canonical path URL, `document_lookup_redirect`).

**Why this order:** A unlocks B1 and C3 for free (shared DTOs), and A1 is both the highest-value
agent question and the single biggest current gap. B2 and C1/C2 are independent polish that can
interleave.

## 5. Decisions
- **D-A1 — document identity: RESOLVED → the entry short-name.** `Task::find_by_name(name, corpus,
  service)` already keys a document by its short name (the paper id, e.g. `0801.1234`, matched as
  `entry LIKE %name.zip`) — exactly how documents appear in reports. The agent endpoint uses it
  (`/api/corpus/<c>/<svc>/document/<name>`); no schema change, no ugly full-path URLs.
- **D-B2 — CLI init UX: RESOLVED → `ratatui` rich TUI** (owner, 2026-06-15). The guided
  `cortex init` gets a full-screen terminal UI (navigable strategic-choice panels, live validation).
  A real dependency + event loop, accepted for the delightful admin onboarding.
