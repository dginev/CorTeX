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
  `bench_pipeline` runs clean at ~8 k tasks/s (1 worker, unsaturated, no hang/panic).
- **Phase 2 LANDED** (2026-06-14): DB finalize **batching** (`finalize_batch_size` N /
  `finalize_flush_ms` T — one `mark_done` transaction per coalesced batch).
- **Phase 3 LANDED** (2026-06-14, owner: std-thread writer pool — OPEN_QUESTIONS #12 option a):
  **sink writer fan-out**, closing **D-7**. The blocking `/data` write + `cortex.log` parse moved off
  the receive loop to a pool of `dispatcher.sink_writers` (default 4) std-thread writers. Gated green on
  `dispatcher_torture_test` (byte-exact integrity + cap), `echo_roundtrip`, and the 8-worker bench (no
  loss, throughput-neutral on loopback). See phase 3 in the migration plan below.
- **Phase 4 LANDED** (2026-06-14, owner approved `dashmap` — OPEN_QUESTIONS #13): **lock-free maps** —
  the in-flight set (`server::InFlightSet` = sharded `DashMap` + `AtomicUsize` size counter) and the
  service cache (`server::ServiceCache` = `DashMap`) replace the two `Arc<Mutex<HashMap>>`. Built
  red/green TDD (200-concurrent-task unit test); throughput-neutral within noise (the map was never the
  wall — the DB is). See phase 4 below.
- **Leveled logging + keepalive LANDED** (2026-06-15): the dispatcher's per-event `eprintln!`/`println!`
  became leveled `tracing` (Arm 8, `cortex::observability`; hot-path narration is `trace`/`debug`, off
  at the default `info` level → no synchronous stderr write per dispatched task in production; closes
  **D-11** alongside the existing rate-limited discard logs). Separately, **TCP keepalive** was added to
  the worker-facing ROUTER/PULL sockets (`dispatcher.tcp_keepalive_idle_seconds`, default 120) so idle
  remote-worker connections survive NAT/overlay idle-timeouts (`server::apply_tcp_keepalive`).
- **Remaining:** phase 5 (tokio + pure-Rust `zeromq` transport, carrying the deferred `tokio::fs` async
  file I/O) — still owner-gated on the tokio async core.
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

1. **The sink serializes receive + slow *blocking* disk write + DB-queueing on one thread** (D-7).
   ✅ **FIXED — phase 3 (2026-06-14):** a std-thread writer pool now does the `/data` write + parse off
   the receive loop, so ZMQ PULL no longer backs up behind disk latency. *(Was: the single biggest
   ceiling.)*
2. **`Mutex<Vec>` done queue** between sink and finalize: a lock per result + per drain, plus the
   `DONE_QUEUE_HARD_LIMIT` panic backstop standing in for real backpressure. A **bounded channel** *is*
   this hand-off, without the lock or the panic.
3. **`Mutex<HashMap>` in-flight set**: locked on every lease, return, backpressure size-check, and the
   timeout sweep — contended between vent + sink + reaper. ✅ **FIXED — phase 4 (2026-06-14):** now
   `InFlightSet` = a sharded `DashMap` + an `AtomicUsize` size counter (O(1) backpressure read), no
   global lock.
4. **`Mutex<HashMap>` service cache**: locked on every dispatch though it is ~read-only after warmup.
   ✅ **FIXED — phase 4 (2026-06-14):** now `ServiceCache` = a `DashMap`, so a dispatch lookup never
   waits on a whole-map lock.
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
| **Visibility timeout** | a lease unreturned past `TaskProgress::expected_at` (`(retries+1) × dispatcher.lease_timeout_seconds`, default **1 h** so slow latexml conversions are never re-leased) is reaped → re-queued to *its own* service. Timeout + sweep cadence (`reap_interval_seconds`, default 60 s) are now **runtime knobs** (2026-06-14), which also unlock the bench's `BENCH_CHAOS` recovery gate. | `helpers.rs::expected_at`, `server.rs::timeout_progress_tasks` |
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

**Re-validated 2026-06-15** (release, loopback, current tree — confirming the transport choice still
holds before authorizing phase 5): the three decisive spikes reproduce. `zmq_payload_zeromq`
**1000/1000 byte-clean, 3951 MB/s, async-`tokio::fs` write ok**; `zmq_payload_libzmq` **1000/1000
clean, 4689 MB/s** (so `zeromq` ≈ **84 %** of libzmq, both ~4 GB/s ≫ the ~100–200 tasks/s prod rate);
`zmq_interop` (zeromq ROUTER/PULL ↔ 20 libzmq DEALER/PUSH workers) **2000/2000 clean, 2996 tasks/s, no
interleaving/reorder/misrouting/loss** — confirming the swap is dispatcher-first + reversible.

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
2. **DB finalize batching. ✅ DONE (2026-06-14).** The finalize thread now blocks for the first report,
   then **accumulates** more (`accumulate_batch`) until **N** reports (`finalize_batch_size`, default
   **1024**) **or** **T** ms elapse (`finalize_flush_ms`, default **300**) — whichever first — and
   persists the whole batch in one `mark_done` transaction + one rollup refresh. N/T derivation
   (owner's flush-knob question): **T** from the acceptable crash *re-work* + report-staleness budget
   (an unflushed batch is never *lost* — tasks stay `Queued` and recover on restart — so this trades a
   little latency, not safety); **N** is the empirical throughput knee from `dispatcher_bench`
   (tasks/s climbs to ~1024 then plateaus and *regresses* by 4096; see `docs/DISPATCHER_BENCH.md`).
   Keeps `Queued`-until-flush + `on_conflict` (crash-safe + idempotent), `job_limit` (counts batches),
   and the refresh-on-drain/-at-least-daily cadence. Pure size-vs-time logic unit-tested. Green on
   `dispatcher_bench` (20000 tasks, no loss, all `NoProblem`). `finalize.rs`/`config.rs`.
3. **Sink writer fan-out (closes D-7). ✅ DONE (2026-06-14, owner chose the std-thread pool —
   OPEN_QUESTIONS #12 option a).** The sink is now a receive loop + a pool of `dispatcher.sink_writers`
   (default **4**) std-thread archive-writers, each fed a bounded per-writer command channel. The
   receive loop owns the ZMQ-PULL socket and streams each result to one writer round-robin
   (`Begin{task,path} → Chunk* → Commit|Reject`); the writer does the blocking `/data` write +
   `generate_report` + finalize hand-off — so receiving is no longer hostage to disk latency. Per-task
   ordering holds (a task's frames go contiguously to one writer's FIFO); fan-out is across *different*
   tasks; memory stays O(chunk) (chunks streamed + dropped, never the whole archive resident, bounded by
   the per-writer channel). Every receive-side invariant is preserved — the `[identity, service, taskid,
   …data]` RCVMORE envelope hardening (D-4/D-12), the `max_result_bytes` cap + frame-drain (W-1③), the
   rate-limited discard logging (D-11), the metadata enqueue. Fail-fast preserved: a writer death is
   caught by the receive loop (per-iteration `is_finished` sweep + send error) → `Err` → manager abort →
   supervised restart; a crash mid-write leaves the task `Queued` for the reaper (no loss). Also fixed
   two latent socket-desync bugs of the old inline path (an early `continue` on `File::create` failure /
   a path-derivation failure used to skip draining the data frames). **Green:** `dispatcher_torture_test`
   (byte-exact integrity + cap accept/reject under the concurrent malformed sink+vent floods),
   `echo_roundtrip`, `dispatcher_bench` (8-worker / 20000 tasks: no loss, all terminal, throughput-
   neutral vs the inline baseline on loopback — 8940 vs 8932 tasks/s; the disk-decoupling win is on the
   *slow* `/data`, which loopback's page-cache writes can't show). `sink.rs`/`config.rs`. **Deferred to
   phase 5:** the `tokio::fs` *async* file I/O the plan's default envisioned + the ventilator's async
   source reads — natural alongside the tokio core; the std-thread pool already closes D-7's
   blocking-serialization essence without committing to tokio.
4. **In-flight set → DashMap + AtomicUsize; service cache → DashMap. ✅ DONE (2026-06-14, owner approved
   `dashmap` — OPEN_QUESTIONS #13).** The in-flight set is now `server::InFlightSet` (a sharded
   `DashMap<i64, TaskProgress>` + an `AtomicUsize` size counter so backpressure reads the size O(1)
   without locking/scanning); the service cache is `server::ServiceCache` (`DashMap<String,
   Option<Service>>`). The ventilator lease, the sink return, the reaper sweep, and every dispatch
   lookup no longer serialise on one global `Mutex`. The counter is maintained in lock-step with the map
   (its only mutation site — so it converges to the map size; a momentary ±1 skew is harmless for
   backpressure), and the fail-fast `PROGRESS_QUEUE_HARD_LIMIT` backstop is preserved. Built **red/green
   TDD**: the `InFlightSet` unit tests (200 concurrent leases/drains with a consistent counter;
   duplicate-insert / negative-remove edge cases) written first (red), then green. **Empirical finding:**
   throughput-neutral within noise on `dispatcher_bench` 8-worker (median ~8.9k tasks/s; the DB finalize
   ~9k/s is the bottleneck, as predicted — the map was never the wall), so the win is architectural
   (lock-free, O(1) size) and accrues as the DB ceiling lifts / under the phase-5 async core. A new
   200-task end-to-end gate (`tests/concurrent_dispatch_test.rs`) plus `echo_roundtrip` + the torture
   suite stay green. `server.rs`/`ventilator.rs`/`sink.rs`/`manager.rs`/`Cargo.toml`.
5. **Transport swap → tokio + `zeromq`.** ROUTER/PULL as async tasks. Workers stay on libzmq
   (interop-proven); a later, optional phase migrates `worker.rs` + `pericortex` to finish removing the
   C dependency.

**Cross-cutting requirement (every phase): observability.** Emit `tracing` spans + `metrics` for the
new pipeline — in-flight gauge, **finalize-channel depth (backpressure/lag)**, batch size + latency
histogram, re-lease + dead-letter counters. These are the health signals that tell an operator the DB
is the bottleneck *before* it backs up (and feed the `/metrics` endpoint already shipped).

**First signals landed (2026-06-15, transport-independent — they survive the phase-5 swap):** the
finalize loop emits a per-batch `debug` event `finalize: persisted batch {batch, persist_ms,
size_capped, batches_total}` — `size_capped` (`batch.len() >= finalize_batch_size`) is the honest
backpressure/lag proxy, since the std `sync_channel` between sink and finalize can't expose its depth
directly (a full batch was already queued ⇒ the DB finalize is the bottleneck). The ventilator's
reaper emits an `info` event `dispatcher: reaped timed-out in-flight tasks {in_flight, requeued,
dead_lettered}` whenever a reaping pass actually times anything out (the in-flight gauge + re-lease +
dead-letter counts; `reap_expired_into` now returns a `ReapSummary`). Cross-process current-state
backlog is already always-on at `/metrics` (`cortex_workers_in_flight_total`, DB-derived). Still to
do under the tokio core: a finalize-latency histogram + bringing these onto the `metrics` crate if we
adopt it.

## Phase 5 — detailed sub-plan (transport swap → tokio + `zeromq`)

> **Status: planned, awaiting owner sign-off before any hot-path code.** Phases 1–4 + the
> transport-independent observability signals have landed; the spikes (re-validated 2026-06-15) say
> GO. This decomposes the swap into independently-shippable, reversible sub-steps. **Workers stay on
> libzmq throughout** (ZMTP interop proven by `zmq_interop`), so each step is dispatcher-only and a
> bad step reverts by swapping that one socket back — external workers never notice.

**Key simplification the swap *buys* us.** libzmq's `zmq` crate delivers a multipart message
**frame-by-frame** (`recv()` + `get_rcvmore()`), which is the root of the entire D-4/D-12
desync-bug family (recv-then-check reads into the *next* message on an already-complete one). The
pure-Rust `zeromq` crate delivers **a whole `ZmqMessage` (all frames) per `recv()`** — so envelope
parsing becomes "iterate the frames of one message," and the recv-then-check hazard **cannot occur
by construction**. The swap is not just a maintenance/async win; it *retires a bug class*. The
existing RCVMORE-hardening invariants (D-4 request `[identity, service]`, D-12 reply `[identity,
service, taskid, …data]`, W-1③ size-cap frame-drain) become straight-line frame-count checks on the
message's frame vector — keep their **tests** (`dispatcher_torture_test`) as the regression net.

**5a — Async sink (PULL). ✅ DONE (2026-06-15).** The sink's sync `zmq` PULL receive loop is now a
`zeromq::PullSocket` driven on a current-thread tokio runtime owned by the sink thread; the phase-3
std-thread writer pool is **unchanged** (the receive loop still streams each result to a writer over
the existing bounded channel — the blocking sends are the correct backpressure for this single-task
runtime). As predicted, `zeromq`'s atomic whole-message delivery **retired the entire D-4/D-12 desync
class**: the four chained `RCVMORE` guards + the malformed-reply drain loops collapsed into one pure
`parse_reply_envelope` frame-count check (a short reply is just dropped — it can't swallow the next
one). The `max_result_bytes` cap is computed up front from the data-frame sizes (same disk-protection
guarantee — ZMQ buffers whole multipart messages atomically regardless); rate-limited discard
logging, metadata enqueue, per-task FIFO-to-one-writer, and the writer-death fail-fast are preserved.
**`zeromq`/`tokio` graduated to real `[dependencies]`.** *Gates all green:* `echo_roundtrip`,
`concurrent_dispatch_test` (200 tasks / 8 workers, zero loss, byte-exact), `dispatcher_torture_test`
(byte-exact + cap accept/reject under the malformed sink/vent floods), `dispatcher_bench` (20000
tasks, **9840 tasks/s**, no loss — no throughput regression vs the libzmq sink). Five new unit tests
for `parse_reply_envelope`. **Known gap (recorded):** `zeromq` exposes no TCP-keepalive knob, so
`dispatcher.tcp_keepalive_idle_seconds` no longer applies to the sink PULL socket — keepalive was
stability-only (remote-worker NAT mappings) and the lease reaper is the correctness net; revisit if
remote-result NAT drops show up. *Revert:* swap the PULL socket back to `zmq`.

**5b — Async ventilator (ROUTER) + async source reads.** Replace the sync ROUTER recv/send and the
source-archive streaming with `zeromq` ROUTER + `tokio::fs` reads of the source archive. The
lease / backpressure / reap logic is **unchanged** — it operates on the shared `InFlightSet` +
`queues`, which are already lock-free (phase 4) and runtime-agnostic. Preserve: the `[identity,
service]` request framing (now a frame-count check), `max_in_flight` backpressure mock-reply, the
60 s reap cadence + `ReapSummary` health log, `clear_limbo_tasks_except(in_flight)` on start. *Gate:*
same + the ventilator request-flood torture gate. *Revert:* swap the ROUTER socket back.

**5c — Unify runtime, async writers, drop the dispatcher's `zmq` use.** Once both sockets are
`zeromq`: run vent + sink as tasks on one tokio runtime (or keep a runtime per component — see open
Q), move the phase-3 writers to `tokio::fs` (closes the last sync I/O), and remove the `zmq`
dependency from the dispatcher path (`worker.rs` may still use it → keep the crate, just unused by
the dispatcher, or feature-gate). Wire the finalize-latency histogram here.

**5d — (optional, later) migrate `worker.rs` + `pericortex` off libzmq** to delete the C dependency
entirely. Not required for the dispatcher win; sequenced after the dispatcher is stable on `zeromq`.

**The one genuinely-new risk: supervision under tokio.** Today the manager (`manager.rs`) `join()`s
the ventilator thread and polls `sink/finalize.is_finished()` every 1 s → on any component death it
aborts with `ETERM` → external restart (fail-fast, D-3/D-9). Tokio tasks aren't `std::thread`
`JoinHandle`s, so this supervision must be re-expressed: each async component gets a `JoinHandle`
whose completion (or a shared `watch`/`Notify` shutdown signal) trips the same abort. **This is the
part to design carefully and test first** (a deliberately-panicking sink task must still abort the
process), because it's the invariant that turns a dead pipeline into a restart rather than a silent
stall.

**Invariants every sub-step must hold (unchanged from the robustness table):** at-least-once +
idempotent finalize (`Queued` mark + `on_conflict`), crash-consistent restart (`clear_limbo`),
backpressure (`max_in_flight` + bounded channels block-not-drop), `max_result_bytes` cap, envelope
integrity, poison-task dead-lettering, **fail-fast supervision** (the new-risk item above).

**Open decisions for sign-off (recommendations in parens):** (1) start at **5a/async sink**
(*recommended* — smallest, most reversible)? (2) one shared tokio runtime vs a runtime per component
(*recommended: per-component first* — minimises shared-state churn, unify later if it pays)? (3)
keep std-thread writers through 5a/5b and switch to `tokio::fs` only in 5c (*recommended* — smallest
diffs)? (4) confirm the supervision-under-tokio approach (JoinHandle-completion vs shared shutdown
signal) before 5a, since both sockets will eventually depend on it.

## Crates ("prefer the foundations")

- **Transport:** `zeromq` 0.6 (pure-Rust async; escapes libzmq). `tmq`/`async-zmq` rejected (wrap
  libzmq → don't address maintenance).
- `tokio` — runtime, `tokio::fs`, `tokio::sync::mpsc`.
- `dashmap` — sharded concurrent maps (in-flight set, service cache). `std::sync::atomic` — counters.

## Modern-best-practices audit

| Quality | How the (decided) design achieves it | Status |
| --- | --- | --- |
| Fearless concurrency | ownership transfer via channels; lock-free DashMap/atomics only where shared | ✅ done (phases 1,4): done-queue channel; in-flight `DashMap`+`AtomicUsize`; service-cache `DashMap` |
| Async, non-blocking I/O | sink fan-out via std-thread writer pool (✅ phase 3, closes D-7); `tokio::fs` for `/data` + async ZMQ deferred to the tokio core | 🟡 fan-out done; async I/O phase 5 |
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
