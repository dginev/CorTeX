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

1. **The sink serializes receive + slow *blocking* disk write + DB-queueing on one thread.** Each
   result blocks the next on a `/data` QLC-RAID6 write (D-7), and the write is **synchronous** (no
   async file I/O). This is the single biggest throughput ceiling: ZMQ PULL backs up behind disk
   latency. *(Owner: wants async file I/O for both the sink writes and the ventilator source reads.)*
2. **`Mutex<Vec>` done queue** between sink and finalize: a lock on every result (write side) and on
   every drain (read side), plus the `DONE_QUEUE_HARD_LIMIT` panic backstop standing in for real
   backpressure. A channel *is* this hand-off, without the lock or the panic.
3. **`Mutex<HashMap>` in-flight set**: locked on every lease, every return, every backpressure size
   check, and the timeout sweep — contended between vent + sink + reaper.
4. **`Mutex<HashMap>` service cache**: locked on every dispatch though it is ~read-only after warmup.
5. **The DB finalize persists per-result, not in batches.** *(Owner: batching enough results into a
   single multi-row INSERT "reduces latency tremendously".)* Larger batches amortize the round-trip,
   the index maintenance, and the rollup-refresh trigger across many results — a major throughput win
   that a channel hand-off makes natural (the finalize drains *up to N* off the channel, inserts once).
6. **The ZMQ binding (`zmq` 0.10) is libzmq-FFI, battle-proven but slow-maintained**, and shows **rare
   large-response flakiness** — a big result archive streamed as tens of multipart frames can
   interleave/corrupt against other messages. *(Owner: is an async ZMQ crate better maintained?)* See
   the ZMQ-library evaluation below.

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

## ZMQ-library evaluation (owner: maintenance + the large-multipart flakiness)

The owner's two pointed concerns — *is an async ZMQ crate better maintained?* and *the rare large
multi-frame response interleaving/corrupting* — reframe the transport choice. The landscape (verify
versions/activity on crates.io before committing):

| Crate | Kind | Async? | Escapes libzmq FFI? | Notes |
| --- | --- | --- | --- | --- |
| `zmq` 0.10 (current) | libzmq C-FFI binding | sync | no | battle-proven; **slow-maintained**; the large-multipart flakiness lives here/in our framing |
| `tmq` 0.5 / `async-zmq` 0.4 | **tokio/async wrappers over `zmq`** | yes | **no** — still libzmq underneath | async ergonomics, but inherit the *same* binding + its maintenance + (likely) the same multipart behavior |
| **`zeromq` 0.6 (zmq.rs)** | **pure-Rust** reimplementation | **async-native** | **yes** | escapes the C FFI entirely + is async-native (fits async file I/O too); **caveat: less battle-proven than libzmq** — must validate the large-multipart case + perf in a spike |

**Key correction to the earlier framing:** `tmq`/`async-zmq` do **not** solve the maintenance concern
— they wrap the very `zmq` libzmq binding the owner is wary of. The only option that *escapes* it is
the pure-Rust **`zeromq`** crate, which is also async-native. So the owner's "better maintained +
async" goal points at **`zeromq` (zmq.rs)**, not at the async wrappers.

The large-multipart bug is partly a *framing* issue (the sink must reassemble all `RCVMORE` frames of
one message atomically before processing). A new crate may handle it more robustly, but the
application-level reassembly should be made bullet-proof regardless. **A spike is the way to know.**

### Revised recommendation (given the owner's async-I/O + maintenance input)

The earlier doc leaned "A (channel threads) first, B (async) later." The owner's specific asks —
**async file I/O** (needs a runtime) and **escaping the libzmq binding** — now tilt toward a
**tokio-based async core on the pure-Rust `zeromq` crate** (effectively approach **B**, but motivated
by maintenance + async I/O, not uniformity). That is a larger rewrite, so **de-risk it with a spike
first**:

> **Proposed next step — a throwaway spike** (in `examples/`, not touching the production dispatcher):
> a minimal `zeromq`-crate (pure-Rust, async) ROUTER/PULL round-trip that (a) streams a **large
> multi-frame** result and confirms it reassembles without interleaving (the owner's bug), (b) does an
> **async `tokio::fs`** write of the archive, and (c) sanity-benches the dispatched/returned rate vs.
> the current libzmq path. If the spike holds, commit to the tokio + `zeromq` core; if not, fall back
> to **approach A** (channel-pipelined threads over the existing sync `zmq`), which still delivers
> lock-free + fan-out + batching without the transport swap.

Either way, the **lock-free / fan-out / batching** work below is shared — only the socket layer
differs (async `zeromq` tasks vs. channel-pipelined sync threads).

## Incremental migration plan (each phase = one reviewable PR, green on echo_roundtrip + bench)

The **transport-independent** phases (1–4) deliver lock-free + fan-out + batching and ship first; the
**transport** decision (phase 0 spike → async `zeromq` vs. stay sync `zmq`) is settled in parallel and
only changes the socket layer.

0. **Spike the pure-Rust async `zeromq` crate** (`examples/`, throwaway): large-multipart round-trip +
   async `tokio::fs` write + a quick dispatched/returned bench vs. libzmq. Decides the socket layer
   (async `zeromq` core, or stay on sync `zmq` + channel-pipelined threads).
1. **Done queue → bounded channel.** Replace `Arc<Mutex<Vec<TaskReport>>>` + `push_done_queue`/drain
   with a bounded channel. Delete `DONE_QUEUE_HARD_LIMIT`. Finalize loops on `recv`; sink `send`s.
   *Smallest, highest-clarity first step; transport-independent.*
2. **DB finalize batching.** The finalize drains **up to N** reports (or a short time-window) off the
   channel and persists them in **one multi-row INSERT** (then one rollup refresh), instead of
   per-result. Amortizes the round-trip + index + trigger across a batch — the owner's "reduces
   latency tremendously". Tune N as a `DispatcherConfig` knob (`finalize_batch`).
3. **Sink writer fan-out + async file I/O (closes D-7).** Split the sink: a receive loop + a pool of K
   writers fed by a bounded channel doing the **archive write asynchronously** (`tokio::fs` if on the
   async core, else blocking writes on the pool threads); writers forward `TaskReport`s to the
   finalize channel. The ventilator's **source reads** go async likewise. Receiving is no longer
   hostage to disk latency, and I/O fans out across the RAID.
4. **In-flight set → DashMap + AtomicUsize; service cache → DashMap/arc-swap.** Replace the two
   remaining `Mutex<HashMap>`s: backpressure reads the atomic counter, the reaper iterates the
   DashMap, dispatch lookups are contention-free.

## Crates ("prefer the foundations")

- **Transport (phase 0 outcome):** either **`zeromq` 0.6 (zmq.rs, pure-Rust async)** — escapes the
  libzmq FFI + async-native — **or** keep `zmq` 0.10 (sync) if the spike disfavors the pure-Rust impl.
  `tmq`/`async-zmq` are *not* recommended (they wrap libzmq → don't address the maintenance concern).
- `tokio` — the async runtime (if the async core is chosen), incl. `tokio::fs` async file I/O and
  `tokio::sync::mpsc` channels.
- `crossbeam-channel` (or `flume`) — bounded channels for the channel-pipelined (sync) variant.
- `dashmap` — sharded concurrent maps (in-flight set, service cache). `std::sync::atomic` — counters.

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

## Open questions for the owner (please confirm before implementing)

1. **Green-light the phase-0 spike** of the pure-Rust async **`zeromq`** crate (throwaway, in
   `examples/`)? It settles the transport: does it reassemble large multi-frame results without the
   interleaving bug, and is its throughput acceptable vs. libzmq? (`tmq`/`async-zmq` are off the table —
   they wrap the same libzmq binding, so they don't address your maintenance concern.)
2. **If the spike holds → go async core (tokio + `zeromq` + `tokio::fs`)**; if not → channel-pipelined
   threads over the existing sync `zmq`. Agree with letting the spike decide?
3. **`dashmap`** for the in-flight set + service cache — acceptable new dependency? (Alternative: a
   hand-sharded `Mutex<HashMap>` — more code, no new dep.)
4. **Config knobs**: `finalize_batch` (DB batch size) and the sink writer-pool size as
   `DispatcherConfig` knobs (defaulting batch ~ a few hundred, writers ~ host cores)? Consistent with
   the existing dispatcher knobs.

*(Status: holding implementation for your review per the 2026-06-14 directive. The transport-
independent phases 1–4 are ready to start the moment you confirm; phase 0 is the spike.)*
