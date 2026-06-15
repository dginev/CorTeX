# Dispatcher phase-5b perf audit findings

Date: 2026-06-15

## Short answer

The `zeromq` 0.6 ROUTER send path does not enqueue into a large libzmq-style I/O
thread pipe. For ROUTER, `peer.send_queue` is the actual
`asynchronous_codec::FramedWrite` for that peer's TCP write half. Calling
`RouterSendHalf::send(reply).await` uses `futures::SinkExt::send()`, and that
future does `poll_ready -> start_send -> poll_flush`. So each application-level
ROUTER send waits until the whole multipart reply has been written/flushed to
that selected worker socket.

Our ventilator has exactly one FIFO reply channel and one send loop. That makes
all worker replies globally head-of-line blocked behind the slowest current
socket flush. As peer count rises, the probability that any one peer's TCP write
is temporarily not ready rises, and the single send loop stops serving every
other ready peer while it awaits that one flush. The prep thread then blocks in
`reply_tx.blocking_send`, recv-side backpressure builds, workers wait, and
throughput falls off a cliff.

This is not a hidden per-peer connection task starving, and it is not a tiny
per-peer Rust `mpsc` queue. The small/serializing buffer is the framed writer's
write buffer plus the fact that `SinkExt::send()` flushes it synchronously. The
contention is our one global reply lane.

## What the source says

`zeromq-0.6.0/src/router.rs` has the same implementation for `RouterSocket` and
`RouterSendHalf`:

```rust
let peer_id: PeerIdentity = message.pop_front().unwrap().try_into()?;
match self.inner.backend.peers.get_async(&peer_id).await {
    Some(mut peer) => {
        peer.send_queue.send(Message::Message(message)).await?;
        Ok(())
    }
    None => Err(ZmqError::Other("Destination client not found by identity")),
}
```

`zeromq-0.6.0/src/backend.rs` defines:

```rust
pub(crate) struct Peer {
    pub(crate) send_queue: ZmqFramedWrite,
}
```

and `peer_connected()` stores the write half from `FramedIo::into_parts()`:

```rust
let (recv_queue, send_queue) = io.into_parts();
self.peers.upsert_async(peer_id.clone(), Peer { send_queue }).await;
```

`zeromq-0.6.0/src/codec/framed.rs` aliases `ZmqFramedWrite` to
`asynchronous_codec::FramedWrite<Box<dyn FrameableWrite>, ZmqCodec>`. There is
no extra ROUTER writer coroutine and no per-peer `mpsc` capacity to tune.

The decisive detail is in `futures-util`'s `SinkExt::send()` future: after the
item has been fed to the sink, it always calls `poll_flush()` before resolving.
`asynchronous-codec-0.7.0/src/framed_write.rs` implements that flush by repeatedly
calling the underlying TCP `poll_write()` until the buffer is empty, then
`poll_flush()` on the socket.

The codec itself is straightforward. `zeromq` encodes every multipart frame into
one `BytesMut`; frames longer than 255 bytes use the long-frame header, then the
payload is appended. Our default `dispatcher.message_size` is 100,000 bytes, so
real task replies are not small control messages. A single reply can easily be
one or more large framed writes, and the caller awaits the full socket flush.

## What is not the cause

The `split()` halves are not sharing one coarse backend mutex. Peers live in an
`scc::HashMap`, and recv's `FairQueue` has its own mutex around recv streams.
That mutex has overhead, but the interop spike uses the same fair queue and runs
with 200 libzmq DEALER peers, so fair-queue overhead alone does not explain a
collapse at 16 peers.

The "per-peer connection/io task drains `send_queue`" model does not apply to
ROUTER in `zeromq` 0.6. Accept/handshake runs in spawned tasks, but after
`backend.peer_connected()` the ROUTER backend stores the framed read half in the
fair queue and the framed write half in `Peer`. The task that calls
`send().await` performs the write.

Changing Tokio worker-thread count cannot fix a single FIFO send lane. Spawning
the send loop as a separate task only gives that one lane its own scheduler slot;
it still serializes every peer's full flushed reply. That matches the audit
update: separate send task improved one point but still collapsed at 16/64
workers.

## Why the spike does not collapse

`examples/zmq_interop.rs` does this in one socket-owning task:

```rust
router.recv().await;
build reply in memory;
router.send(reply).await;
```

It never has a blocking prep thread filling a FIFO reply channel ahead of the
socket writer, and it never creates a backlog where a reply for ready peer B is
stuck behind an awaited flush to peer A. The successful spike is therefore proof
that `zeromq` ROUTER/DEALER interop and the recv fair queue are viable, not proof
that a single out-of-band FIFO send loop is viable for the real dispatcher.

The spike also avoids the real dispatch path: no Diesel fetch, no source-archive
read, no in-flight bookkeeping, no mock-reply sleeps, and no async-to-std bridge.
Those differences matter because they let the real prep thread produce replies
independently of socket write readiness. Once that producer is faster than the
single flushed send lane for even short bursts, FIFO head-of-line blocking
dominates high-peer latency.

## Minimal fix for `src/dispatcher/ventilator.rs`

Keep the prep thread and the split ROUTER, but replace the single `reply_tx /
reply_rx / send_task` with a small set of sharded send lanes. Hash the ROUTER
identity and send the reply to that shard. Each shard owns a cloned
`RouterSendHalf` and has its own bounded channel:

```rust
const SEND_LANES: usize = 16;

fn reply_lane(identity: &[u8]) -> usize {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    identity.hash(&mut h);
    (h.finish() as usize) % SEND_LANES
}
```

Concrete shape:

1. Change the prep reply output from one `mpsc::Sender<ZmqMessage>` to
   `Vec<mpsc::Sender<ZmqMessage>>`.
2. In `prep_loop`, replace `reply_tx.blocking_send(reply)` with:

   ```rust
   let lane = reply_lane(&req.identity);
   if reply_txs[lane].blocking_send(reply).is_err() {
       break;
   }
   ```

   Do this for real replies and mock replies. Compute the lane before `req` is
   moved into the reply if needed.
3. After `router.split()`, create `SEND_LANES` bounded reply channels and spawn
   `SEND_LANES` send tasks. Each task gets `let mut sender = send_half.clone();`
   and drains one receiver with `sender.send(reply).await`.
4. Preserve per-worker ordering by always hashing the same identity to the same
   lane. The dispatcher protocol has one outstanding request per worker anyway,
   but this keeps the routing invariant explicit.
5. On recv termination, drop `req_tx`; when the prep thread exits it drops all
   lane senders; await all send tasks.

This is a narrow change: no Diesel ownership change, no `spawn_blocking`, no
source-read redesign, no protocol change, and no fork of `zeromq`. It removes
the global head-of-line blocker while keeping bounded backpressure. With 16
lanes, one temporarily slow peer can stall only its shard, not the entire worker
fleet. If the benchmark still shows a cliff, try 32 lanes before changing the
architecture; the lane count should be at least the measured collapse threshold.

## Secondary options

A unified sequential loop like the spike is a useful diagnostic fallback, but it
is not the best first production fix because the real ventilator must offload
blocking DB/file work. A unified loop can avoid recv-side runahead only by
waiting for prep before sending, which sacrifices useful overlap and still keeps
one flushed socket write at a time.

Reducing `dispatcher.message_size` below 100,000 may reduce individual flush
latency, but it increases multipart frame count and does not remove the global
FIFO. Increasing the framed writer HWM would require changing/forking `zeromq`
or `asynchronous-codec`, and `SinkExt::send()` would still flush before return.

Moving file reads to `spawn_blocking` is orthogonal. It may help prep latency,
but the non-`Send` Diesel connection and single-owner dispatch queues make the
current prep thread reasonable. The measured cliff is on the socket send side.

## Validation to run

Instrument before/after, then run the existing gate:

- latency of each `send_half.send(reply).await`, tagged by lane;
- depth/wait time of each reply lane;
- `req_tx.send` wait time;
- real replies vs mock replies, because taskid `0` makes workers sleep for
  `CORTEX_WORKER_THROTTLE_SECS`;
- total send failures by lane.

Benchmark:

```bash
source ~/.cargo/env
set -a; . ./.env; set +a
export CORTEX_WORKER_THROTTLE_SECS=1 BENCH_DEADLINE_S=30
cargo build --release --example dispatcher_bench
for w in 4 16 64; do
  BENCH_WORKERS=$w BENCH_TASKS=20000 ./target/release/examples/dispatcher_bench 2>&1 \
    | grep -E "drained|throughput|PASS|FAIL"
done
```

Acceptance target: 16/64-worker `zeromq` runs should no longer sit near
230-260 tasks/s. A reasonable first bar is to match or beat the existing
`examples/zmq_interop.rs` high-peer result around 3033 tasks/s; if sharding gets
near the libzmq ventilator's 8k-9k/s, no deeper transport work is needed.
