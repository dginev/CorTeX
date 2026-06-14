// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Dispatcher-rationalization SPIKE (throwaway; docs/DISPATCHER_RATIONALIZATION.md). The
// **definitive pre-cutover torture test** for the pure-Rust `zeromq` transport + the rationalized
// pipeline (bounded channel → batched DB finalize). Approximates real arXiv conditions, per owner
// spec:
//
//   1. FLAKY NETWORK    — consumers randomly disconnect + reconnect mid-stream.
//   2. VARIABLE SIZES   — job bytes drawn from a calibrated **log-normal**: 500 KB … 200 MB,
//      **median 800 KB, mean ~1.5 MB** (a rare-giant injector guarantees the 200 MB tail is
//      exercised even in a short run). Chunked into ZMQ frames, so a giant is an 800-frame
//      multipart message.
//   3. CROSS-TALK       — hundreds of simultaneous consumers round-tripping; every frame is stamped
//      and re-verified so any interleaving / reordering / misrouting is caught.
//   4. TIMEOUT FLAKINESS— some consumers sleep an intended 10 s … 45 min (capped for runnability),
//      blowing past the max reply time → their lease must be re-issued.
//   5. SLOW/UNRELIABLE DB— the **batch finalize** sleeps a random latency **up to 15 s per batch**,
//      so the bounded sink→finalize channel must backpressure without loss/OOM.
//
// SUCCESS = every task is persisted exactly once (no loss; duplicates from re-leases deduped), zero
// integrity anomalies, no hang/OOM/panic — under all five stressors at once.
//
// Run:
//   cargo run --release --example zmq_torture
//   CONSUMERS=400 TASKS=4000 DB_MAX_LATENCY_MS=15000 cargo run --release --example zmq_torture

use std::collections::{HashMap, HashSet, VecDeque};
use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use zeromq::{PullSocket, RouterSocket, Socket, SocketRecv, SocketSend, ZmqMessage};

const RETRY_SENTINEL: u64 = u64::MAX;

fn env_usize(key: &str, default: usize) -> usize {
  std::env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}
fn env_u64(key: &str, default: u64) -> u64 {
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
/// A deterministic uniform in (0,1) from a 64-bit hash (for the Box–Muller draw; no `rand`).
fn u01(h: u64) -> f64 { ((h >> 11) as f64 / (1u64 << 53) as f64).clamp(1e-12, 1.0 - 1e-12) }

/// Drawn job size in bytes for task `seq`: a log-normal calibrated to **median 800 KB, mean ~1.5
/// MB** (μ = ln(800 KB), σ = 1.121 ⇒ mean = median·e^(σ²/2) ≈ 1.875·800 KB), clamped to [500 KB,
/// MAX]. A `giant_bp`-basis-point fraction is instead drawn uniformly in [50 MB, MAX] so the
/// extreme tail is actually torture-tested (the pure log-normal puts 200 MB at ~5σ — too rare to
/// ever appear).
fn job_bytes(seq: u64, max_bytes: u64, giant_bp: u64) -> u64 {
  if giant_bp > 0 && (mix(seq ^ 0x6161_6161) % 10_000) < giant_bp {
    let span = (max_bytes - 50 * 1024 * 1024).max(1);
    return 50 * 1024 * 1024 + mix(seq ^ 0x6262_6262) % span;
  }
  let z = (-2.0 * u01(mix(seq)).ln()).sqrt() * (2.0 * PI * u01(mix(seq ^ 0xABCD_EF01))).cos();
  let mu = (800.0 * 1024.0_f64).ln();
  let bytes = (mu + 1.121 * z).exp();
  (bytes as u64).clamp(500 * 1024, max_bytes)
}

/// Frames for `bytes` at `frame_bytes` per frame (≥1; a 200 MB job at 256 KB ⇒ 800 frames).
fn frames_for(bytes: u64, frame_bytes: usize) -> usize {
  (bytes as usize).div_ceil(frame_bytes).max(1)
}

/// Sleep that re-checks the shutdown flag every 200 ms (so end-of-run join doesn't wait seconds).
fn interruptible_sleep(total_ms: u64, shutdown: &AtomicBool) {
  let mut slept = 0u64;
  while slept < total_ms && !shutdown.load(Ordering::Relaxed) {
    let step = 200.min(total_ms - slept);
    thread::sleep(Duration::from_millis(step));
    slept += step;
  }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let consumers = env_usize("CONSUMERS", 250).max(1);
  let tasks = env_usize("TASKS", 2000).max(1);
  let frame_bytes = env_usize("FRAME_KB", 256).max(1) * 1024;
  let max_bytes = env_u64("MAX_MB", 200) * 1024 * 1024;
  let giant_bp = env_u64("GIANT_BP", 20); // 0.20% forced giants (50–200 MB)
  let flaky_net_pct = env_u64("FLAKY_NET_PCT", 25);
  let flaky_to_pct = env_u64("FLAKY_TIMEOUT_PCT", 20);
  let disconnect_permille = env_u64("DISCONNECT_PERMILLE", 40);
  let sleep_permille = env_u64("SLEEP_PERMILLE", 120);
  let db_batch = env_usize("DB_BATCH", 512).max(1);
  let db_max_latency_ms = env_u64("DB_MAX_LATENCY_MS", 15_000);
  let lease_timeout = Duration::from_millis(env_u64("LEASE_TIMEOUT_MS", 4000));
  let sleep_cap_ms = env_u64("SLEEP_CAP_MS", 8000); // > lease_timeout, < forever (runnability)
  let deadline = Duration::from_secs(env_u64("DEADLINE_S", 300));

  // Report the realized payload distribution (the "designed payload set"), so we can confirm it
  // matches the target before trusting the run.
  {
    let mut sizes: Vec<u64> = (0..tasks as u64)
      .map(|s| job_bytes(s, max_bytes, giant_bp))
      .collect();
    let total: u128 = sizes.iter().map(|&b| b as u128).sum();
    sizes.sort_unstable();
    let pct = |p: f64| sizes[((p * (sizes.len() - 1) as f64) as usize).min(sizes.len() - 1)];
    let giants = sizes.iter().filter(|&&b| b >= 50 * 1024 * 1024).count();
    let kb = |b: u64| b as f64 / 1024.0;
    let mb = |b: u64| b as f64 / 1048576.0;
    println!(
      "torture payload set: {tasks} jobs — min {:.0}KB · p50 {:.0}KB · mean {:.0}KB · p99 {:.1}MB · \
       max {:.1}MB; {giants} giants ≥50MB",
      kb(sizes[0]),
      kb(pct(0.50)),
      (total / tasks as u128) as f64 / 1024.0,
      mb(pct(0.99)),
      mb(sizes[sizes.len() - 1]),
    );
  }
  println!(
    "  stressors: {consumers} consumers · flaky-net {flaky_net_pct}% (disc {disconnect_permille}‰) · \
     timeout-flaky {flaky_to_pct}% (sleep 10s–45min, cap {}s) · DB batch {db_batch} latency ≤{}s · \
     lease {}s",
    sleep_cap_ms / 1000,
    db_max_latency_ms / 1000,
    lease_timeout.as_secs()
  );

  // Shared dispatch state. Lock order never nests two of {todo,in_flight,done}.
  let todo: Arc<Mutex<VecDeque<u64>>> = Arc::new(Mutex::new((0..tasks as u64).collect()));
  let in_flight: Arc<Mutex<HashMap<u64, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
  let done: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
  let anomalies = Arc::new(AtomicUsize::new(0));
  let re_leased = Arc::new(AtomicUsize::new(0));
  let dead_peer = Arc::new(AtomicUsize::new(0));
  let reconnects = Arc::new(AtomicUsize::new(0));
  let sleeper_events = Arc::new(AtomicUsize::new(0));
  let dup_results = Arc::new(AtomicUsize::new(0));
  let shutdown = Arc::new(AtomicBool::new(false));
  let complete = Arc::new(tokio::sync::Notify::new());

  // Bounded sink→finalize channel: the backpressure path. A slow DB makes this fill, blocking the
  // sink's ZMQ recv, which backs up the workers' PUSH — no unbounded buffering, no loss.
  let (fin_tx, mut fin_rx) = tokio::sync::mpsc::channel::<u64>(db_batch * 4);

  // FINALIZE: batch-drain the channel and "persist" each batch with a random DB latency ≤15 s.
  // `done` (persisted, deduped) is set here — not at the sink — so completion means *durably
  // stored*.
  let finalize = {
    let (done, dup_results, complete) = (done.clone(), dup_results.clone(), complete.clone());
    tokio::spawn(async move {
      let mut batch_num = 0u64;
      while let Some(first) = fin_rx.recv().await {
        let mut batch = vec![first];
        while batch.len() < db_batch {
          match fin_rx.try_recv() {
            Ok(s) => batch.push(s),
            Err(_) => break,
          }
        }
        // Mock unreliable DB load: this batch's multi-row INSERT takes up to db_max_latency_ms.
        let latency = mix(batch_num ^ 0xDB).rem_euclid(db_max_latency_ms + 1);
        batch_num += 1;
        tokio::time::sleep(Duration::from_millis(latency)).await;
        let mut d = done.lock().unwrap();
        for seq in batch {
          if !d.insert(seq) {
            dup_results.fetch_add(1, Ordering::Relaxed);
          }
        }
        let n = d.len();
        drop(d);
        if n >= tasks {
          complete.notify_one();
          break;
        }
      }
    })
  };

  // SINK: PULL. Verify result integrity, clear the worker lease on *receipt* (so a slow DB does not
  // trigger spurious re-leases), then hand off to finalize (may block → backpressure).
  let mut sink = PullSocket::new();
  let sink_ep = sink.bind("tcp://127.0.0.1:0").await?.to_string();
  let sink_task = {
    let (in_flight, anomalies) = (in_flight.clone(), anomalies.clone());
    tokio::spawn(async move {
      while let Ok(msg) = sink.recv().await {
        let header = msg.get(0).unwrap();
        let seq = u64::from_le_bytes(header[0..8].try_into().unwrap());
        let nframes = u64::from_le_bytes(header[8..16].try_into().unwrap()) as usize;
        let mut bad = msg.len() != nframes + 1;
        for (idx, f) in msg.iter().skip(1).enumerate() {
          if u64::from_le_bytes(f[0..8].try_into().unwrap()) != seq
            || u32::from_le_bytes(f[8..12].try_into().unwrap()) as usize != idx
          {
            bad = true;
          }
        }
        if bad {
          anomalies.fetch_add(1, Ordering::Relaxed);
        }
        in_flight.lock().unwrap().remove(&seq);
        if fin_tx.send(seq).await.is_err() {
          break;
        }
      }
    })
  };

  // VENTILATOR: ROUTER. Lease a task, stream its (variable, possibly-giant) source; re-lease at
  // once if the reply can't be delivered (requester already vanished).
  let mut router = RouterSocket::new();
  let router_ep = router.bind("tcp://127.0.0.1:0").await?.to_string();
  let ventilator = {
    let (todo, in_flight, done, dead_peer) = (
      todo.clone(),
      in_flight.clone(),
      done.clone(),
      dead_peer.clone(),
    );
    tokio::spawn(async move {
      while let Ok(req) = router.recv().await {
        let identity = req.get(0).cloned().unwrap();
        let nonce = u64::from_le_bytes(req.get(1).unwrap()[0..8].try_into().unwrap());
        let next = todo.lock().unwrap().pop_front();
        let mut reply = ZmqMessage::from(identity.to_vec());
        match next {
          Some(seq) => {
            in_flight.lock().unwrap().insert(seq, Instant::now());
            let nframes = frames_for(job_bytes(seq, max_bytes, giant_bp), frame_bytes);
            let mut header = vec![0u8; 32];
            header[0..8].copy_from_slice(&seq.to_le_bytes());
            header[8..16].copy_from_slice(&nonce.to_le_bytes());
            header[16..24].copy_from_slice(&(nframes as u64).to_le_bytes());
            reply.push_back(Bytes::from(header));
            for idx in 0..nframes as u32 {
              let mut buf = vec![0u8; frame_bytes];
              buf[0..8].copy_from_slice(&seq.to_le_bytes());
              buf[8..12].copy_from_slice(&idx.to_le_bytes());
              buf[12..20].copy_from_slice(&nonce.to_le_bytes());
              reply.push_back(Bytes::from(buf));
            }
            if router.send(reply).await.is_err() {
              in_flight.lock().unwrap().remove(&seq);
              todo.lock().unwrap().push_front(seq);
              dead_peer.fetch_add(1, Ordering::Relaxed);
            }
          },
          None => {
            if done.lock().unwrap().len() >= tasks {
              break;
            }
            let mut header = vec![0u8; 32];
            header[0..8].copy_from_slice(&RETRY_SENTINEL.to_le_bytes());
            reply.push_back(Bytes::from(header));
            let _ = router.send(reply).await;
          },
        }
      }
    })
  };

  // REAPER: re-lease any worker lease older than the timeout (the recovery net for crashed +
  // sleeping workers; transport-agnostic, so no ZMTP heartbeat needed for correctness).
  let reaper = {
    let (todo, in_flight, done, re_leased) = (
      todo.clone(),
      in_flight.clone(),
      done.clone(),
      re_leased.clone(),
    );
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let now = Instant::now();
        let expired: Vec<u64> = {
          let mut inf = in_flight.lock().unwrap();
          let ex: Vec<u64> = inf
            .iter()
            .filter(|(_, t)| now.duration_since(**t) > lease_timeout)
            .map(|(s, _)| *s)
            .collect();
          for s in &ex {
            inf.remove(s);
          }
          ex
        };
        if !expired.is_empty() {
          let mut td = todo.lock().unwrap();
          for s in &expired {
            td.push_front(*s);
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

  // CONSUMERS: libzmq DEALER + PUSH (the pericortex config), hundreds of them, each with a mix of
  // flaky traits. They verify the source they receive, then return a (also-variable) result.
  let ctx = Arc::new(zmq::Context::new());
  let mut handles = Vec::new();
  for w in 0..consumers {
    let (ctx, router_ep, sink_ep) = (ctx.clone(), router_ep.clone(), sink_ep.clone());
    let (shutdown, anomalies) = (shutdown.clone(), anomalies.clone());
    let (reconnects, sleeper_events) = (reconnects.clone(), sleeper_events.clone());
    handles.push(thread::spawn(move || {
      let nonce = w as u64;
      let net_flaky = (mix(nonce) % 100) < flaky_net_pct;
      let timeout_flaky = (mix(nonce ^ 0x1234) % 100) < flaky_to_pct;
      let connect = |ctx: &zmq::Context| {
        let dealer = ctx.socket(zmq::DEALER).unwrap();
        dealer.set_identity(format!("w{w}").as_bytes()).unwrap();
        dealer.set_rcvtimeo(500).unwrap();
        dealer.set_sndtimeo(500).unwrap();
        dealer.connect(&router_ep).unwrap();
        let push = ctx.socket(zmq::PUSH).unwrap();
        push.set_sndtimeo(2000).unwrap();
        push.connect(&sink_ep).unwrap();
        (dealer, push)
      };
      let (mut dealer, mut push) = connect(&ctx);
      let mut iter = 0u64;
      loop {
        if shutdown.load(Ordering::Relaxed) {
          break;
        }
        iter += 1;
        // (1) Flaky network: random disconnect → reconnect a fresh socket pair.
        if net_flaky && (mix(nonce ^ iter.wrapping_mul(0x9E37)) % 1000) < disconnect_permille {
          drop(dealer);
          drop(push);
          let f = connect(&ctx);
          dealer = f.0;
          push = f.1;
          reconnects.fetch_add(1, Ordering::Relaxed);
        }
        let req: Vec<Vec<u8>> = vec![nonce.to_le_bytes().to_vec(), iter.to_le_bytes().to_vec()];
        let refs: Vec<&[u8]> = req.iter().map(|v| v.as_slice()).collect();
        if dealer.send_multipart(&refs, 0).is_err() {
          continue;
        }
        let src = match dealer.recv_multipart(0) {
          Ok(m) => m,
          Err(_) => continue, // timeout/EAGAIN → loop, re-check shutdown
        };
        let seq = u64::from_le_bytes(src[0][0..8].try_into().unwrap());
        if seq == RETRY_SENTINEL {
          thread::sleep(Duration::from_millis(15));
          continue;
        }
        // Verify the source (cross-talk / misrouting detection).
        let echoed = u64::from_le_bytes(src[0][8..16].try_into().unwrap());
        let nframes = u64::from_le_bytes(src[0][16..24].try_into().unwrap()) as usize;
        let mut bad = echoed != nonce || src.len() != nframes + 1;
        for (idx, f) in src.iter().skip(1).enumerate() {
          if u64::from_le_bytes(f[0..8].try_into().unwrap()) != seq
            || u32::from_le_bytes(f[8..12].try_into().unwrap()) as usize != idx
            || u64::from_le_bytes(f[12..20].try_into().unwrap()) != nonce
          {
            bad = true;
          }
        }
        if bad {
          anomalies.fetch_add(1, Ordering::Relaxed);
        }
        // (4) Timeout flakiness: sometimes sleep past the max reply time (intended 10 s … 45 min,
        // capped for the test). The reaper re-leases this task; our late result is deduped.
        if timeout_flaky && (mix(nonce ^ iter ^ 0x5A5A) % 1000) < sleep_permille {
          let intended = 10_000 + mix(nonce ^ iter) % (45 * 60_000 - 10_000); // 10s..45min
          sleeper_events.fetch_add(1, Ordering::Relaxed);
          interruptible_sleep(intended.min(sleep_cap_ms), &shutdown);
        }
        // Return a result of (independently) variable size.
        let rframes = frames_for(
          job_bytes(seq ^ 0x5555_5555, max_bytes, giant_bp),
          frame_bytes,
        );
        let mut header = vec![0u8; 16];
        header[0..8].copy_from_slice(&seq.to_le_bytes());
        header[8..16].copy_from_slice(&(rframes as u64).to_le_bytes());
        let mut result = vec![header];
        for idx in 0..rframes as u32 {
          let mut buf = vec![0u8; frame_bytes];
          buf[0..8].copy_from_slice(&seq.to_le_bytes());
          buf[8..12].copy_from_slice(&idx.to_le_bytes());
          result.push(buf);
        }
        let refs: Vec<&[u8]> = result.iter().map(|v| v.as_slice()).collect();
        let _ = push.send_multipart(&refs, 0);
      }
    }));
  }

  let outcome = tokio::time::timeout(deadline, complete.notified()).await;
  shutdown.store(true, Ordering::Relaxed);
  ventilator.abort();
  reaper.abort();
  sink_task.abort();
  finalize.abort();
  for h in handles {
    let _ = h.join();
  }
  let secs = start.elapsed().as_secs_f64().max(1e-9);
  let completed = done.lock().unwrap().len();

  println!(
    "  {completed}/{tasks} persisted in {:.1}s ({:.0}/s); recovery: {} reaper + {} dead-peer \
     re-leases; churn: {} reconnects, {} sleeper-misses, {} dup results deduped; integrity: {} \
     anomalies",
    secs,
    completed as f64 / secs,
    re_leased.load(Ordering::Relaxed),
    dead_peer.load(Ordering::Relaxed),
    reconnects.load(Ordering::Relaxed),
    sleeper_events.load(Ordering::Relaxed),
    dup_results.load(Ordering::Relaxed),
    anomalies.load(Ordering::Relaxed),
  );
  let ok = outcome.is_ok() && completed == tasks && anomalies.load(Ordering::Relaxed) == 0;
  if ok {
    println!(
      "  ✓ TORTURE PASSED: every task persisted exactly once, zero corruption, no hang/OOM — under \
       flaky network + 500KB–200MB payloads + {consumers}-way cross-talk + timeout sleepers + a DB \
       stalling up to {}s",
      db_max_latency_ms / 1000
    );
  } else {
    println!(
      "  ✗ TORTURE FAILED: completed={completed}/{tasks}, anomalies={}, timed_out={}",
      anomalies.load(Ordering::Relaxed),
      outcome.is_err()
    );
  }
  Ok(())
}
