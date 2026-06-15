# Dispatcher phase-5b — async `zeromq` ventilator throughput collapse (perf audit handoff)

> **Audience:** a second engineer (codex) doing an independent performance audit. **Goal:** find why
> our async `zeromq` ROUTER ventilator throughput **collapses as worker count rises** (fast at 4
> workers, ~30× slower at ≥16), when the equivalent libzmq ventilator is flat-fast at all counts and
> a throwaway `zeromq` spike handled 200 workers fine.

## Context: what we're doing

CorTeX's dispatcher (`src/dispatcher/`) leases tasks to remote workers over ZeroMQ. We are migrating
the transport from the libzmq C-FFI `zmq` crate (0.10) to the **pure-Rust async `zeromq` crate**
(0.6, "zmq.rs") to escape the C dependency and a large-multipart desync bug class.

- **Phase 5a (sink, DONE + shipped):** the result-receiving **PULL** sink was swapped to async
  `zeromq` on a tokio **current-thread** runtime. It works great — **9840 tasks/s, zero regression**,
  all gates green. The sink only *receives* (one recv loop), and its blocking work (archive writes to
  disk) is offloaded to a std-thread writer pool, so its socket loop never blocks.
- **Phase 5b (ventilator, THIS PROBLEM):** the task-dispatching **ROUTER** ventilator. The ventilator
  is request/reply: a worker (libzmq DEALER) sends `[service_name]`, the ROUTER prepends the worker
  identity → `[identity, service]`; the ventilator replies `[identity, taskid, ...source-archive
  frames]`. The workers **stay on libzmq** (interop is proven — see below), only the dispatcher moves.

## The symptom (measured, release build, loopback, this 64-core box)

Bench = `examples/dispatcher_bench.rs`: runs the real dispatcher (perpetual, `job_limit=None`) + N
libzmq `EchoWorker`s (each: DEALER request → recv source → echo it back → PUSH result → repeat), 20000
tasks, polls the DB until all terminal. `CORTEX_WORKER_THROTTLE_SECS=1` (worker sleeps 1s on an empty
"mock" reply). Same bench for both transports.

| Workers | **libzmq ventilator** (baseline) | **`zeromq` ventilator** (5b, best config) |
| --- | --- | --- |
| 4   | 8947 tasks/s | **8200 tasks/s** ✓ |
| 16  | 8950 tasks/s | **232 tasks/s** ✗ (collapse) |
| 64  | 8206 tasks/s | **249 tasks/s** ✗ (collapse) |

- **libzmq is flat-fast** (~8.5k/s) at every worker count — it isn't even the bottleneck (the
  sink/DB-finalize at ~9k/s is).
- **`zeromq` is fast at 4 workers (~8.2k/s) but collapses ~30× at ≥16 workers** to ~230–260/s, and
  *stays* collapsed (does not recover or scale) up to 128 workers. Per-worker rate at the collapse is
  ~2–15 tasks/s (≈ tens of ms per round-trip); at 4 workers it's ~2050 tasks/s/worker (≈0.5 ms).
- The collapse **threshold is between 4 and 16 workers** and is independent of the tokio runtime's
  worker-thread count (tried current-thread, multi-thread default=64, and `worker_threads(4)` — all
  collapse at ≥16, see "What we've tried").

## The KEY reference: zeromq ROUTER *can* do this fast

`examples/zmq_interop.rs` (a throwaway spike, already validated) runs **our side on `zeromq`**
(ROUTER ventilator + PULL sink) and **workers on libzmq** (DEALER + PUSH), with synthetic in-memory
replies (no DB, no file I/O). It does **3033 tasks/s at 200 workers, zero loss/misrouting**. So:

- `zeromq` ROUTER ↔ libzmq DEALER interop is correct and *does not collapse* at 200 peers.
- The spike's ventilator is a **single async task** doing `router.recv().await` → build reply
  in-memory → `router.send().await` **sequentially on the SAME unified socket**, on a multi-thread
  runtime. **No `split()`, no channels, no blocking work.**

This is the crux: the spike (unified sequential socket) scales to 200 workers, our 5b ventilator
(split halves + channels + prep thread) collapses at 16. **The difference is our architecture, not
zeromq itself.**

## Our 5b architecture (`src/dispatcher/ventilator.rs`)

We can't copy the spike directly because the real ventilator must do **blocking** work per dispatch:
a `fetch_tasks` Postgres query (diesel, blocking; amortized — batches `queue_size`=100 tasks per
query) and a **source-archive file read** (blocking). Doing that blocking work inline on the async
socket task starves zeromq's I/O (we measured 40–260/s for the inline-blocking variants). So we
**decoupled** it (mirroring the sink's writer-pool offload):

1. **`router.split()`** into `RouterSendHalf` + `RouterRecvHalf` (the `zeromq` API for "concurrent
   send/recv from independent tasks"; both wrap a shared `Arc<RouterSocketInner>` whose `backend:
   Arc<GenericSocketBackend>` holds the per-peer state).
2. **recv loop** (async, never blocks): `recv_half.recv().await` → parse `[identity, service]` →
   forward a `WorkerRequest{identity, service}` over a **bounded `tokio::mpsc`** to the prep thread.
3. **prep thread** (a dedicated `std::thread`, blocking OK): owns its own DB connection + the
   per-service dispatch queues. Loops: `req_rx.blocking_recv()` → reap-on-cadence → service lookup →
   backpressure check → `fetch_tasks` if queue empty → pop a task → **insert into the shared in-flight
   set BEFORE replying** (a correctness invariant) → read the source archive into `message_size`
   frames → build the reply `ZmqMessage` → `reply_tx.blocking_send(reply)`.
4. **send loop** (async, never blocks): `reply_rx.recv().await` → `send_half.send(reply).await`.
5. The two async loops run under `tokio::select!` inside `runtime.block_on(...)`. The prep thread is a
   plain std thread joined at the end.

So per dispatch there are **two async↔std-thread channel crossings** (recv-loop → prep via
`tokio::mpsc` + `blocking_recv`; prep → send-loop via `blocking_send` + `tokio::mpsc`).

## What we've tried (and the result)

| Variant | 4 workers | ≥16 workers |
| --- | --- | --- |
| Inline blocking, current-thread runtime | 40/s | 40/s (send-flush starved) |
| Inline blocking, multi-thread `worker_threads(4)` | 3314/s | 78/s (collapse) |
| Inline blocking, multi-thread default(64) | 149/s | ~120/s |
| **Decoupled (split+channels+prep)**, current-thread | ~150/s | ~260/s (send-flush starved — slow but "stable") |
| **Decoupled**, multi-thread default(64) | **7576/s** | 225/s (collapse) |
| **Decoupled**, multi-thread `worker_threads(4)` | **8200/s** | 232/s (collapse) |

Observations:
- **current-thread** is slow even at 4 workers (~150–260/s): the single runtime thread can't flush a
  `send_half.send()` to the wire promptly because zeromq's per-peer connection *task* (which does the
  actual socket write) only runs when the thread next polls. **multi-thread fixes the 4-worker case
  (→8200/s)** because the connection tasks run on other threads.
- But **multi-thread collapses at ≥16 workers regardless of `worker_threads`** (4 or 64). So the
  collapse is *not* tokio thread oversubscription.
- The collapse is specific to **many concurrent zeromq ROUTER peers** in *our* design. The spike
  (unified sequential socket) does not collapse at 200 peers.

## Leading hypothesis

The `split()` halves share one `Arc<GenericSocketBackend>`. Our recv loop and send loop touch it
**concurrently** (recv locks the peer/fair-queue state to pull a message; send locks the peer map to
route by identity), *plus* zeromq's own per-peer connection tasks lock it. As peer count rises, this
contention explodes — whereas the spike's **single task serialises recv-then-send**, so our code never
holds the backend concurrently. Alternatively/additionally, the **two channel crossings** per dispatch
interact pathologically with many peers (e.g., waker storms, or the prep thread's `blocking_recv`/
`blocking_send` parking latency under load).

## Questions for the audit

1. **Is `split()` + concurrent recv/send the collapse cause?** Would a **single async task** owning the
   unified `RouterSocket` (recv → hand to prep → await prepared reply → send, sequentially like the
   spike, but with the blocking work still offloaded to the prep thread) avoid the contention while
   keeping the socket non-blocking? Does that sacrifice too much (sequential = no recv/send overlap)?
2. **Is the tokio↔std-thread bridge (`blocking_send`/`blocking_recv`) a latency source under load?**
   Would `spawn_blocking` (tokio's blocking pool) for the DB/file work — instead of a dedicated std
   thread + channels — be better? Constraint: the diesel `PgConnection` is **not `Send`**, so it can't
   be moved into `spawn_blocking`; only the file read (path → bytes) is trivially offloadable.
3. **Is there a `zeromq` 0.6 socket option / pattern we're missing** (send HWM, a flush, a fairness or
   batching knob, `connect`/`bind` options) that the spike implicitly benefits from and we don't?
   (TCP_NODELAY *is* set by zeromq — checked. Sockets use `Arc<GenericSocketBackend>` + a `FairQueue`
   on the recv half.)
4. **Is the collapse a zeromq bug at ≥N concurrent ROUTER peers under our access pattern**, or purely
   our misuse? The spike at 200 peers (3033/s) argues "our misuse."
5. What is the **simplest change** to get stable throughput that **scales with workers** (target: at
   least match the spike's ~3000/s at high peer counts; ideally approach libzmq's ~8500/s)?

## Where to look

- `src/dispatcher/ventilator.rs` — the 5b implementation (the `prep_loop` fn + the `start` method's
  `block_on` with `recv_loop`/`send_loop` under `select!`).
- `src/dispatcher/sink.rs` — the *working* 5a async sink (current-thread, writer-pool offload) for
  contrast.
- `examples/zmq_interop.rs` — the spike that scales to 200 workers (unified sequential socket).
- `examples/dispatcher_bench.rs` — the benchmark (env knobs: `BENCH_WORKERS`, `BENCH_TASKS`,
  `BENCH_DEADLINE_S`, `CORTEX_WORKER_THROTTLE_SECS`).
- `zeromq` 0.6 crate source: `~/.cargo/registry/src/*/zeromq-0.6.0/src/router.rs` (the `split()` impl,
  `RouterSendHalf`/`RouterRecvHalf`, `GenericSocketBackend`).

## How to reproduce

```bash
source ~/.cargo/env; set -a; . ./.env; set +a        # TEST/DATABASE_URL
export CORTEX_WORKER_THROTTLE_SECS=1 BENCH_DEADLINE_S=30
cargo build --release --example dispatcher_bench
for w in 4 16 64; do
  BENCH_WORKERS=$w BENCH_TASKS=20000 ./target/release/examples/dispatcher_bench 2>&1 \
    | grep -E "drained|throughput|PASS|FAIL"
done
```
Switch transports by editing `src/dispatcher/ventilator.rs` (the committed version is libzmq; the 5b
zeromq version is the current working tree).

## UPDATE — more experiments (none fixed the ≥16-worker collapse)

| Variant (decoupled split + prep thread, unless noted) | 4 workers | 16 workers | 64 workers |
| --- | --- | --- | --- |
| current-thread runtime | ~150/s | ~260/s | ~260/s |
| multi-thread default (64 threads) | **7576/s** | 225/s | 145/s |
| multi-thread `worker_threads(4)` | **8200/s** | 232/s | 249/s |
| multi-thread(4) + send loop **spawned as separate task** (not select!) | 4689/s | 350/s | 167/s |

**Ruled out:** Nagle (zeromq sets TCP_NODELAY), tokio thread oversubscription (collapse is independent
of `worker_threads`), current-vs-multi-thread (multi fixes 4-worker, not ≥16), `select!` send-starvation
(spawning recv/send as separate tasks didn't fix it), split-half backend lock (peers is a concurrent
`scc::HashMap`, only the recv `fair_queue` is a `Mutex`).

**The invariant that correlates with the collapse:** every design that does the **real blocking
dispatch** (prep thread + `tokio::mpsc` bridges + `fetch_tasks`/source-read) collapses at ≥16 workers;
the spike (`zmq_interop.rs`, no blocking, no channels, unified sequential socket) does NOT collapse at
200 workers. **zeromq CAN hit 8200/s here (= libzmq) at 4 workers**, so it's a fixable misconfiguration,
not a fundamental zeromq limit.

**Remaining suspect:** `send_half.send(reply).await` latency rises with peer count — the router send
routes to `peer.send_queue.send(msg).await`; if that per-peer queue is small and the peer's connection
task is starved among many peers, `send()` blocks, the reply channel backs up, the prep thread's
`blocking_send` blocks, and dispatch throughput collapses. Need to verify the `peer.send_queue`
capacity and whether the send path serialises under many peers.

## UPDATE 2 — codex consulted + two more experiments; still unsolved

`docs/DISPATCHER_5B_CODEX_FINDINGS.md` has codex's analysis. Codex's primary hypothesis (recv/send
co-scheduled in one `select!` task starves send) **was empirically disproved** — spawning the send
loop as a separate task still collapses (350/s @16, 167/s @64). Codex couldn't run the bench (no DB).

Two more variants tested:

| Variant | 4 workers | 16 workers | 64 workers |
| --- | --- | --- | --- |
| split + send loop **spawned as separate task** | 4689/s | 350/s | 167/s |
| **unified sequential** (recv→prep→await reply→send, the spike's turn-taking pattern) | 112/s | 86/s | 92/s |

- **split/pipelined** designs are fast at 4 workers (4.7–8.2k/s) but **collapse at ≥16** (unsolved).
- **unified sequential** does NOT collapse but is **uniformly ~100/s** — the sync↔async channel-bridge
  round-trip (`blocking_send`/`blocking_recv`, ~ms each) sits in the critical path when strictly
  sequential. The spike avoids this because it builds the reply *in-memory inline* (no channel, no DB).

**Diagnostic fact:** at the collapse, the bench shows **in-flight=27, TODO=12400** — so it is NOT
backpressure/mock-replies/sink; it is purely the ventilator's dispatch rate falling to ~250/s.

**Status: root cause of the ≥16-peer collapse in the split/pipelined design is still unidentified.**
What's proven: zeromq *can* hit 8200/s here (= libzmq) at 4 workers, so it's a fixable misconfig, not
a fundamental limit. **Next step (codex's instrumentation plan):** add per-stage latency counters
(`send_half.send` time, `reply_rx.recv` delay, prep time, real-vs-mock replies) to localize whether
the cliff is in `send().await`, the channel bridge, or the recv `FairQueue` under many peers — before
any further architecture change.
