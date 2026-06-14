# Dispatcher rationalization — toward lock-free, fanned-out, fearless concurrency

Owner directive (2026-06-14): *"we could greatly benefit from rationalizing our dispatcher business
logic. It is not asynchronous enough and not fanned out enough — we need to aspire for a lock-free
design and fearless concurrency."*

This is a **design for review** before any hot-path change — the dispatcher is the throughput-critical
core (~100–200 tasks/s, ~200 ZMQ workers) and CLAUDE.md treats it as carefully-reviewed. It maps the
current design, names the contention/serialization points, and proposes an **incremental,
test-validated** migration. Nothing here is implemented yet; the phases land one at a time, each
green against `echo_roundtrip` + `examples/bench_pipeline.rs`.

## Current architecture (as built)

Three long-lived threads spawned by `dispatcher::manager`, sharing state through **three
`Arc<Mutex<…>>`**:

| Shared state | Type | Writers | Readers |
| --- | --- | --- | --- |
| service cache | `Arc<Mutex<HashMap<String, Option<Service>>>>` | sink/vent on cache-miss | every dispatch (`get_service`) |
| in-flight set | `Arc<Mutex<HashMap<i64, TaskProgress>>>` | vent (lease), sink (return), reaper | vent (backpressure size + timeout reap) |
| done queue | `Arc<Mutex<Vec<TaskReport>>>` | **sink** (per result) | **finalize** (drains to DB) |

- **Ventilator** (ZMQ ROUTER, :51695): leases TODO tasks, streams sources, inserts into the in-flight
  set; backpressure stops leasing once the set ≥ `max_in_flight` (D-6, done). Restart-looped + joined.
- **Sink** (ZMQ PULL, :51696): for each result it **writes the result archive to `/data` inline**
  (`file.write(...)`, a blocking QLC-RAID6 write — D-7), parses `cortex.log`, then locks the done
  queue and pushes. Single thread.
- **Finalize**: drains the done queue under the lock, persists to Postgres, refreshes the rollup.
- **worker_metadata writer**: already a **bounded `sync_channel` + single writer** (D-1) — the one
  piece that is already message-passing rather than per-event lock/thread.
- Supervision: the manager polls `sink/finalize.is_finished()` every 1s and `join()`s the ventilator;
  any death → `Err(ETERM)` → process abort → external restart (the intended fail-fast).

**Already-resolved resilience** (so this work is about *throughput/rationalization*, not crashes):
D-1 (bounded metadata writer), D-2 (metadata upsert race), D-6 (backpressure), D-3 (panics: the
mutex-poison `.expect()`s + the queue HARD_LIMIT `panic!`s are *deliberate* fail-fast; `connection_at`
is retry-hardened; thread deaths are supervised → abort → restart). **D-3 should be marked 🟢 by this
investigation.** **D-7 (single blocking sink writer) folds into this work.**

## Where it is "not asynchronous / not fanned out / not lock-free"

1. **The sink serializes receive + slow disk write + DB-queueing on one thread.** Each result blocks
   the next on a `/data` QLC-RAID6 write (D-7). This is the single biggest throughput ceiling: ZMQ
   PULL backs up behind disk latency.
2. **`Mutex<Vec>` done queue** between sink and finalize: a lock on every result (write side) and on
   every drain (read side), plus the `DONE_QUEUE_HARD_LIMIT` panic backstop standing in for real
   backpressure. A channel *is* this hand-off, without the lock or the panic.
3. **`Mutex<HashMap>` in-flight set**: locked on every lease, every return, every backpressure size
   check, and the timeout sweep — contended between vent + sink + reaper.
4. **`Mutex<HashMap>` service cache**: locked on every dispatch though it is ~read-only after warmup.

## Target shape — fearless concurrency by message-passing + lock-free structures

Lean on Rust's ownership model: **transfer ownership through channels** (no shared mutable state to
lock), and use **lock-free/sharded concurrent structures** only where shared state is unavoidable.
The compiler then guarantees data-race freedom — "fearless concurrency" in the literal sense.

- **Done queue → bounded MPSC channel** (`crossbeam-channel` or `flume`): sink `send`s `TaskReport`s,
  finalize `recv`s. Removes the mutex *and* the `DONE_QUEUE_HARD_LIMIT` panic — a **bounded** channel
  is the backpressure (a full channel makes the sink wait, which is correct, instead of OOM-then-panic).
- **Sink fan-out (D-7)**: the PULL loop does **only** receive + a cheap hand-off; a **pool of
  archive-writer workers** (N threads, fed by a channel) do the blocking `/data` writes in parallel,
  then forward the parsed `TaskReport` to the finalize channel. Receiving is no longer hostage to disk
  latency, and writes fan out across the RAID. (This is the "more fanned out" + "more asynchronous"
  the directive asks for — a pipelined, non-blocking dataflow.)
- **In-flight set → `DashMap<i64, TaskProgress>`** (sharded, lock-free-on-the-hot-path) + an
  **`AtomicUsize` in-flight counter** for the O(1) backpressure check (no map lock to read the size).
  The timeout reaper iterates the DashMap without a global lock.
- **Service cache → `DashMap`** (or `arc-swap` for a near-static snapshot): contention-free dispatch
  lookups.

### "How async?" — the one architecture fork for the owner

Two ways to be "more asynchronous"; they are not exclusive (start with A, consider B later):

- **A. Channel-pipelined threads (recommended first).** Keep the synchronous `zmq` sockets, but make
  the *dataflow* asynchronous/pipelined via channels + a writer pool, and lock-free via DashMap/atomics.
  Achieves lock-free + fan-out + pipelining with **bounded, incremental, well-tested** changes and no
  new runtime. Works with the existing sync `zmq` crate.
- **B. Full async runtime (tokio + `tmq`/`async-zmq`).** Rewrite the vent/sink event loops as async
  tasks awaiting socket readiness, fanning out writes as spawned tasks. More uniform "async", but a
  larger rewrite: swaps the `zmq` crate for an async binding, threads a runtime through the dispatcher,
  and re-validates the whole ZMQ lifecycle. Higher risk; better as a *second* step once A proves out.

**Recommendation: do A incrementally now; revisit B afterwards.** A delivers most of the directive's
benefit (lock-free + fan-out + pipelined) at a fraction of B's risk, and leaves B as a clean follow-on.

## Incremental migration plan (each phase = one reviewable PR, green on echo_roundtrip + bench)

1. **Done queue → bounded channel.** Replace `Arc<Mutex<Vec<TaskReport>>>` + `push_done_queue`/drain
   with a `crossbeam-channel` (bounded). Delete `DONE_QUEUE_HARD_LIMIT`. Finalize loops on `recv`;
   sink `send`s. Supervision: a disconnected channel (a dead peer) is detected directly. *Smallest,
   highest-clarity first step.*
2. **Sink writer fan-out (closes D-7).** Split the sink: a receive loop + a pool of K archive-writer
   threads fed by a bounded channel; writers forward `TaskReport`s to the phase-1 finalize channel.
   Bench the dispatched/returned rate before/after on the production-scale dump.
3. **In-flight set → DashMap + AtomicUsize.** Replace `Arc<Mutex<HashMap<i64, TaskProgress>>>`;
   backpressure reads the atomic counter; the reaper iterates the DashMap. Removes the busiest mutex.
4. **Service cache → DashMap / arc-swap.** Contention-free dispatch lookups.
5. **(Optional, later) Option B**: async ZMQ event loops on tokio, if A leaves headroom worth taking.

## Crates ("prefer the foundations")

- `crossbeam-channel` (or `flume`) — bounded MPSC/SPSC channels (the done-queue + writer-pool feeds).
- `dashmap` — sharded concurrent maps (in-flight set, service cache).
- `std::sync::atomic` — the in-flight counter.
- (Option B only) `tmq` or `async-zmq` + `tokio`.

## Risk & validation

- The dispatcher has `echo_roundtrip` (full vent→sink→finalize round-trip) and
  `examples/bench_pipeline.rs` (the A/B harness that already proved the D-1 connection-storm). **Every
  phase must stay green on the round-trip and not regress the bench's dispatched/returned rate.**
- Bench gotchas (from prior runs, `productize-2026-sprint` memory): run the in-process sampler
  foreground (sandbox seccomp kills backgrounded signal-using runs); chunk inserts under PG's 65535
  bind-param cap; the `job_limit` lockstep path can hang, so time-box.
- Ordering guarantee to preserve: results must still be persisted (the bounded channels must never
  silently drop a `TaskReport` — unlike best-effort metadata, a dropped result loses work). Bounded
  channels **block** the producer rather than drop, which preserves this.

## Open questions for the owner (please confirm before phase 1)

1. **Approach A (channel-pipelined threads) first, B (full tokio/async-zmq) later — agree?**
2. **`crossbeam-channel` vs `flume`** for the channels (both fine; crossbeam is the more standard
   "foundation", flume is a touch faster + simpler API). Default: `crossbeam-channel`.
3. **`dashmap`** for the in-flight set + service cache — acceptable new dependency? (Alternative: keep
   a `Mutex<HashMap>` but shard it by hand — more code, no new dep.)
4. **Sink writer-pool size** — fixed (e.g. `cores`) or a `DispatcherConfig` knob (`sink_writers`,
   defaulting to host cores)? Default: a config knob (consistent with the existing dispatcher knobs).
