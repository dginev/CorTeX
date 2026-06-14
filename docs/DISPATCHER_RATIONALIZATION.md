# Dispatcher rationalization — toward lock-free, fanned-out, fearless concurrency

Owner directive (2026-06-14): *"we could greatly benefit from rationalizing our dispatcher business
logic. It is not asynchronous enough and not fanned out enough — we need to aspire for a lock-free
design and fearless concurrency."*

## Status (2026-06-14)

- **Transport: DECIDED → pure-Rust `zeromq` (zmq.rs).** The owner green-lit the switch, conditional on
  a resilience spike proving production-readiness; the condition is met (see *Validation evidence*).
- **Phase 0 (de-risking spikes): COMPLETE** — five throwaway spikes in `examples/zmq_*` (dev-deps
  only; the production hot path is untouched) settle feature coverage, throughput, ZMTP interop,
  resilience, and a five-stressor torture test. All green.
- **Hot-path implementation: PHASE 1 LANDED** (2026-06-14, owner: "go on dispatcher phase 1"). The
  done queue is now a **bounded channel** (`std::sync::mpsc::sync_channel`, capacity
  `DONE_QUEUE_CAPACITY`) instead of `Arc<Mutex<Vec<TaskReport>>>` + the `DONE_QUEUE_HARD_LIMIT` panic:
  the sink + ventilator-reaper `send` (cloned senders), the finalize thread owns the receiver and is
  now **event-driven** (`recv_timeout(1s)` + `try_recv` batch-drain) rather than a 1s poll. A full
  channel **blocks** the producers (backpressure) instead of OOM-then-panic; nothing is dropped. The
  fail-fast on a DB runaway is preserved (`mark_done_batch` → `Err` → finalize panics → manager
  aborts), as is the `job_limit` semantics (counts drains). **Green:** `echo_roundtrip` passes;
  `bench_pipeline` runs clean at ~8 k tasks/s (1 worker, unsaturated, no hang/panic). Phases 2–4 (DB
  batch tuning, sink fan-out + async I/O, lock-free maps) and the transport swap remain.
- **Residual gate before flipping production traffic:** a **real multi-host network soak** — the one
  property a loopback harness cannot prove. Not a blocker for starting the build.

## Current architecture (as built)

Three long-lived threads spawned by `dispatcher::manager`, sharing state through **three
`Arc<Mutex<…>>`**:

| Shared state | Type | Writers | Readers |
| --- | --- | --- | --- |
| service cache | `Arc<Mutex<HashMap<String, Option<Service>>>>` | sink/vent on cache-miss | every dispatch (`get_service`) |
| in-flight set | `Arc<Mutex<HashMap<i64, TaskProgress>>>` | vent (lease), sink (return), reaper | vent (backpressure size + timeout reap) |
| done queue | `Arc<Mutex<Vec<TaskReport>>>` | **sink** (per result) | **finalize** (drains to DB) |

- **Ventilator** (ZMQ ROUTER, :51695): leases TODO tasks (marking them `Queued` in the DB), streams
  sources, inserts into the in-flight set; backpressure stops leasing once the set ≥ `max_in_flight`
  (D-6, done). On startup it **resets orphaned `Queued` tasks back to `TODO`** (crash recovery) except
  those currently in-flight. Restart-looped + joined.
- **Sink** (ZMQ PULL, :51696): per result it **writes the result archive to `/data` inline** (a
  blocking QLC-RAID6 write — D-7), parses `cortex.log`, then locks the done queue and pushes. Single
  thread.
- **Finalize**: drains the done queue under the lock, persists to Postgres (idempotent — batched
  status `UPDATE`s + `on_conflict_do_nothing` for logs), refreshes the rollup.
- **worker_metadata writer**: already a **bounded `sync_channel` + single writer** (D-1) — the one
  piece already message-passing rather than per-event lock/thread.
- Supervision: the manager polls `sink/finalize.is_finished()` every 1s and `join()`s the ventilator;
  any death → `Err(ETERM)` → process abort → external restart (the intended fail-fast).

## Where it is "not asynchronous / not fanned out / not lock-free"

1. **The sink serializes receive + slow *blocking* disk write + DB-queueing on one thread** (D-7). The
   `/data` write is synchronous; ZMQ PULL backs up behind disk latency. *The single biggest ceiling.*
2. **`Mutex<Vec>` done queue** between sink and finalize: a lock per result + per drain, plus the
   `DONE_QUEUE_HARD_LIMIT` panic backstop standing in for real backpressure. A **bounded channel** *is*
   this hand-off, without the lock or the panic.
3. **`Mutex<HashMap>` in-flight set**: locked on every lease, return, backpressure size-check, and the
   timeout sweep — contended between vent + sink + reaper.
4. **`Mutex<HashMap>` service cache**: locked on every dispatch though it is ~read-only after warmup.
5. **The DB finalize persists per-result, not in batches.** *(Owner: batching "reduces latency
   tremendously".)* The torture test confirms the DB is the true bottleneck — larger batches amortize
   the round-trip + index maintenance + rollup refresh; a channel hand-off makes batching natural.
6. **The ZMQ binding (`zmq` 0.10) is libzmq-FFI, battle-proven but slow-maintained.** The owner wanted
   to escape the C dependency; the pure-Rust `zeromq` crate does (the async wrappers `tmq`/`async-zmq`
   do *not* — they wrap the same libzmq binding). *Settled in favor of `zeromq` — see evidence.*

## Target design (decided)

Lean on Rust's ownership model: **transfer ownership through channels** (no shared mutable state to
lock) and use **lock-free / sharded structures** only where shared state is unavoidable. The compiler
then guarantees data-race freedom — "fearless concurrency" literally.

- **Async core on tokio + pure-Rust `zeromq`.** ROUTER ventilator + PULL sink as tokio tasks; async
  `tokio::fs` for the `/data` archive writes and the ventilator's source reads (closes D-7's blocking
  serialization). `zeromq` is async-native, so this needs no FD-readiness bridging.
- **Done queue → bounded MPSC channel** (`tokio::sync::mpsc`): sink `send`s `TaskReport`s, finalize
  `recv`s. Deletes the mutex *and* the `DONE_QUEUE_HARD_LIMIT` panic — a **bounded** channel *is* the
  backpressure (a full channel makes the producer wait, which is correct, instead of OOM-then-panic).
- **Batched finalize.** Finalize drains *up to N* reports (or a short time-window) off the channel and
  persists them in one multi-row write + one rollup refresh, instead of per-result.
- **Sink fan-out.** The PULL loop does only receive + a cheap hand-off; a pool of async archive-writers
  does the `/data` writes concurrently, then forwards to the finalize channel. Receiving is no longer
  hostage to disk latency.
- **In-flight set → `DashMap` + an `AtomicUsize` counter** for O(1) backpressure (no map lock to read
  the size); the reaper iterates the DashMap without a global lock.
- **Service cache → `DashMap`** (or `arc-swap` for a near-static snapshot): contention-free dispatch.

## Robustness model — already in place; **preserve through the refactor**

The dispatcher is already a textbook **lease / visibility-timeout / dead-letter work queue** with a
durable source of truth. The rationalization is a *throughput* refactor; these invariants are the
contract it must not regress (each is load-bearing and already exists):

| Property | Mechanism (today) | Where |
| --- | --- | --- |
| **Durable source of truth** | leasing marks the DB task `Queued` (positive status); the in-flight map is only a *cache* | `ventilator.rs`, `helpers.rs::TaskStatus` |
| **Crash recovery** | on startup, orphaned `Queued` tasks reset to `TODO` (except those currently in-flight) — a crash mid-batch loses no work | `tasks_aggregate.rs:44`, `ventilator.rs:57` |
| **Visibility timeout** | a lease unreturned past `TaskProgress::expected_at` (**≥1 h**, so slow latexml conversions are never re-leased) is reaped → re-queued to *its own* service | `ventilator.rs:15`, `server.rs::timeout_progress_tasks` |
| **Dead-letter / poison-task bound** | re-queue carries a **retry budget**; an exhausted task is *not* re-queued — it dead-letters (`ExpiredOutcome::Fatal`), so one hostile arXiv paper can't cycle the fleet forever | `server.rs:300-333` |
| **Idempotent persistence** | finalize is batched status `UPDATE`s + `on_conflict_do_nothing` for logs, so a re-leased/duplicate result persists cleanly (exactly-once *effect* from at-least-once delivery) | `mark.rs:25,81` |
| **Backpressure (no unbounded growth)** | `max_in_flight` caps leasing; the bounded channel (replacing the `*_HARD_LIMIT` panics) blocks rather than drops | `server.rs`, `manager.rs` |
| **No silent loss** | a dropped result loses work, so the channels are **bounded-block, never drop** (unlike best-effort `worker_metadata`) | design rule |
| **Fail-fast on poisoned state** | mutex-poison `.expect()`s + thread-death supervision → abort → external restart (deliberate; *do not* convert to silent recovery) | `manager.rs`, CLAUDE.md |

**Batching is crash-safe** precisely because of rows 1–2: a result sits in the finalize batch only
*after* the DB task is already `Queued`; the terminal status is written when the batch flushes, so a
crash mid-batch leaves the task `Queued` → the startup reset re-queues it. The in-flight cache is
cleared on *receipt* (not on persist), so a slow DB never triggers spurious re-leases. The torture
test exercised exactly this: a DB stalling ≤15 s behind a bounded channel, every task persisted
**exactly once** with duplicates deduped.

**The one thing `zeromq` lacks vs. libzmq — ZMTP heartbeats (PING/PONG)** — does **not** dent this
model: a silent/half-open worker is recovered by the **visibility-timeout reaper** regardless of
keepalive. Heartbeats would only shorten *detection latency* for power-cut hosts; the ≥1 h reaper is
the correctness net. (zeromq *does* detect graceful disconnects → a ROUTER `send` to a vanished peer
returns `Err`, which the ventilator turns into an immediate re-lease.)

## Failure modes & supervised shutdown (catastrophic deaths)

Owner requirement (2026-06-14): *"examine unexpected deaths in our async dispatcher pieces — if the DB
refuses connection and the finalize arm dies, we must not get an inconsistent state — better to try a
recovery and if impossible stop the entire dispatcher. Similarly … full disk / disk death … a network
firewall prevents communication in one direction … inexplicable packet loss."*

The governing rule (already CLAUDE.md doctrine for the thread design, and **the single biggest risk of
the tokio migration**): **a fatal in any one arm must, after a bounded recovery attempt, bring down the
*whole* dispatcher — never leave a zombie arm.** In the thread design the manager enforces this
(`join`/`is_finished` → `Err(ETERM)` → process abort → external restart). In tokio it is *easy to get
wrong*: a dropped `JoinHandle`, a swallowed `JoinError`, or a task that returns `Err` and nobody checks
leaves the ventilator happily leasing into a dead pipeline. So the async core **must** carry an
explicit supervisor.

**Supervisor pattern (required for the async core):** spawn the arms into a `JoinSet` (or tracked
handles); the main loop `select!`s over "all done" vs. "any arm exited". *Any* arm finishing —
normally, by panic, or by returning `Err` — trips a shared **halt** signal (a `CancellationToken` /
`watch`), every arm observes it and stops, then the process exits non-zero for the external supervisor
to restart. Recovery is a **bounded-retry wrapper** *inside* an arm (reconnect the DB / retry the
write N times with backoff); only an *exhausted* budget escalates to the halt. This is exactly the
thread design's semantics, made explicit in tokio.

| Catastrophe | Recover first (bounded) | Then, if impossible | Why no inconsistency |
| --- | --- | --- | --- |
| **DB refuses connection / finalize dies** | reconnect with backoff (today: `connection_at` 5× + the finalize 2 s retry) | trip halt → stop **all** arms → abort → restart | A batch is committed (`Queued`→terminal) atomically; a failed commit leaves the tasks `Queued`, so the startup reset re-queues them. **Nothing is acked-but-lost.** |
| **Disk full / disk death (I/O)** | retry the `/data` write N× | trip halt → stop all → abort | The result is not removed from in-flight / its task not marked terminal until the write+persist succeed; on halt the task stays `Queued` → recoverable. |
| **Firewall blocks one direction / silent transport** | n/a (can't recover a blocked link) | a **progress watchdog**: no task finalized for *T* while in-flight > 0 → trip halt | The dispatcher stalls *visibly* (watchdog fires) rather than spinning forever; in-flight tasks stay `Queued`. |
| **Inexplicable packet loss** | the lease/visibility-timeout reaper re-leases dropped work; workers retry dropped requests | (only escalates if loss is total → caught by the watchdog) | Lost results → task stays `Queued` past the timeout → reaped → re-leased. Already covered by the churn/torture spikes. |

**New requirement surfaced:** a **progress watchdog** — the existing per-task ≥1 h timeout recovers a
*single* dead worker, but a *one-directional transport failure* (or a wedged finalize) means **no**
task makes progress while in-flight stays pinned; that needs a separate liveness check that trips the
halt. Add it as part of the supervisor.

**Validated empirically** by `examples/zmq_faults.rs` (below): inject DB-death, disk-full, and a
one-directional transport block; confirm each either *recovers* (transient) or *halts every arm* with a
single clear reason and a **consistent durable state** (no task both acked and unpersisted).

## Memory discipline — a light dispatcher co-resident with workers

Owner requirement (2026-06-14): *"discipline in memory use — deallocations of RAM tightly after use,
buffered streaming for enormous items … max 300 concurrent jobs … the dispatcher runs while the
workers also use the machine, so be light on RAM, likely at most 32 GB."*

The dispatcher shares the box with ~200–300 worker processes, so its own footprint must be a **small,
*bounded* slice of the 32 GB** — single-digit GB, leaving the bulk for workers + Postgres + OS. The one
decision that governs this is: **do we ever hold a whole archive resident per in-flight job, or do we
stream it in bounded chunks?** With 300 concurrent jobs and a 200 MB tail, the answer is forced.

**Empirical (`examples/dispatcher_memory.rs`, process VmRSS for 300 concurrent jobs):**

| Design | Typical mix (no burst) | 40 concurrent 200 MB giants (8.2 GB of data) | 300 all-giant (58.6 GB of data) |
| --- | --- | --- | --- |
| **Whole archive resident / job** | 0.40 GB | **8.2 GB** | ~58 GB → **OOM** |
| **Chunked streaming, 1 MB** | ~0.2 GB | **0.23 GB** | ~0.3 GB |
| **Chunked streaming, 4 MB** | ~0.5 GB | ~0.5 GB | **1.18 GB** |

The whole-archive design's RSS tracks the *actual* job sizes — fine on the average (0.4 GB) but **8 GB
under a 40-giant burst and OOM under a larger one**: it honors 32 GB only by luck of the workload.
Chunked streaming makes the footprint **flat and independent of job size** — **~1 GB even when all 300
jobs are 200 MB giants** (58.6 GB of underlying data). That is the only design that *guarantees* the
cap rather than hoping the distribution stays benign.

**The rules (required of the rationalized hot path):**

1. **Never hold a whole archive resident.** Stream both directions in bounded chunks: the ventilator
   reads `/data` chunk-by-chunk and sends; the sink writes each received chunk straight to `/data` and
   drops it. Per-job footprint is **O(chunk)**, not O(archive). *(Caveat: a single multi-frame `zeromq`
   message reassembles the whole archive in memory before `recv()` returns — so genuinely-huge
   archives must be sent as a **sequence of bounded chunk-messages**, not one giant multipart message.
   The torture spike used one-message-per-archive, which is fine up to a few MB but not for the 200 MB
   tail — chunk those.)*
2. **Tight deallocation.** Chunk buffers are *moved* (`Bytes`/ownership), used, and dropped at end of
   scope — no accumulation. The **finalize channel carries metadata only** (task id, status, parsed-log
   digest, `/data` path) — *never* the archive bytes; those are already on disk.
3. **Bound the ZMQ high-water-marks** (`SNDHWM`/`RCVHWM`) so the socket layer can't buffer more than a
   fixed number of in-flight chunk-messages per socket — end-to-end backpressure with a fixed ceiling.
4. **Byte-aware admission control (hard backstop).** Alongside `max_in_flight` (300 jobs), track an
   estimated in-flight-bytes budget and pause leasing if a new lease would exceed it — so even a
   pathological burst can't exceed the cap, regardless of streaming.

**Budget (300 jobs, 1 MB chunks, a few buffers/job + bounded HWM):** job-data RSS ≈ **0.2–1 GB**; with
4 MB chunks ≈ **~1 GB worst-case**. Other consumers are negligible by comparison: the r2d2 Postgres
pool (a handful of connections), the in-flight `DashMap` (300 × a small `TaskProgress` ≈ KB), the
finalize batch (≈ a few hundred metadata `TaskReport`s ≈ MB), the service cache (KB). **Net: a few GB
peak, well under 32 GB, flat against the 200 MB tail** — leaving ≥28 GB for the co-resident workers.
*Recommend a `chunk_bytes` (default 1 MB) and an `inflight_bytes_budget` config knob.*

## Validation evidence (phase 0 — all spikes green)

Five throwaway spikes (`examples/zmq_*.rs`, dev-deps only). Workloads are parameterizable; numbers are
release-build, loopback.

| Spike | Question | Result |
| --- | --- | --- |
| `zmq_payload_{zeromq,libzmq}` | large-multipart integrity + throughput, A/B | both clean on 7.7 MB / 60-frame msgs under 8 senders; `zeromq` ≈ **90 %** of libzmq (both GB/s, ≫ the ~100 tasks/s prod rate) |
| `zmq_arxiv_workload` | full topology (ROUTER+DEALER+PUSH+PULL) under heavy-tailed load | **4298 tasks/s** @200 workers, **zero** interleaving/reorder/misrouting over 20 k tasks |
| `zmq_interop` | does a `zeromq` dispatcher talk ZMTP to **libzmq** workers? | **YES** — 3033 tasks/s @200 libzmq workers, clean → migration is dispatcher-first + reversible |
| `zmq_resilience` | worker churn (crash / request-then-die / reconnect) | **every task recovered, zero loss** even at 80 % flaky / 40 killed + 41 reconnects |
| `zmq_torture` | all 5 owner stressors at once (below) | **2000/2000 persisted exactly once, zero anomalies** |
| `zmq_faults` | catastrophic deaths (DB-dead, disk-full, one-way transport block) | transient faults **recover**; persistent faults **halt every arm** with one reason + **consistent durable state** (no task acked-but-unpersisted) |
| `dispatcher_memory` | RAM footprint at 300 jobs (whole-archive vs chunked) | whole-archive **8 GB under a 40-giant burst → OOM**; chunked streaming **~1 GB even with 300×200 MB jobs** (flat, independent of size) |

**Feature coverage — complete for our usage.** Confirmed from `src/`: we use `ROUTER` (ventilator),
`DEALER` (worker source), `PUSH` (worker sink), `PULL` (dispatcher sink) over TCP, multi-frame. The
`zeromq` 0.6 source implements all four + TCP/IPC + multipart. What it omits (PAIR, `inproc`, CURVE) we
don't use — CorTeX's ZMQ is internal; the web tier is guarded by Anubis + the perimeter.

**Torture profile** (`zmq_torture`, per owner spec): log-normal job sizes **median 800 KB / mean
~1.5 MB**, clamped [500 KB, 200 MB] with a giant-injector for the 50–200 MB tail (256 KB frames ⇒ a
200 MB job is an ~800-frame message); flaky network (random disconnect→reconnect); hundreds of
cross-talking consumers (every frame stamped + re-verified); timeout-flaky sleepers (intended
10 s–45 min, blowing past the lease); and a **batch finalize stalling ≤15 s** behind a **bounded**
channel. At 250 consumers / 2000 tasks: realized `min 500 KB · p50 867 KB · max 150 MB`; **2000/2000
persisted exactly once, zero integrity anomalies**, with the mock DB the bottleneck (51/s = the
backpressure the design absorbs). No hang/OOM/panic. *(The torture spike already exercises the
phase-1→3 shape — bounded channel + batched, latency-stalled finalize — end-to-end.)*

## Incremental migration plan (each phase = one reviewable PR; green on `echo_roundtrip` + `bench_pipeline`)

Each phase **must preserve every invariant in the robustness table** and is independently shippable.
Phases 1–4 are transport-independent; the transport swap (phase 5) is a separable layer the spikes
de-risk.

1. **Done queue → bounded channel. ✅ DONE (2026-06-14).** Replaced `Arc<Mutex<Vec<TaskReport>>>` +
   `DONE_QUEUE_HARD_LIMIT` with a bounded `sync_channel` (`DONE_QUEUE_CAPACITY`). Sink + ventilator-
   reaper `send` (cloned senders); finalize owns the receiver, event-driven via `recv_timeout(1s)` +
   `try_recv` batch-drain. Backpressure = a full channel blocks (no drop, no panic). Green on
   `echo_roundtrip` + `bench_pipeline` (~8 k tasks/s, 1 worker). `server.rs`/`finalize.rs`/`sink.rs`/
   `ventilator.rs`/`manager.rs`.
2. **DB finalize batching.** Drain up to N (or a time-window) per flush → one multi-row write + one
   rollup refresh. Knob `finalize_batch`. *Time-bound flush too, so idle periods still persist
   promptly.* Keep `Queued`-until-flush + `on_conflict` (crash-safe + idempotent).
3. **Sink writer fan-out + async file I/O (closes D-7).** Receive loop + pool of async `/data` writers;
   ventilator source reads go async too. Receiving no longer hostage to disk latency.
4. **In-flight set → DashMap + AtomicUsize; service cache → DashMap/arc-swap.** Backpressure reads the
   atomic; the reaper iterates the DashMap; dispatch lookups contention-free.
5. **Transport swap → tokio + `zeromq`.** ROUTER/PULL as async tasks. Workers stay on libzmq
   (interop-proven); a later, optional phase migrates `worker.rs` + `pericortex` to finish removing the
   C dependency.

**Cross-cutting requirement (every phase): observability.** Emit `tracing` spans + `metrics` for the
new pipeline — in-flight gauge, **finalize-channel depth (backpressure/lag)**, batch size + latency
histogram, re-lease + dead-letter counters. These are the health signals that tell an operator the DB
is the bottleneck *before* it backs up (and feed the `/metrics` endpoint already shipped).

## Crates ("prefer the foundations")

- **Transport:** `zeromq` 0.6 (pure-Rust async; escapes libzmq). `tmq`/`async-zmq` rejected (wrap
  libzmq → don't address maintenance).
- `tokio` — runtime, `tokio::fs`, `tokio::sync::mpsc`.
- `dashmap` — sharded concurrent maps (in-flight set, service cache). `std::sync::atomic` — counters.

## Modern-best-practices audit

| Quality | How the (decided) design achieves it | Status |
| --- | --- | --- |
| Fearless concurrency | ownership transfer via channels; lock-free DashMap/atomics only where shared | ✅ planned |
| Async, non-blocking I/O | tokio core, `tokio::fs` for `/data`, async ZMQ | ✅ planned (phase 3,5) |
| Backpressure, bounded resources | `max_in_flight` + bounded channels (block, never drop); no unbounded per-event acquisition | ✅ (today + plan) |
| At-least-once + idempotent ⇒ exactly-once effect | `Queued` durable mark + `on_conflict` finalize + dedup | ✅ today |
| Crash consistency | startup `Queued`→`TODO` reset; batch flush is the commit point | ✅ today |
| Poison-task containment | retry budget → dead-letter | ✅ today |
| Transparent failure | fail-fast on poisoned state → abort → restart; everything else logged + counted | ✅ today |
| Observability | tracing + metrics on every lifecycle transition; backpressure/lag visible | 🟡 **required by the refactor** |
| Memory discipline | chunked streaming + tight dealloc + bounded HWM + byte-admission ⇒ flat ~1 GB at 300 jobs, independent of the 200 MB tail | 🟡 **required by the refactor** (empirically scoped) |
| Dependency hygiene | escape the unmaintained libzmq C FFI → pure-Rust async; **archive crate under review** (libarchive-sys git dep) | 🟡 zeromq decided; archive crate evaluating |
| Graceful shutdown | *intentional fail-fast* (abort) — optional SIGTERM drain of the finalize batch is a nicety, not needed (crash recovery covers it) | ⚪ optional |

**Verdict:** the resilience qualities are largely **already maximized** by the existing
lease/visibility-timeout/dead-letter/crash-recovery machinery; the rationalization adds the *throughput*
qualities (lock-free, async, fanned-out, batched) **without** regressing them. The only must-add is
**observability for the new pipeline** (so backpressure/lag is legible); the only must-not-regress is
the robustness table.

## Open questions for the owner

1. **`dashmap` dependency** — acceptable for the in-flight set + service cache? (Alternative: a
   hand-sharded `Mutex<HashMap>` — more code, no new dep.) *Needed at phase 4.*
2. **Config defaults** — propose: `finalize_batch` ≈ a few hundred **with a ≤250 ms time-bound flush**
   (so low traffic still persists promptly); sink writer-pool ≈ host cores. `max_in_flight` and the
   ≥1 h visibility timeout already exist as knobs — keep as-is? *Phases 2–3.*
3. **Worker migration** — after the dispatcher moves to `zeromq`, do we *also* migrate `worker.rs` +
   `pericortex` off libzmq (finishing the C-dependency removal), or stay hybrid indefinitely? Interop
   makes hybrid viable forever; full removal is a later, separable phase. Strategic call.
4. **Real-network soak** — where do we run the pre-cutover soak (a staging deployment with real workers
   across hosts, with induced packet loss / partitions)? This is the last gate before prod traffic.
5. **Start phase 1?** — the transport question the "hold for review" protected is settled; phase 1
   (done-queue → bounded channel) is transport-independent, the smallest step, and already validated in
   shape by the torture spike. Green light to begin?

## Risk & validation

- `echo_roundtrip` (full vent→sink→finalize round-trip) + `examples/bench_pipeline.rs` (the A/B harness
  that proved the D-1 connection-storm) gate every phase: **stay green + don't regress the
  dispatched/returned rate.** The new `zmq_*` spikes add transport + resilience + torture coverage.
- Bench gotchas (`productize-2026-sprint` memory): run the in-process sampler foreground (sandbox
  seccomp kills backgrounded signal-using runs); chunk inserts under PG's 65535 bind-param cap; the
  `job_limit` lockstep path (D-5) can hang → time-box.
- The loopback spikes prove correctness-in-principle + ZMTP interop; the **real multi-host network
  soak** remains the final gate before flipping production traffic.
