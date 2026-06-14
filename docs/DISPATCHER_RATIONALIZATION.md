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

## Phase-0 spike — empirical results (2026-06-14)

Built two throwaway, payload-parameterizable spikes (env knobs `MSG_COUNT`/`SENDERS`/`FRAMES`/
`FRAME_BYTES`/`LARGE_EVERY`) running the **same** workload over each transport — a mix of large
multi-frame messages + small ones, sent by N **concurrent** PUSH senders into one PULL receiver that
verifies every frame of every message carries the right `(seq, frame_index)` header (so any
cross-message contamination = interleaving, any out-of-order frame = reordering is caught):

- `examples/zmq_payload_zeromq.rs` — pure-Rust async **`zeromq`** crate, `tokio` runtime, `tokio::fs`
  archive write.
- `examples/zmq_payload_libzmq.rs` — the current libzmq **`zmq`** crate, threads, `send_multipart`/
  `recv_multipart`.

**Headline (release build, loopback, heavy stress = 3000 msgs · 8 senders · every 2nd msg = 60×128 KB
≈ 7.7 MB, rest 1-frame):**

| Transport | Throughput | Integrity |
| --- | --- | --- |
| libzmq (`zmq`, sync + threads) | 1245 msg/s · **4745 MB/s** | ✓ no interleaving/reordering/corruption over 3000 msgs |
| **`zeromq`** (pure-Rust, tokio async) | 1121 msg/s · **4275 MB/s** | ✓ no interleaving/reordering/corruption over 3000 msgs |

(Dev builds, default + heavy payloads, were likewise clean on both; libzmq ~10–20 % faster on
loopback.)

**What the spike establishes:**

1. **Correctness — the owner's large-multipart bug does NOT reproduce on *either* crate** under heavy
   concurrent senders. Both reassemble 7.7 MB / 60-frame messages atomically with zero interleaving
   across thousands of messages. So the flakiness is **not** a fundamental "this crate can't stream
   large multipart" limit — it points at **application-level framing** (the sink's `RCVMORE`
   reassembly) or a **real-network / libzmq-version edge**, and the reassembly must be made
   bullet-proof *regardless of which crate we pick*. Crucially, the pure-Rust crate is **not
   disqualified** on this axis.
2. **Throughput — not a deciding factor.** Pure-Rust `zeromq` runs at **~90 % of libzmq** (4275 vs
   4745 MB/s). Both are **GB/s on loopback** — wildly over-provisioned vs. the production ~100–200
   tasks/s (≈ a few MB each ⇒ low-single-digit GB/s *peak*, and production is bound by the real
   network + the `/data` disk, not the socket crate). A 10 % crate delta is invisible against that.
3. **Async-nativeness — real, in `zeromq`'s favor.** The `tokio::fs` async archive write dropped
   straight into the `zeromq` receive loop; the libzmq baseline had to use sync `std::fs` (async would
   need the FD-readiness bridging that `async-zmq`/`tmq` add).

**Honest limitation:** loopback ≠ the real deployment. The reported bug was on ~200 real workers over
TCP (segmentation, congestion, many peers) — conditions the in-process spike does not recreate. So the
spike proves the crate framing is **correct in principle** and the pure-Rust impl is **viable**, but
it does **not** prove the production bug is gone. That requires the application-level reassembly
hardening (phase 3) and/or a real-network soak — both independent of the crate choice.

**Conclusion / recommendation:** the spike **clears the pure-Rust `zeromq` crate** (correct under
stress, ~90 % throughput, async-native), so the decision reduces to *maintenance-escape + async-native
(`zeromq`)* vs. *maximum battle-tested-ness (libzmq)* — a judgement call for the owner, not a
correctness blocker. Either way, **transport-independent phases 1–4 (channel hand-off, DB batching,
sink fan-out + async I/O, lock-free maps) deliver most of the win and should proceed first**; the
transport swap is a separable, reversible layer that the spikes de-risk.

### Full-topology + ZMTP interop validation (2026-06-14, owner: "does zmq.rs support all features we need")

The PUSH/PULL spike above only covered the result path. Two further spikes answer the owner's three
direct questions — *does zmq.rs cover our features, at the performance + robustness we need* — against
CorTeX's **full** topology and a **mixed, arXiv-like (heavy-tailed)** payload.

**Feature coverage — YES, complete for our usage.** CorTeX's wire needs, confirmed from `src/`:
`ROUTER` (ventilator `ventilator.rs:72`), `DEALER` (worker source `worker.rs:117`), `PUSH` (worker
sink `worker.rs:124`), `PULL` (dispatcher sink `sink.rs:46`), TCP transport, multi-frame messages.
The `zeromq` 0.6 **source** implements `router`, `dealer`, `push`, `pull` (plus req/rep/pub/sub/
xpub/xsub) — **all four of our socket types** — over TCP (+ IPC), with inherently multi-frame
`ZmqMessage`. What it omits (README: *"does not implement all of ZeroMQ's feature set"*) is **outside
our usage**: PAIR sockets, `inproc` transport, and CURVE security (our ZMQ is internal; the web tier is
guarded by Anubis + the network perimeter, not ZMTP CURVE).

**`examples/zmq_arxiv_workload.rs`** — pure-Rust `zeromq` on *every* side: a ROUTER ventilator leases
heavy-tailed sources (≈80 % small 64–192 KB, ≈17 % medium 1–3 MB, ≈3 % large 5–10 MB) to N concurrent
DEALER workers, who PUSH results to a PULL sink. Every frame is stamped `[seq|idx|worker-nonce]` so a
worker detects interleaving, reordering, **and misrouting** (a reply meant for another worker — the
ROUTER's core job).

**`examples/zmq_interop.rs`** — THE decisive test: **our side on pure-Rust `zeromq` (ROUTER + PULL),
workers on libzmq `zmq` (DEALER + PUSH, in threads, with explicit ZMQ identities — the pericortex
configuration).** A migrated production runs exactly this split, so the two implementations must speak
ZMTP to each other.

| Run (release, loopback) | Result | Throughput | Integrity |
| --- | --- | --- | --- |
| same-impl, 200 workers, 20 000 tasks | 20000/20000 | **4298 tasks/s** · 2870 MB/s | ✓ clean |
| same-impl, 200 workers, 256 KB frames | 8000/8000 | 847 tasks/s · 2315 MB/s | ✓ clean |
| **interop** (zeromq ↔ libzmq), 200 workers, 20 000 tasks | 20000/20000 | **3033 tasks/s** · 2026 MB/s | ✓ clean |
| **interop**, 200 workers, 256 KB frames | 8000/8000 | 794 tasks/s · 2173 MB/s | ✓ clean |

**Performance — YES, with 30–40× headroom.** Production target is ~100 tasks/s (`deployment-sizing`).
The full topology sustains **4298 tasks/s** (same-impl) / **3033 tasks/s** (interop) at the real
200-worker fleet size — and even fat-paper 256 KB-frame loads stay ~800 tasks/s. Loopback MB/s is
multi-GB/s, far above the real network + `/data` disk that actually bound production.

**Robustness — YES in these faithful models.** Zero interleaving / reordering / **misrouting** /
frame-loss across ~56 000 tasks total (28 k same-impl + 28 k interop), 200 concurrent workers,
heavy-tailed payloads up to ~10–40 MB, exercising **ROUTER routing-by-identity** under concurrent
variable-size requests — the riskiest path and the one the owner's interleaving bug would live on. It
did **not** manifest on `zeromq`.

**Interop — YES, the migration is incremental + wire-compatible.** A pure-Rust `zeromq` dispatcher and
**unchanged libzmq `pericortex` workers** interoperate cleanly over ZMTP at fleet scale. So we can move
**the dispatcher first** and leave the workers; full removal of the **C libzmq dependency** then only
requires later migrating `src/worker.rs` + `pericortex` — and the interop proof is exactly what makes
that staged + reversible.

**Honest caveats (gate the production cutover on these):**
- **Maturity:** zmq.rs's README is thin — *"Basic ZMTP implementation is working and tested against the
  reference implementation."* That is far less battle-proven than libzmq's decades. → stage + soak.
- **Loopback ≠ a real multi-host network** (TCP segmentation, congestion, NIC, reconnects). The spikes
  prove correctness-in-principle + ZMTP interop, **not** a production soak.
- **Not yet stressed:** ZMTP **heartbeats / worker-disconnect detection / reconnect** — the ventilator's
  worker-timeout reaper depends on noticing dead workers; validate this before cutover.

**Bottom line:** the validation **clears pure-Rust `zeromq` for CorTeX's needs** — feature-complete for
our topology, 30–40× perf headroom, clean under a faithful arXiv-like load, and **wire-compatible with
the existing libzmq workers**. The remaining risk is maturity/soak, not features or correctness.
Recommend adopting `zeromq` for the dispatcher behind the staged plan below (transport-independent
phases 1–4 first; transport swap as a separable layer), gating the cutover on a real-network soak +
heartbeat/reconnect validation, then optionally migrating the workers to finish removing libzmq.

### Caveat #3 — resilience + realistic torture (2026-06-14): **PASSED → owner green-lit the switch**

The owner made the transport switch conditional on a resilience spike proving production-readiness,
then specified a realistic torture profile. Two more spikes close it.

**ZMTP-level findings (from the `zeromq` source):** the ROUTER + PULL implement **disconnect
detection** (`set_on_disconnect`/`peer_disconnected`) and there is an auto-**reconnect** module — so a
ROUTER `send` to a vanished peer returns an `Err` (not a hang) and reconnecting DEALERs are re-accepted.
But there is **no ZMTP heartbeat (PING/PONG)** and no TCP-keepalive option, so a *silent/half-open*
peer (power-cut host) is only noticed via OS TCP timeouts. **This does not threaten correctness**:
CorTeX recovers dead-worker tasks via the **application-level lease-timeout reaper** (the in-flight
set's timeout sweep), which is transport-agnostic and fires regardless of ZMTP keepalive. Heartbeats
would only speed up *silent*-peer detection; the reaper is the real safety net.

**`examples/zmq_resilience.rs`** — a zeromq ROUTER ventilator + lease-timeout reaper vs. libzmq workers
under churn (crash-holding-a-lease, request-then-die → dead-peer send, drop+reconnect). Result: **every
task recovered & completed, zero loss**, even at *extreme* churn (100 workers / 80 % flaky / 5000
tasks → 40 killed + 41 reconnects, all recovered; no hang/panic). Two recovery paths confirmed: the
ventilator catches dead-peer `send` errors (immediate re-lease) and the reaper re-queues timed-out
leases.

**The torture payload set (`examples/zmq_torture.rs`), per owner spec — all five stressors at once:**

| Stressor | Model |
| --- | --- |
| Variable sizes | **log-normal** `μ=ln(800 KB), σ=1.121` ⇒ **median 800 KB, mean ~1.5 MB**, clamped [500 KB, 200 MB]; a 0.2 % giant-injector forces the 50–200 MB tail (pure log-normal puts 200 MB at ~5σ — never seen otherwise). Chunked at 256 KB/frame ⇒ a 200 MB job is an **~800-frame** multipart message. |
| Flaky network | per-task random **disconnect → reconnect** of a fresh DEALER+PUSH pair (‰-rate knob). |
| Cross-talk | **hundreds** of concurrent consumers round-tripping; every frame stamped `[seq|idx|nonce]` and re-verified ⇒ any interleaving/reordering/**misrouting** is counted. |
| Timeout flakiness | a fraction of consumers **sleep an intended 10 s–45 min** (capped for runnability) on some tasks, blowing past the lease ⇒ must be re-issued; their late result deduped. |
| Slow/unreliable DB | the **batch finalize** sleeps a random latency **≤15 s per batch**; a **bounded** sink→finalize channel must backpressure (no loss/OOM). |

**Result (release, 250 consumers, 2000 tasks, DB ≤15 s):** realized payloads `min 500 KB · p50 867 KB ·
mean 1.9 MB · max 150 MB · 6 giants ≥50 MB` (on spec; the mean rides above 1.5 MB only because of the
torture giant-injector — `GIANT_BP=0` gives the pure 1.5 MB). **2000/2000 persisted exactly once, zero
integrity anomalies**, under 5902 reconnects + 40 timeout-sleeper misses (all re-leased; 40 late results
correctly **deduped**) + 40 reaper re-leases, with the **mock DB the bottleneck** (51 tasks/s — exactly
the backpressure the bounded-channel + batched-finalize design must absorb). No hang, no OOM, no panic.

**Decision:** the resilience + torture spikes **reveal production-readiness for our model**, so per the
owner's conditional green light **the dispatcher transport will move to pure-Rust `zeromq`**. The lone
residual that a loopback harness *cannot* prove is a **real multi-host network soak** (true packet
loss, partitions, NIC saturation, OS-level half-open detection) — that stays the final gate before
flipping production traffic, not a blocker for starting the staged implementation.

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

## Decisions & open questions

1. **Phase-0 spikes — DONE & SETTLED.** Payload A/B, full-topology, ZMTP interop, resilience, and the
   five-stressor torture all pass (see above). **Transport decided: pure-Rust `zeromq`** (owner green
   light, 2026-06-14, conditional on the resilience spike — condition met). `tmq`/`async-zmq` are off
   the table (they wrap the same libzmq binding). The one open *validation* is the real-network soak,
   which gates flipping production traffic, not starting the build.
2. **`dashmap`** for the in-flight set + service cache — acceptable new dependency? (Alternative: a
   hand-sharded `Mutex<HashMap>` — more code, no new dep.) *Needed at phase 4.*
3. **Config knobs**: `finalize_batch` (DB batch size) and the sink writer-pool size as
   `DispatcherConfig` knobs (defaulting batch ~ a few hundred, writers ~ host cores)? Consistent with
   the existing dispatcher knobs. *Needed at phases 2–3.*

*(Status: **phase 0 complete; transport green-lit.** Implementation of the hot path is still gated on
the owner's "Hold for review" of the phased plan itself — the spikes settle the transport question the
hold was protecting. Recommended first step: **phase 1 (done-queue → bounded channel)**, the smallest,
transport-independent, highest-clarity change. The torture spike already exercises the phase-1→3 shape
(bounded sink→finalize channel + batched, latency-stalled finalize) end-to-end.)*
