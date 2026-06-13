# Background Jobs & Run Handles — design

> Status: **design for sign-off**, not yet implemented. The shared mechanism behind every
> long-running administrative write. Consumed by Arm 5 (corpus import/extend), Arm 6 (service
> activation), Arm 7 (service runs), and Arm 10 (dataset export). Cross-ref:
> [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md).

## The problem

Several admin operations take seconds to hours and **cannot block an HTTP request**:

- **Corpus import** — unpack tarballs + walk the filesystem + insert thousands of tasks (minutes–hours).
- **Service activation** (`register_service`) — delete + recreate every task for a `(corpus,service)`.
- **Dataset export** — bundle archives across a corpus.
- **Service runs / reruns** — kick off a dispatch campaign.

Today these are out-of-band CLI/example binaries with no progress surface. Productized, each must:
**start → return a handle immediately → run in the background → be pollable for progress → reach a
terminal state**, with the *same* surface for humans (a progress page) and agents (poll JSON) — the
symmetry contract.

## The model

A **job** is one persisted row representing one long-running operation.

```sql
CREATE TABLE jobs (
  id               BIGSERIAL PRIMARY KEY,
  uuid             UUID NOT NULL UNIQUE DEFAULT gen_random_uuid(),   -- external handle (D8)
  kind             VARCHAR(50)  NOT NULL,        -- 'corpus_import' | 'service_activate' | 'dataset_export' | …
  status           VARCHAR(20)  NOT NULL DEFAULT 'queued',           -- queued|running|succeeded|failed|interrupted
  progress_current INTEGER      NOT NULL DEFAULT 0,
  progress_total   INTEGER,                       -- NULL when the total is not yet known
  message          TEXT         NOT NULL DEFAULT '',   -- current step, or the error on failure
  actor            VARCHAR(200) NOT NULL DEFAULT '',   -- who started it (Arm 9 identity)
  params           JSONB        NOT NULL DEFAULT '{}', -- inputs (e.g. corpus path)
  result           JSONB,                         -- terminal payload (counts, artifact paths)
  created_at       TIMESTAMP    NOT NULL DEFAULT NOW(),
  updated_at       TIMESTAMP    NOT NULL DEFAULT NOW()
);
CREATE INDEX jobs_status_idx ON jobs(status);
CREATE INDEX jobs_kind_idx   ON jobs(kind);
```

**Lifecycle:** `queued → running → (succeeded | failed)`; `interrupted` is set to any
`queued/running` job on frontend restart (see below). `gen_random_uuid()` is built into PostgreSQL
13+ (we run 18). UUIDs are the first concrete landing of decision **D8** (stable external handles).

**Library shape** (testable, in the lib — handlers stay thin):

```rust
// progress handle passed to the job body; each call persists an UPDATE on the jobs row.
pub struct JobProgress { /* job uuid + pool */ }
impl JobProgress {
  pub fn step(&self, current: i32, total: Option<i32>, message: &str);  // persisted progress
}

// spawn: insert a 'queued' row, return its uuid, run `body` on a background thread.
pub fn spawn_job<F>(pool: DbPool, kind: &str, actor: &str, params: Value, body: F) -> Uuid
where F: FnOnce(&JobProgress) -> Result<Value, String> + Send + 'static;

pub fn find_job(conn, uuid) -> Option<JobDto>;   // poll
```

## The execution decision (the one real fork — needs sign-off)

**Recommended: in-process background threads in the frontend, with the persisted `jobs` table.**
`spawn_job` inserts a `queued` row, spawns a thread that flips it to `running`, runs the body
(reporting progress via `JobProgress`, each step an `UPDATE`), then writes the terminal
`succeeded`/`failed` + `result`. Threads check out from the existing r2d2 pool.

Rationale, grounded in the deployment profile (~2 admins, ~20 users): **admin jobs are infrequent
and few** — a corpus import now and then, not a high-throughput path (that's the 200-worker ZeroMQ
side, which is unaffected). A bounded in-process worker pool is more than enough, and it keeps the
mechanism simple and self-contained in the frontend.

Alternatives considered:
- **Dedicated job-runner daemon** polling the `jobs` table — more robust (survives a frontend
  restart, isolates heavy work) but a whole new process to supervise. Overkill for the job volume.
- **Reuse the ZeroMQ dispatcher** — corpus import *already* runs through the dispatcher via the
  `init` service + `InitWorker`. Tempting for import specifically, but it requires a running
  dispatcher + worker just to register a corpus from the UI, and it doesn't generalize to export.
  Keep the dispatcher for *task processing*; use jobs for *one-shot admin ops*. (We can still let an
  import job internally drive the init path if we want — that's an implementation detail behind the
  job handle.)

**Restart handling:** on frontend boot, `UPDATE jobs SET status='interrupted', message='…' WHERE
status IN ('queued','running')`. Honest and simple; **resume** is deliberately out of scope for the
pilot (most jobs — import especially — are idempotent and can simply be re-run).

## API (symmetry contract)

| Capability action | Agent (JSON) | Human (HTML) |
|---|---|---|
| start a long op | `POST /api/corpora` → **202** + `{ "job": { "uuid", "status", … } }` | form POST → redirect to the job page |
| poll a job | `GET /api/jobs/<uuid>` → `JobDto` | `GET /jobs/<uuid>` → progress page (light jQuery polls the JSON; D11) |
| list jobs | `GET /api/jobs?status=running` | a jobs panel |

`JobDto = { uuid, kind, status, progress: {current, total?}, message, actor, result?, created_at,
updated_at }`. The progress page polls the same JSON the agent reads — one surface, two renderings.

## Relationship to `historical_runs`

- A **run** (`historical_runs`) is a *dispatch campaign* over a `(corpus,service)` — its severity
  tallies fill in as the dispatcher/finalizer completes tasks; it has its own start/end lifecycle
  (Arm 7).
- A **job** is a *one-shot admin operation* with progress + a terminal state (this doc).
- They are distinct but related: starting a run may be wrapped as a job, and Arm 8 (Observatory)
  presents both under one unified live + historical view. We do **not** fold one into the other.

## Open questions for the owner

1. **Execution model:** in-process threads (recommended) vs a dedicated job-runner daemon? (Affects
   robustness vs simplicity.)
2. **Interrupted jobs:** mark `interrupted` and require a manual re-run (recommended), or attempt
   resume? (Resume is much more work and most ops are idempotent.)
3. **Cancellation:** needed in the pilot, or later? (A cooperative cancel flag the body polls.)

## First implementation plan (TDD, once signed off)

1. `uuid` crate + diesel `uuid` feature; `jobs` migration (embedded, Arm 2).
2. `Job`/`NewJob` model + `JobDto` + `spawn_job`/`JobProgress`/`find_job` in a `jobs` library module
   — contract test: a trivial job runs to `succeeded` and reports progress.
3. `GET /api/jobs/<uuid>` + the HTML progress page (D11 polling) — contract tests.
4. Wire the **first consumer**: `POST /api/corpora` (corpus import) returns a job handle; the import
   body reports per-checkpoint progress (the importer already checkpoints every 1000 entries).
5. Reuse for service activation (Arm 6), runs (Arm 7), export (Arm 10).
