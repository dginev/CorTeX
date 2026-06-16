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
| Reports: severity → category → `what` | ✓ | ✓ JSON | ✗ |
| Run list / current / **diff** / changed-tasks | ✓ | ✓ JSON | ✗ |
| **Per-article forensics** ("errors of this article") | partial (`/preview`) | ✓ **A1 landed** | ✗ |
| **Macro history trend** (rate over time) | ✓ Vega chart | ✓ (via `/api/runs/<c>/<s>` tallies) | ✗ |
| Top-of-service severity summary (`progress_report`) | ✓ | ✓ **A3 landed** (`/api/reports/<c>/<s>`) | ✗ |
| Rerun / reconvert (filtered) | ✓ | ✓ | ✗ |
| Extend corpus | ✓ | ✓ | ✗ |
| Sandbox via filter | ✓ | ✓ | ✗ |
| Service register / activate / delete | ✓ | ✓ | ✗ |
| Export dataset | ✗ | ✗ | ✓ |
| Live ops console | ✓ (landed) | `/metrics` | ✗ |
| Init / configure / health | ✓ | ✓ | ✓ (install only) |

Reading: the **agent micro + macro magnifications are the biggest gaps**, the **CLI is install-only**
(no management), and the web has cohesion/workflow polish debt. Everything else is one backend op
already shipped — the work is *projection + gap-fill*, not new pipelines.

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
  clumsy. Export dataset gets an agent endpoint (currently CLI-only).

### Arm B — CLI as a first-class surface (direction 4)
- **B1 — Management subcommands.** `cortex report|runs|document|rerun|extend|sandbox …` — thin
  clients over the **same backend ops/DTOs** as Arm A (no new logic; render the DTO as a table/JSON).
  Makes the CLI scriptable for the same questions and mutations.
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
  trend), consuming the Arm-A DTOs — closing the symmetry loop the other way.

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
