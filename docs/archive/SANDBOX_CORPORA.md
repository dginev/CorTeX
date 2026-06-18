# Filtered Sandbox Corpora — design note + creation landed

> Status: **LANDED 2026-06-15** — creation (`src/backend/sandbox.rs` + `POST /api/corpora/<parent>/sandbox`
> + the `corpus_sandbox` background job) **and** rerun-output isolation (Decisions §1, formerly
> KNOWN_ISSUES F-6, now resolved). The owner's 4 design questions are **decided** (see below). A
> sandbox is now a safe rerun target. Relates to Arm 5 (corpus management), Arm 7 (runs), and Arm 10
> (data management); built on the background-job mechanism ([`JOB_MODEL.md`](JOB_MODEL.md)).
> Cross-ref: [`PRODUCTIZING_PLAN.md`](../PRODUCTIZING_PLAN.md).

## Decisions (owner, 2026-06-15) + as-built

Design **C** (a real corpus + parent link + stored selection) was chosen, with these answers to the
open questions:

1. **Outputs — isolated own tree.** A sandbox is its own `corpus_id` (own tasks, runs, reports), so
   its DB-level run state is already isolated. **Filesystem outputs are now isolated too** (2026-06-15,
   was KNOWN_ISSUES F-6): result-archive paths are derived by the single corpus-aware helper
   `helpers::result_archive_path(entry, service, sandbox_id)`, which name-scopes a sandbox's archives
   by its own id — `<entry-dir>/<service>.sandbox-<id>.zip` — so a sandbox rerun can't overwrite the
   parent's `<service>.zip` (ordinary corpora keep the historical path). The sink learns a task's
   sandbox id from a lock-free `SandboxCache` memoised by the ventilator on dispatch (no per-result
   DB hit); the frontend readers pass `corpus.sandbox_id()` so they read back the same path. Sources
   are still referenced in place.
2. **Sources — referenced in place.** Sandbox tasks carry the parent's `entry` paths verbatim; nothing
   is copied or symlinked (the dispatcher already streams a task's source from its `entry`).
3. **Selection — one-time snapshot.** The predicate is evaluated once at creation; the carved set is
   then stable to iterate campaigns against.
4. **Origin — the selection predicate IS the origin** (owner: *"the origin is the filter predicate
   applied over the larger corpus"*). No per-task `origin_task_id` link; the sandbox corpus stores its
   `parent_corpus_id` + `selection` JSON, and that predicate over the parent is the provenance.

**As built:** migration `2026-06-15-120000_sandbox_corpora` adds `corpora.parent_corpus_id` +
`corpora.selection JSONB`. `backend::create_sandbox` performs the carve **entirely server-side** —
`INSERT INTO tasks (...) SELECT ... FROM tasks ...` (a `category`/`what` filter joins the severity's
`log_*` table; `SELECT DISTINCT` dedups) — so a 100k-entry carve loads **no entries into the
application** (no client RAM, no 65535 bind-param cap) and is one atomic transaction. The carve runs
as the `corpus_sandbox` background job (the matching `SELECT` over a large parent can take up to an
hour). Tasks land as `TODO`, so the sandbox is immediately a runnable work-list.

## The target workflow

CorTeX should let an **agent** (or human) carve a working subset out of the main corpus by a
**message condition**, then iterate on it:

1. An agent identifies a group of articles via a **filter on the log messages** — e.g. *"the 10,000
   arXiv articles with a Warning `missing_file foo.cls`"*, or any `(severity, category, what)`
   selection (the same dimensions the reports already use).
2. The agent **requests a sandbox corpus** from that filter.
3. CorTeX extracts the matching entries from the main corpus and creates a new **sandbox corpus**
   that views only the filtered set.
4. **All writeable actions work on the sandbox** — rerun campaigns, save/record history — so the
   agent can **iterate conversion campaigns until a target success rate** is reached.

This is the loop agents are expected to drive: *find documents that need work (by message condition)
→ request them as a corpus → iterate campaigns until healthy.*

## How it was done before, and why it needs rationalizing

The prototype made a new filesystem directory and **symlinked the entry files** from the main
corpus into it (cf. `examples/sandbox_arxiv.rs`, which does the ID-list version). Two problems:

- **Loses the connection to the original log messages.** The sandbox starts blank; the rich
  per-entry history (why it was selected — the warnings/errors) is left behind in the parent corpus.
- **Produces a separate output tree** (one set of result archives per corpus). This is *contamination
  avoidance* — which the owner notes can be a **feature** (sandbox reruns don't perturb the main
  corpus's outputs) as much as a cost (duplication).

## Design options

The core question: is a sandbox a **physical** sub-corpus (its own `corpus_id`, its own tasks/
outputs) or a **logical** view (a saved filter over the main corpus's existing tasks)?

- **A — Logical view (saved selection):** the sandbox is just a stored query over the parent's
  tasks; no new tasks. *Pros:* zero duplication, shares the original logs + outputs, always in sync.
  *Cons:* writeable actions must be re-scoped to the selection, and a rerun mutates the **parent's**
  task state (no isolation) — fighting the existing per-`(corpus,service)` rerun/history machinery.
- **B — Physical copy (new corpus, copied tasks):** a new `corpus_id` with fresh tasks for the
  matching entries. *Pros:* the existing rerun/history machinery works unchanged (it is per-corpus);
  full isolation. *Cons:* duplicates tasks and the output tree; loses the original logs unless copied
  — the prototype's problem, just moved into the DB.
- **C — Derived sandbox (recommended): a real corpus + a parent link + a stored selection.** A
  sandbox is a first-class corpus that records its **parent corpus** and the **selection** (the
  message condition) it was built from, and whose tasks **link back to the parent's origin task** so
  the original logs remain reachable. Reruns/history run on the sandbox in isolation (its own outputs
  — the contamination-avoidance feature), while the origin link preserves provenance and the original
  messages. Creation is a **background job** that queries the parent's logs for the condition.

**Recommended: C.** It keeps the existing per-corpus writeable machinery (rerun, history) working
unchanged, gives isolation (the feature), and fixes the prototype's "lost the original logs" defect
via an explicit origin link.

## Data-model sketch (option C)

- `corpora`: add nullable `parent_corpus_id INT REFERENCES corpora(id)` and `selection JSONB`
  (the message condition: `{severity, category, what, …}`) — non-null only for sandbox corpora.
- `tasks`: add nullable `origin_task_id BIGINT REFERENCES tasks(id)` — a sandbox task points at the
  parent task it was carved from (preserves the original-log connection without copying logs).
- **Source files:** the sandbox references the parent's entry files in place (shared path, no copy);
  only *outputs* are sandbox-local. **As-built mechanism (resolved):** path indirection in
  `helpers::result_archive_path` — a sandbox's archives are name-scoped `…/<service>.sandbox-<id>.zip`,
  no symlinks. (The sketched `tasks.origin_task_id` link was **not** built: the stored `selection`
  predicate is the sandbox's provenance, so no per-task origin link is kept.)

## API (symmetry contract)

- `POST /api/corpora/<parent>/sandbox` with a body `{ name, selection: { severity, category, what } }`
  → spawns a **`sandbox_create` job** (queries the parent's `log_*` for the condition → builds the
  sandbox corpus + linked tasks) → `202` + a job handle to poll. Mirrors today's `POST /api/corpora`
  import flow.
- The sandbox then behaves like any corpus: `GET /api/corpora/<name>`, run/rerun, history — all the
  writeable actions, scoped naturally by its own `corpus_id`.

## Open questions for the owner

1. **Output contamination:** confirm sandbox outputs should be **isolated** (own result tree) —
   treating contamination-avoidance as the intended feature (recommended) — vs sharing the parent's
   outputs.
2. **Source files:** symlink the parent's entry files into a sandbox dir, or reference them in place
   (no second filesystem tree)? In-place is cleaner if the dispatcher can stream from the parent path.
3. **Selection freshness:** is the sandbox a one-time snapshot of the condition at creation
   (recommended — stable to iterate on), or a live view that re-queries as the parent changes?
4. **Origin logs:** is the `origin_task_id` link enough to surface "why it was selected," or should
   the selecting messages be copied into the sandbox at creation for a self-contained history?

## Relationship to existing work

- **Background jobs** (`JOB_MODEL.md`): sandbox creation is just another job kind — the mechanism is
  already built.
- **`examples/sandbox_arxiv.rs`**: the ID-list precursor; the message-condition version generalizes
  it and is the productized replacement.
- **Reports / rollups** (`RESOURCE_RATIONALIZATION.md` #6): the selection uses the same
  `(severity, category, what)` dimensions as the reports, so the filter UI/API can reuse them.
- **Data management** (Arm 10): a sandbox is a close cousin of a dataset slice; they may share the
  selection model.
