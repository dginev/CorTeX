// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Dispatcher-rationalization SPIKE (throwaway; docs/archive/DISPATCHER_RATIONALIZATION.md). Caveat
// #3 — the resilience gate before committing to the pure-Rust `zeromq` transport: does a zeromq
// ROUTER dispatcher survive **worker churn** (crashes, request-then-die, reconnects) and still
// complete **every** task without loss?
//
// What this models (faithful to CorTeX's recovery design):
//   * A ROUTER ventilator that LEASES tasks and tracks them in an in-flight map, with an
//     **application-level lease-timeout reaper** that re-queues tasks whose worker never returned
//     them — the transport-AGNOSTIC safety net CorTeX already relies on (so it does not depend on
//     ZMTP heartbeats, which zmq.rs does not implement).
//   * Workers on libzmq (`zmq`, the pericortex config) that misbehave: a fraction are **flaky** —
//     they die holding a lease, or die right after requesting (so the ROUTER's reply hits a dead
//     peer), or **bounce** (drop + reconnect a fresh socket).
//   * Two recovery paths are exercised: (a) ROUTER `send` to a vanished peer returns an Err the
//     ventilator catches → immediate re-lease; (b) a lease that times out → reaper re-queues it.
//
// SUCCESS = all TASKS complete (zero loss) despite the churn, with no hang/panic. That demonstrates
// the dispatcher's resilience model is preserved on zeromq.
//
// Run:
//   cargo run --release --example zmq_resilience
//   WORKERS=40 TASKS=4000 FLAKY_PCT=40 cargo run --release --example zmq_resilience

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use zeromq::{PullSocket, RouterSocket, Socket, SocketRecv, SocketSend, ZmqMessage};

const RETRY_SENTINEL: u64 = u64::MAX; // "no work right now, ask again"
const LEASE_TIMEOUT: Duration = Duration::from_millis(1500);

fn env_usize(key: &str, default: usize) -> usize {
  std::env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

fn mix(mut x: u64) -> u64 {
  x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
  let mut z = x;
  z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  z ^ (z >> 31)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let workers = env_usize("WORKERS", 24).max(1);
  let tasks = env_usize("TASKS", 3000).max(1);
  let flaky_pct = env_usize("FLAKY_PCT", 35);
  let frame_bytes = 64 * 1024;

  println!(
    "zeromq resilience spike: ROUTER ventilator + lease-timeout reaper ↔ {workers} libzmq workers \
     ({flaky_pct}% flaky), {tasks} tasks — recover every task under churn"
  );

  // Shared dispatch state (all touched only by the tokio tasks; libzmq worker threads touch only
  // their sockets). Lock discipline: never hold two of {todo,in_flight,done} at once, except the
  // sink's done→in_flight; nothing takes in_flight→done, so there is no cycle.
  let todo: Arc<Mutex<VecDeque<u64>>> = Arc::new(Mutex::new((0..tasks as u64).collect()));
  let in_flight: Arc<Mutex<HashMap<u64, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
  let done: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
  let re_leased = Arc::new(AtomicUsize::new(0));
  let dead_peer_recovered = Arc::new(AtomicUsize::new(0));
  let shutdown = Arc::new(AtomicBool::new(false));
  let complete = Arc::new(tokio::sync::Notify::new());

  // Sink: PULL. On each result, mark the task done (idempotent — a re-leased task may complete
  // twice) and clear its in-flight lease; notify when all tasks are accounted for.
  let mut sink = PullSocket::new();
  let sink_ep = sink.bind("tcp://127.0.0.1:0").await?.to_string();
  let sink_task = {
    let (done, in_flight, complete) = (done.clone(), in_flight.clone(), complete.clone());
    tokio::spawn(async move {
      while let Ok(msg) = sink.recv().await {
        let seq = u64::from_le_bytes(msg.get(0).unwrap()[0..8].try_into().unwrap());
        let mut d = done.lock().unwrap();
        if d.insert(seq) {
          in_flight.lock().unwrap().remove(&seq);
          if d.len() == tasks {
            complete.notify_one();
            break;
          }
        }
      }
    })
  };

  // Ventilator: ROUTER. Leases a task per request; re-leases immediately if the reply can't be
  // delivered (the requesting worker already vanished — recovery path (a)).
  let mut router = RouterSocket::new();
  let router_ep = router.bind("tcp://127.0.0.1:0").await?.to_string();
  let ventilator = {
    let (todo, in_flight, done) = (todo.clone(), in_flight.clone(), done.clone());
    let (re_leased_dp, _re) = (dead_peer_recovered.clone(), re_leased.clone());
    tokio::spawn(async move {
      while let Ok(req) = router.recv().await {
        let identity = req.get(0).cloned().unwrap();
        let next = todo.lock().unwrap().pop_front();
        let mut reply = ZmqMessage::from(identity.to_vec());
        match next {
          Some(seq) => {
            in_flight.lock().unwrap().insert(seq, Instant::now());
            let mut header = vec![0u8; 16];
            header[0..8].copy_from_slice(&seq.to_le_bytes());
            reply.push_back(Bytes::from(header));
            let mut body = vec![0u8; frame_bytes];
            body[0..8].copy_from_slice(&seq.to_le_bytes());
            reply.push_back(Bytes::from(body));
            if router.send(reply).await.is_err() {
              // The worker vanished before we could hand off — re-queue at once (recovery path a).
              in_flight.lock().unwrap().remove(&seq);
              todo.lock().unwrap().push_front(seq);
              re_leased_dp.fetch_add(1, Ordering::Relaxed);
            }
          },
          None => {
            // No work queued right now (tasks may be in-flight to slow/dead workers) — tell the
            // worker to retry; the reaper will re-queue any that time out.
            if done.lock().unwrap().len() >= tasks {
              break;
            }
            let mut header = vec![0u8; 16];
            header[0..8].copy_from_slice(&RETRY_SENTINEL.to_le_bytes());
            reply.push_back(Bytes::from(header));
            let _ = router.send(reply).await;
          },
        }
      }
    })
  };

  // Reaper: the application-level lease-timeout safety net — re-queues any lease older than
  // LEASE_TIMEOUT (recovery path (b)). Transport-agnostic; this is what makes ZMTP heartbeats
  // unnecessary for correctness.
  let reaper = {
    let (todo, in_flight, done, re_leased) = (
      todo.clone(),
      in_flight.clone(),
      done.clone(),
      re_leased.clone(),
    );
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let now = Instant::now();
        let expired: Vec<u64> = {
          let mut inf = in_flight.lock().unwrap();
          let expired: Vec<u64> = inf
            .iter()
            .filter(|(_, leased)| now.duration_since(**leased) > LEASE_TIMEOUT)
            .map(|(seq, _)| *seq)
            .collect();
          for seq in &expired {
            inf.remove(seq);
          }
          expired
        };
        if !expired.is_empty() {
          let mut td = todo.lock().unwrap();
          for seq in &expired {
            td.push_front(*seq);
          }
          re_leased.fetch_add(expired.len(), Ordering::Relaxed);
        }
        if done.lock().unwrap().len() >= tasks {
          break;
        }
      }
    })
  };

  let start = Instant::now();

  // Workers: libzmq DEALER + PUSH, with timeouts so they never block forever, so they can notice
  // the shutdown flag and exit. A `flaky_pct` fraction misbehave (die / bounce).
  let ctx = Arc::new(zmq::Context::new());
  let killed = Arc::new(AtomicUsize::new(0));
  let bounced = Arc::new(AtomicUsize::new(0));
  let mut handles = Vec::new();
  for w in 0..workers {
    let (ctx, router_ep, sink_ep) = (ctx.clone(), router_ep.clone(), sink_ep.clone());
    let (shutdown, killed, bounced) = (shutdown.clone(), killed.clone(), bounced.clone());
    handles.push(thread::spawn(move || {
      let nonce = w as u64;
      let flaky = (mix(nonce) % 100) < flaky_pct as u64;
      // flaky workers act up after a few tasks; ~half that do so "bounce" (reconnect) instead of
      // die.
      let act_after = 2 + (mix(nonce ^ 7) % 6);
      let bounce = flaky && mix(nonce ^ 9).is_multiple_of(2);
      let connect = |ctx: &zmq::Context| {
        let dealer = ctx.socket(zmq::DEALER).unwrap();
        dealer.set_identity(format!("w{w}").as_bytes()).unwrap();
        dealer.set_rcvtimeo(400).unwrap();
        dealer.set_sndtimeo(400).unwrap();
        dealer.connect(&router_ep).unwrap();
        let push = ctx.socket(zmq::PUSH).unwrap();
        push.set_sndtimeo(400).unwrap();
        push.connect(&sink_ep).unwrap();
        (dealer, push)
      };
      let (mut dealer, mut push) = connect(&ctx);
      let mut processed = 0u64;
      let mut bounced_once = false;
      loop {
        if shutdown.load(Ordering::Relaxed) {
          break;
        }
        let req: Vec<Vec<u8>> = vec![
          nonce.to_le_bytes().to_vec(),
          processed.to_le_bytes().to_vec(),
        ];
        let refs: Vec<&[u8]> = req.iter().map(|v| v.as_slice()).collect();
        if dealer.send_multipart(&refs, 0).is_err() {
          if shutdown.load(Ordering::Relaxed) {
            break;
          }
          continue;
        }
        // Flaky behavior trigger.
        if flaky && processed >= act_after && !(bounce && bounced_once) {
          if bounce {
            // Reconnect: drop the sockets and build fresh ones (tests ROUTER reconnect handling).
            drop(dealer);
            drop(push);
            let fresh = connect(&ctx);
            dealer = fresh.0;
            push = fresh.1;
            bounced_once = true;
            bounced.fetch_add(1, Ordering::Relaxed);
            continue;
          } else {
            // Die right after requesting — the ROUTER's reply now hits a dead peer (path a), or the
            // lease it just took out times out (path b). Either way the task must be recovered.
            killed.fetch_add(1, Ordering::Relaxed);
            break;
          }
        }
        let src = match dealer.recv_multipart(0) {
          Ok(m) => m,
          Err(_) => {
            if shutdown.load(Ordering::Relaxed) {
              break;
            }
            continue;
          },
        };
        let seq = u64::from_le_bytes(src[0][0..8].try_into().unwrap());
        if seq == RETRY_SENTINEL {
          thread::sleep(Duration::from_millis(15));
          continue;
        }
        // Return the result.
        let mut header = vec![0u8; 16];
        header[0..8].copy_from_slice(&seq.to_le_bytes());
        let mut body = vec![0u8; frame_bytes];
        body[0..8].copy_from_slice(&seq.to_le_bytes());
        let result = [header, body];
        let refs: Vec<&[u8]> = result.iter().map(|v| v.as_slice()).collect();
        let _ = push.send_multipart(&refs, 0);
        processed += 1;
      }
    }));
  }

  // Wait for completion (every task accounted for) with a hard deadline so a true hang fails
  // loudly.
  let outcome = tokio::time::timeout(Duration::from_secs(90), complete.notified()).await;
  shutdown.store(true, Ordering::Relaxed);
  ventilator.abort();
  reaper.abort();
  sink_task.abort();
  for h in handles {
    let _ = h.join();
  }
  let elapsed = start.elapsed();

  let completed = done.lock().unwrap().len();
  let secs = elapsed.as_secs_f64().max(1e-9);
  println!(
    "  {completed}/{tasks} tasks completed in {:.2}s ({:.0}/s); {} killed, {} bounced(reconnect); \
     recovery: {} reaper re-leases, {} dead-peer re-leases",
    secs,
    completed as f64 / secs,
    killed.load(Ordering::Relaxed),
    bounced.load(Ordering::Relaxed),
    re_leased.load(Ordering::Relaxed),
    dead_peer_recovered.load(Ordering::Relaxed),
  );
  match outcome {
    Ok(_) if completed == tasks => println!(
      "  ✓ every task recovered & completed under worker churn — no loss, no hang, no panic \
       (zeromq ROUTER + lease-timeout reaper)"
    ),
    _ => println!(
      "  ✗ FAILED: only {completed}/{tasks} completed before the deadline (possible loss/hang)"
    ),
  }
  Ok(())
}
