# Stratified Sandboxes — design / plan-of-record (issue #46)

> Status: **PLANNED 2026-06-19** — design complete, owner decisions locked, implementation deferred.
> This is the live successor to the archived [`archive/SANDBOX_CORPORA.md`](archive/SANDBOX_CORPORA.md),
> which covers the **carve primitive** that already landed (2026-06-15). This doc covers the
> **stratification** layer that closes [#46](https://github.com/dginev/CorTeX/issues/46). Relates to
> Arm 5 (corpus management), Arm 7 (runs), Arm 10 (data management); built on the background-job
> mechanism ([`archive/JOB_MODEL.md`](archive/JOB_MODEL.md)).

## Where we are vs. what #46 asks

The 2026-06-15 work (`src/backend/sandbox.rs` + `POST /api/corpora/<parent>/sandbox` + the
`corpus_sandbox` job) built the **carve primitive**: a real corpus + `parent_corpus_id` + `selection`
JSONB, a one-transaction server-side `INSERT … SELECT`, and output isolation. That `selection` is a
flat **condition filter** — `status` ∩ `message (severity/category/what)` ∩ `entry LIKE` ∩ a global
`LIMIT`. It is *not* a stratified sample.

Issue #46 asks for **balanced per-class sampling** with per-class bounds, surfaced from the
**(corpus, service) report page**, plus a **downloadable-corpus lifecycle**. Measured against the
issue, the current state is:

| #46 scope item | Status today |
|---|---|
| A. Form on the (corpus, service) **report page** | ❌ form lives on the corpus page only |
| B1. Stratify by **path fragments** (balanced per segment) | ❌ only `entry LIKE '%…%'` |
| B2. Stratify by **message class** (equal per `severity:category:what`) | ❌ keeps *all* matches |
| B3. **Count/frequency bounds** (lower + upper per class) | 🟡 only a single global cap |
| C1. New corpus auto-named `{corpus·service·nickname·timestamp}` | 🟡 user types the name |
| C2. Specialized stratified Postgres query | ❌ plain `EXISTS` + `LIKE` + `LIMIT` |
| C3. Each reported entry added to the new corpus | ✅ done |
| C4. Service auto-activated to prime a run | ❌ separate manual call |
| D. Downloadable custom corpora (dangling link + bg task) | ❌ `export-dataset` is HTML results, not the corpus |

## Core insight: filter and stratification axis are independent

The existing fields select the **candidate pool**; stratification then **balances** that pool by one
axis with per-class bounds. All three of the issue's examples decompose cleanly this way:

| Issue example | Filter (existing) | Stratify axis (new) | Bounds (new) |
|---|---|---|---|
| "1 entry per message registered" | (broad) | `MessageClass` | `per_class_max = 1` |
| "≤1,000 per class, by path fragments" | message class | `PathSegment` | `per_class_max = 1000` |
| "unlimited for a specific class" | that class | `None` | (no cap) |

So **one new `stratify_by` axis + per-class bounds** covers the whole vision. Crucially, **no schema
migration** is needed for the core — every new field rides inside the existing `selection JSONB`, and
old selections deserialize unchanged via `#[serde(default)]`.

## Owner decisions (2026-06-19)

1. **Lower bound = "at least min, keep all."** `per_class_min` is a *floor guarantee*, **not a gate** —
   it never drops a class. It only bites when a global `max_entries` would otherwise starve a class, so
   it is implemented as a **two-pass allocation**: reserve `min` per class first (or all, if the class
   is smaller), then fill the remaining global budget by the sampling order. With no global cap it is
   inert. (Simpler than a `HAVING class_n >= min` gate, and matches the "keep all" intent.)
2. **Seeded shuffle by default.** Within-stratum order is `md5(entry || seed)` with an
   **auto-generated seed stored in `selection.sample_seed`**, so the carve is an unbiased
   representative sample yet fully reproducible (re-runnable, auditable). A user-supplied seed
   overrides; deterministic-by-`entry` order remains available via a sentinel/empty seed.
3. **Path stratum key = Nth `/` segment.** `split_part(entry, '/', depth)`, `depth` default `1`,
   exposed as a single numeric form field. No regex-capture surface in v1.

## Data model — extend `SandboxSelection` (`src/backend/sandbox.rs`)

Additive fields, serialized into the existing `selection` column:

```rust
pub stratify_by:   Option<StratifyAxis>,  // PathSegment { depth } | MessageClass   (None = today's flat carve)
pub per_class_max: Option<i64>,           // upper bound: rn <= n per stratum
pub per_class_min: Option<i64>,           // lower bound: floor guarantee under a global cap (decision 1)
pub sample_seed:   Option<String>,        // decision 2 — auto-generated, stored for reproducibility
pub nickname:      Option<String>,        // for auto-naming (C1)
pub activate:      bool,                   // #[serde(default = true)] — prime a run after the carve (C4)
```

`StratifyAxis` is a new `#[serde(tag = …)]` enum. `validate()` gains rules:
- `MessageClass` stratification **requires** `message_severity` (it needs the `log_*` join).
- bounds must be ≥ 0; `per_class_min ≤ per_class_max` when both are set.
- `PathSegment { depth }` requires `depth ≥ 1`.

No migration: this is a strict superset of the current `selection` JSON.

## The stratified carve SQL (the heart — `create_sandbox`)

Replace the four flat `INSERT … SELECT … LIMIT` branches with a windowed CTE. The candidate pool is
**exactly today's WHERE**; stratification wraps it:

```sql
WITH candidates AS (
  SELECT DISTINCT t.id, t.entry, <stratum_key> AS k        -- k present only when stratifying
  FROM tasks t [JOIN <log_table> l ON l.task_id = t.id AND <cat/what>]
  WHERE t.corpus_id = <parent> AND t.service_id = <svc> <status> <EXISTS msg> AND t.entry LIKE $1
),
strata AS (
  SELECT entry, k,
         row_number() OVER (PARTITION BY k ORDER BY <order>) AS rn,
         count(*)     OVER (PARTITION BY k)                  AS class_n
  FROM candidates
)
INSERT INTO tasks (service_id, corpus_id, status, entry)
SELECT DISTINCT ON (entry) <svc>, <sandbox_id>, <todo>, entry
FROM strata
WHERE (<per_class_max> IS NULL OR rn <= <per_class_max>)
ORDER BY entry, k                                            -- DISTINCT ON tiebreak
<global LIMIT max_entries>
```

- **Stratum key**: `PathSegment{depth}` → `split_part(t.entry, '/', depth)`; `MessageClass` →
  `l.category || ':' || l.what`. Axis `None` keeps today's **exact** SQL (regression-tested identical).
- **`<order>`**: `md5(entry || seed)` by default (decision 2); `entry` when the seed is the
  deterministic sentinel. This is what makes it a *sample*, not "the lexicographically-first N".
- **`DISTINCT ON (entry)`** is critical: message-class stratification can surface one entry under
  multiple classes, but `tasks UNIQUE(entry, service_id, corpus_id)` allows one row per entry — keep
  the first class deterministically (faithful to today's `SELECT DISTINCT entry`).
- **Lower-bound two-pass** (decision 1): when `per_class_min` **and** a global `max_entries` are both
  set, a first pass takes `rn <= per_class_min` from every class, then a second pass fills the residual
  `max_entries` budget by global `<order>`, `UNION`-ed and de-duplicated by entry. Without a global
  cap, `per_class_min` is inert and the single-pass query above is used.
- Safety unchanged: validated ints inlined; `category`/`what`/`entry`/`seed` bound; only the
  fixed-map `log_*` identifier interpolated.

## Auto-naming (C1) + auto-activation (C4)

- **Naming**: make `name` optional in `SandboxRequest`. When absent, `start_sandbox` derives
  `{parent}-{service_name}-{nickname}-{UtcYYYYMMDDThhmmZ}` (looks up the service name by `service_id`,
  stamps `Utc::now()`). An explicit `name` still overrides.
- **Activation**: when `selection.activate`, `run_sandbox` calls the existing service-activation
  primitive (the one behind `POST /api/corpora/<corpus>/services/<service>`) on the new sandbox as its
  closing step — so the `TODO` work-list is immediately a primed, reportable run. Idempotent.

## Report-page UI (A)

The human route already exists (`POST /corpus/<parent>/sandbox`); the gap is purely *where the form
lives* and *context pre-fill*. Add a "Carve a stratified sandbox" panel partial to the report
templates (`report.html.tera`, `severity-report.html.tera`, `category-report.html.tera`,
`task-list-report.html.tera`):

- `service_id` is implicit from the report's `(corpus, service)` context — a hidden field, no dropdown.
- On a `severity/category/what` drill page, pre-fill those + default `stratify_by = MessageClass`.
- Expose: nickname, axis (path-segment depth / message-class), `per_class_min` / `per_class_max`,
  optional seed, global cap. Extend `SandboxForm` with the matching optional fields.
- The existing corpus-page form keeps working and gains the same controls.

## Download lifecycle (D) — separable Phase 4

A `corpus_download` background job assembles a portable bundle (entry manifest + `selection`
provenance; optionally results, reusing the `export-dataset` machinery), writes it to the assets dir
under a uuid, and flips a "ready" flag. A dangling link `GET /api/corpora/<name>/download/<token>`
returns `425 Too Early` until ready, then streams; the create call returns `202` + a job handle + the
eventual URL. Optional completion notification rides the existing job-completion surface. Lands after
Phases 1–3 without blocking the close.

## Tests

- **Unit** (`sandbox.rs`): `validate()` for the new stratify combos; bound ordering; `MessageClass`
  needs `message_severity`.
- **Backend** (`tests/corpora_test.rs` or a new `sandbox_test.rs`): seed a parent with a **known class
  distribution**, assert per-class caps/floors, `DISTINCT ON (entry)` dedup, deterministic-vs-seeded
  ordering, two-pass lower-bound allocation under a global cap, and that **non-stratified carves remain
  byte-identical to today**.
- **API/UI**: new fields round-trip JSON ↔ form; report-page form posts; auto-naming; activation
  primes a runnable list.

## Sequencing

- **Phase 1** — model + validation + stratified SQL + its tests (closes B1/B2/B3/C2).
- **Phase 2** — auto-naming + auto-activation (C1/C4).
- **Phase 3** — report-page form + context pre-fill (A).
- **Phase 4** — download lifecycle (D).

#46 is **substantially closeable after Phase 3**; Phase 4 completes the "full lifecycle UX."

## Relationship to existing work

- **Carve primitive**: [`archive/SANDBOX_CORPORA.md`](archive/SANDBOX_CORPORA.md) — the Model-C
  corpus + parent link + `selection` JSONB + output isolation this builds on.
- **Reports / rollups**: the filter and the `MessageClass` axis use the same
  `(severity, category, what)` dimensions the report pages already expose, so the form reuses them.
- **`examples/sandbox_arxiv.rs`**: the ID-list precursor; stratification is the productized
  generalization.
- **Background jobs** ([`archive/JOB_MODEL.md`](archive/JOB_MODEL.md)): both the carve and the
  download are job kinds — the mechanism already exists.
