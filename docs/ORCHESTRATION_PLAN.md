# Orchestration revision — from first principles

*A planning doc (no code changes here). Revises the **start/stop lifecycle** and the **message-passing /
signaling** between the three parties: the Rocket **frontend**, the ZeroMQ **dispatcher**, and the
external **`pericortex` workers**. Companion to [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md).*

## 1. The irreducible job

Orchestration moves conversion **work** from a durable queue to a fleet of **stateless, untrusted,
failure-prone workers**, collects **results**, persists them, and gives operators **live control**
(start / pause / resume / stop / rerun) — at the deployment's scale (~200 workers, ~100 tasks/s; arXiv
data is hostile and `latexml-oxide` fails unpredictably). Three parties, three relationships:

- **Frontend → Dispatcher** — *what work should run, plus operator control.*
- **Dispatcher ↔ Workers** — *hand out work, take back results.*
- **Dispatcher ↔ Postgres** — *the durable queue + result store (the single source of truth).*

From first principles the design must be **durable** (at-least-once — no lost work), **self-healing**
(crash / worker-death recovers with no operator action), **backpressured** (a slow DB or full queue
throttles, never OOMs), **observable** (every transition visible), and **high-throughput at fleet
scale** (the current ceiling).

## 2. Current design (survey summary)

Three concurrent threads: **ventilator** (ZMQ ROUTER — leases TODO tasks, applies `max_in_flight`
backpressure, streams payloads, reaps lease-timeouts on a cadence), **sink** (ZMQ PULL — receives
result envelopes atomically, fans out to a bounded archive-writer pool, drains the in-flight set),
**finalize** (batches `TaskReport`s into one transaction). The frontend controls runs purely through
the Postgres `tasks` status int (pause→`Blocked`, resume→`TODO`, rerun→atomic mark+delete+`TODO`),
which the dispatcher **polls** on refetch. A lock-free in-flight set + visibility-timeout reaper give
at-least-once delivery; the process is **fail-fast** (any critical thread death / DB runaway / in-flight
hard-cap → panic → systemd restart).

**The core is sound and already hardened** (D-1/4/5/6/7/8/11/12 closed). This revision targets **three
structural gaps**, in priority order.

## 3. Gap 1 — Dispatcher ↔ Worker transport (the throughput ceiling)

**Problem.** The pure-Rust `zeromq` (zmq.rs) path runs every socket as ambient `tokio` tasks; measured
throughput **collapses ~8000 → ~250 tasks/s as peers go 4 → 16+** — the dispatch task is descheduled
~97% of each cycle (tokio scheduler latency when many ready I/O tasks compete). libzmq (dedicated C I/O
threads, no per-message task) holds ~8500/s **flat** at any peer count. At the target fleet (~200
workers) the zmq.rs path cannot meet ~100 tasks/s with headroom.

**First-principles options:**
1. **libzmq for the hot path** (the ventilator already uses it; revert the sink's phase-5a zmq.rs
   swap). Proven flat throughput. Cost: a C dependency + `unsafe` FFI; gives up the pure-Rust goal.
2. **Keep zmq.rs, fix the tokio usage** — isolate socket I/O on a dedicated runtime (or
   thread-per-core / `LocalSet` + `spawn_local`), **batch-drain per wake** (`recv_many`), check out the
   `!Send` DB connection **inside** `spawn_blocking` so it never crosses an await, and tune the
   cooperative-scheduling budget. Ceiling uncertain — a minimal single-task zmq.rs prototype caps
   ~3000/s at 200 peers, hinting at an inherent per-message task overhead.
3. **A different transport** (raw length-prefixed TCP, or `nng`). Larger rewrite.

**Recommendation.** **Benchmark (1) vs (2) head-to-head at 4 / 16 / 64 / 200 simulated peers** before
committing — this is the throughput-critical decision and your active research area. Default lean:
**libzmq on the hot path**, keeping pure-Rust as a stretch goal. **[Owner decision needed.]**

## 4. Gap 2 — Frontend → Dispatcher signaling (control latency)

**Problem.** The dispatcher **polls** the DB for TODO tasks (on a worker request or when a queue
empties). Operator actions (pause, resume, rerun, config change) land in the DB but only take effect on
the next refetch — **polling latency**: a paused run keeps dispatching for up to a refetch; fresh rerun
work waits to be noticed. There is no push path.

**First-principles fix.** Keep the **DB as the single source of truth** (do *not* add a brittle direct
frontend↔dispatcher socket), but add **Postgres `LISTEN/NOTIFY`** as a thin **control wake**: the
frontend `NOTIFY`s a channel on pause / resume / rerun / activate / config-reload; the dispatcher
`LISTEN`s and reacts immediately (stop leasing a paused scope, refetch on new work, hot-reload config).
Durable-by-default — the `tasks` row stays authoritative, `NOTIFY` is only a best-effort wake, so a
missed notify simply falls back to the existing poll. No new dependency, no new attack surface, no
direct coupling.

## 5. Gap 3 — Start/stop + graceful shutdown

**Problem.** No SIGTERM/SIGINT handler. On deploy/stop the dispatcher is hard-killed; in-flight leases
become orphans (recovered later by the reaper / limbo-reset on restart). Correct, but wasteful
(re-converts in-flight work) and noisy.

**First-principles fix (layered — preserves fail-fast):**
- **SIGTERM = graceful drain:** stop leasing new tasks, let in-flight results land (bounded wait ≤ a
  deadline), flush the finalize batch, exit 0. The DB ends consistent; no orphaned leases on a *planned*
  deploy.
- **SIGKILL / panic = fail-fast (unchanged):** mutex poison, thread death, DB runaway, in-flight
  hard-cap → abort → systemd restart. The intentional fail-fast stays for **unexpected** failure;
  graceful drain is only for **planned** stop.
- **Document all three components' lifecycle:** systemd units (frontend, dispatcher) + the external
  worker fleet, start order, restart policy, and the deploy sequence (drain dispatcher → deploy →
  restart).

## 6. Worker protocol — keep + formalize

The **pull / lease / visibility-timeout** model is sound (workers request when ready = natural
backpressure + self-pacing; at-least-once via the reaper). Keep it. Formalize the currently-implicit
**worker identity/liveness**: a lightweight registration + heartbeat so `/workers/<service>` shows real
fleet liveness (not just dispatch tallies), and a worker that dies mid-task is caught by heartbeat as
well as by lease-timeout.

## 7. Phased plan (arms)

| Arm | What | Risk | Why this order |
|---|---|---|---|
| **O-1** | Graceful shutdown (Gap 3) | Low, self-contained | Immediate operational value; no protocol change |
| **O-2** | `LISTEN/NOTIFY` control (Gap 2) | Low — falls back to polling | Kills control latency; incremental + safe |
| **O-3** | Transport benchmark → decision (Gap 1) | High — the hot path | The throughput ceiling; **owner-gated** |
| **O-4** | Worker registration/heartbeat (§6) | Low | Observability; unblocks fleet liveness |

## 8. Open decisions (owner input)

1. **Transport (O-3):** libzmq (proven flat) vs investing in tuned zmq.rs (pure-Rust goal, uncertain
   ceiling)? The crux, and your research area.
2. **Graceful-drain deadline:** how long to wait for in-flight results on SIGTERM before force-exit?
3. **Scope:** full rewrite, or incremental hardening of the (already sound) core? I lean **incremental**
   — the core is good; the gaps are graceful-stop, push-signaling, and the transport ceiling.
