# CorTeX Design Principles

> The product goal is a **battle-hardened, best-in-class distributed processing system** for
> converting noisy scholarly documents at scale. These principles are binding on *every* component
> we design, build, or touch. When a design choice trades simplicity for resilience, prefer
> resilience — and write down the trade.

## 0. North star: maximum robustness

CorTeX runs an adversarial workload. **arXiv's data is extremely noisy** (decades of TeX dialects,
broken packages, missing files, exotic encodings, multi-GB sources, malformed archives), and the
**`latexml-oxide` worker is unpredictable** — it emulates TeX, drives graphics conversion, links
`libxml`, and can fail in an open-ended number of ways (hangs, OOM, segfaults, partial output,
non-UTF-8 logs). On top of the *content* chaos we have every classic distributed-systems hazard:
latency spikes, I/O bottlenecks, resource starvation, lock contention, connection exhaustion,
partial failures, and worker death mid-task.

Therefore the system must be designed for **resilience, fault-tolerance, and transparent failure**
end to end. A failure anywhere — a single poisoned document, a slow disk, a dead worker, an
exhausted connection pool — must degrade *locally and visibly*, never silently corrupt state, drop
work without trace, or take down unrelated components.

## The principles

1. **No unbounded resource acquisition.** Never open a connection / spawn a thread / allocate a
   buffer *per event* on a hot path. Pool and bound everything (DB connections, worker threads,
   in-flight tasks, file handles). Unbounded per-event acquisition is the single most dangerous
   anti-pattern here — it converts load into a self-inflicted DoS. (Proven: the pre-#4
   per-event-connection metadata writer exhausted Postgres `max_connections` + OS ports and crashed
   the process under load — see `RESOURCE_RATIONALIZATION.md` § full-pipeline validation.)

2. **Transparent failure, never silent loss.** Every failure is surfaced — logged with context,
   counted in metrics, and reflected in task/run state an operator *and an agent* can query. A
   dropped metadata write, a skipped task, a truncated result must leave a visible trace. **Banish
   `unwrap()`/`expect()`/`panic!` from request and dispatch paths**; return `Result`, record the
   error, and continue. Panics are for truly-impossible invariants only.

3. **Isolate the blast radius.** One bad document must not stall a queue; one dead worker must not
   wedge the dispatcher; one subsystem (reports, metadata, a single corpus) must not exhaust a
   resource shared by the others. Prefer per-task timeouts, bulkheads, and backpressure over
   best-effort fire-and-forget.

4. **Degrade gracefully.** Under overload, shed or slow work predictably (backpressure, bounded
   queues) rather than collapsing. Optional subsystems (e.g. the report cache) must be *optional* —
   their absence or failure degrades a feature, never the core pipeline.

5. **Idempotent, crash-consistent writes.** Workers die and tasks retry; every write path must be
   safe to repeat. A crash between "result on disk" and "status in DB" must be recoverable on
   restart (re-stage / re-dispatch), never leave silent corruption. `clear_limbo_tasks` is the model;
   extend the pattern everywhere.

6. **Bound and time-box external work.** Treat the worker fleet and the filesystem as hostile: every
   task has a timeout, every result a size cap, every archive a guard against decompression bombs.
   A worker that hangs or floods must be detected and reclaimed (retry budget already exists in the
   ventilator — generalise it).

7. **Observable by construction.** Health, queue depths, throughput, error rates, dropped-event
   counts, and per-worker liveness are first-class, queryable data (the symmetry contract: same DTO
   to humans and agents). You cannot harden what you cannot see.

8. **Measure, then harden.** Resilience claims are validated empirically under realistic load
   (see `bench_pipeline.rs`), not asserted. Each hardening increment ships red/green with a test
   that pins the failure it fixes.

## How this is applied

- Every increment is checked against these principles; deviations are logged in
  [`KNOWN_ISSUES.md`](KNOWN_ISSUES.md) as debt to retire, not ignored.
- `KNOWN_ISSUES.md` is the running ledger of resilience gaps discovered (the owner's direction:
  *record every known problem; we go back and solve them all at the end*).
- The Arm 14 resource-rationalization work (`RESOURCE_RATIONALIZATION.md`) is the first concentrated
  pass at principle #1 and #4.
