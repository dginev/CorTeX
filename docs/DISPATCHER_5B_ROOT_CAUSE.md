# Dispatcher 5b — async `zeromq` ventilator throughput collapse: root cause (closing report)

> **Status: line of work CLOSED.** Root cause identified by per-stage instrumentation. This report
> documents the cause, the evidence, and the resulting recommendation. Supersedes the open questions in
> `DISPATCHER_5B_PERF_AUDIT.md` / `DISPATCHER_5B_CODEX_FINDINGS.md`.

## The question

Swapping the dispatcher's task-dispatching **ROUTER ventilator** from libzmq (`zmq` 0.10) to the
pure-Rust async `zeromq` (0.6) crate collapsed throughput ~30–40× under load: ~8200 tasks/s at 4
workers but ~200–260 tasks/s at ≥16 workers, while the libzmq ventilator is flat at ~8500 tasks/s for
any worker count. (The **sink** swap — phase 5a — had no such problem and shipped.) Is this a fixable
misconfiguration or a real limit of `zeromq`?

## Root cause: tokio task-scheduling latency in the per-dispatch critical path

**The ventilator's own work is µs-fast at every worker count. The collapse is the dispatcher task
being *descheduled* — waiting its turn on the tokio runtime — for ~97% of each dispatch.**

Per-stage instrumentation of the decoupled ventilator (recv task → bounded `tokio::mpsc` → prep
thread → bounded `tokio::mpsc` → send task), average **µs per dispatch**, at 16 workers (the collapsed
regime, ~208 tasks/s):

| stage | µs | what it measures |
| --- | --- | --- |
| `recv()` | 116 | `recv_half.recv().await` wall-clock |
| `forward` | **0** | `req_tx.send().await` (recv → prep) |
| `prep_work` | 68 | DB `fetch_tasks` (amortized) + source read + build reply |
| `prep_send_wait` | **0** | `reply_tx.blocking_send` (reply channel was never full) |
| `reply_wait` | 111 | send task idle, waiting for the next prepared reply |
| `send()` | 5 | `send_half.send().await` (the ROUTER framed write) |
| `real_dispatch` | **4000/4000** | every dispatch handed out a *real* task (no mock/throttle churn) |

**The arithmetic that closes it:** 208 tasks/s ⇒ **4808 µs of wall-clock per dispatch**. The recv
loop's *active* time per dispatch is `recv + forward` ≈ **116 µs**. The other **~4690 µs (97.6%)** is
time the recv-loop task spends **off-CPU, descheduled**, between finishing one iteration and being
polled again by the runtime. In-flight stays tiny (~37) and the reply channel never backs up
(`prep_send_wait=0`), so nothing is queued/contended — the pipeline is simply *idle, waiting to be
scheduled*.

So the bottleneck is **not** `send()` latency (5 µs), **not** the DB/file work (68 µs), **not** the
channel bridges (0 µs), **not** mock-reply/throttle churn (0 mocks), and **not** a `zeromq` backend
lock (its `peers` map is a concurrent `scc::HashMap`). It is the **latency of the tokio scheduler
returning to each task in the dispatch chain**.

## Why this hits the ventilator but not the sink or libzmq

- **`zeromq` runs all socket I/O as tokio tasks.** A request/reply round-trip traverses several task
  hops: the worker's request wakes a `zeromq` per-peer connection task → the recv-loop task → (channel)
  the prep thread → (channel) the send-loop task → a `zeromq` connection task flushes the reply. **Each
  hop costs one scheduler wakeup + queue delay.** Those delays are in the *critical path* of the
  worker round-trip, and they **compound with peer count** (more connection tasks contend for the
  runtime, so the time to re-poll any given task grows). The ventilator's pattern — *many tiny,
  latency-sensitive round-trips* — is maximally exposed to per-message scheduling latency.
- **The sink (5a) is immune** because it's a *bulk, one-directional receive*: it pulls a large result
  archive and hands it to a writer pool. There is no per-message round-trip whose latency throttles a
  remote worker; scheduling latency is amortized over big transfers, not paid per tiny dispatch.
- **libzmq has no scheduler in the path.** Each socket has dedicated **C I/O threads** that do the
  TCP read/write directly — no cooperative tokio task, no wakeup queue, no per-message scheduling. So a
  libzmq ROUTER round-trip is ~µs and flat at any peer count (~8500 tasks/s, bottlenecked downstream at
  the DB, not the socket).
- **The `zmq_interop` spike** (`zeromq` ROUTER, 3033 tasks/s @200 workers) is *also* paying this tax —
  3033/s is already ~3× below libzmq — but it has **fewer task hops** (one unified task: recv → build
  reply in-memory → send; no prep thread, no channels), so its scheduling overhead per dispatch is
  smaller. Our ventilator can't use that shape because it must do **blocking** DB/file work, which
  forces the off-reactor prep-thread hop (and the channel hops around it).

This also explains the **instability** we saw (the "same" design measured anywhere from ~200 to
~8200 tasks/s): a scheduling-latency-bound system is acutely sensitive to runtime load, task count,
and timing — exactly what you'd expect if throughput is governed by *when the scheduler gets back to
you*, not by how much work there is.

## The irreducible tension

To match libzmq we'd need to remove the per-dispatch scheduling latency, i.e. minimize task hops (the
spike's single-task shape). But the ventilator **must** run blocking `fetch_tasks`/source-reads, which
must be off the async reactor (a thread + channels), which *is* the extra hops. With the async
`zeromq` transport, the dispatch round-trip cannot avoid several scheduled hops; with libzmq's
dedicated-thread C I/O, there are none. **For a high-frequency, latency-sensitive request/reply socket,
the async-task-per-message model is structurally worse than dedicated-thread C I/O — regardless of
configuration.** We confirmed this empirically across current-thread / multi-thread(N) runtimes,
`select!` vs spawned tasks, split vs unified socket, and inline vs decoupled blocking work — every
variant either collapses at ≥16 peers or is uniformly slow.

## Does production care?

Marginally tolerable, but with no headroom and against the project's stated direction:

- Production offered-load is ~200 workers × ~1 task/s (latexml) ≈ **~200 tasks/s**, and the collapsed
  `zeromq` ventilator does ~200–260/s — so it would *barely* keep up today, with ~0 margin.
- But Arm-14's whole premise is the **fast-worker reality** (latexml-oxide). As `s/task` drops, offered
  load *rises* (0.5 s/task ⇒ ~400/s) and the `zeromq` ventilator becomes the wall — exactly where we're
  trying to scale. libzmq has ~40× headroom (~8500/s) for that future.

## Recommendation

**Keep libzmq for the ventilator; keep the `zeromq` sink (5a).** This is a *mixed transport*, which the
interop spike proved works (zeromq ROUTER/PULL ↔ libzmq workers, and libzmq ventilator ↔ libzmq
workers). The 5a sink swap was a genuine win (its atomic whole-message recv *retired* the D-4/D-12
desync bug class, at zero throughput cost). The 5b ventilator swap is a structural loss for this
workload. The maintenance goal (escape the libzmq C FFI) is **partially** achieved (the sink is
pure-Rust); fully dropping `zmq` would require either accepting the ventilator scheduling tax or a
different async-ZMQ implementation that does socket I/O on dedicated threads rather than per-message
tasks.

## What was tried (for the record)

Runtime: current-thread, multi-thread default(64), multi-thread `worker_threads(4)`. Socket: unified
sequential (spike shape) vs `split()` halves. Scheduling: both loops under one `select!` vs send loop
`spawn`ed as its own task. Blocking work: inline (with/without `block_in_place`) vs decoupled prep
thread. Confirmed not-the-cause: Nagle (TCP_NODELAY is set), thread oversubscription, `select!`
send-starvation, backend-lock contention, mock-reply/throttle churn, channel-bridge latency, `send()`
latency, prep latency. The per-stage instrumentation (this report's table) is the decisive evidence.
