// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Phase-5b omq.rs EVALUATION SPIKE (throwaway; docs/DISPATCHER_5B_ROOT_CAUSE.md). The decisive test
// for replacing the dispatcher transport with the pure-Rust `omq-tokio` crate (which claims ~3x
// libzmq TCP throughput and is wire-compatible with libzmq), after `zmq.rs` 0.6 was found to have a
// crate-architecture send ceiling (~3000 tasks/s; zmq.rs issue #240).
//
// Two questions, answered together:
//   1. INTEROP — do the **unchanged libzmq workers** (`zmq` crate, DEALER + PUSH — exactly what
//      `pericortex` uses) speak ZMTP to an `omq-tokio` ROUTER + PULL dispatcher? (Owner ask: "keep
//      the same conventions for the workers.")
//   2. THROUGHPUT — does it clear the zmq.rs ~3000/s ceiling and approach/beat libzmq's ~8500/s?
//
// Mirrors `examples/zmq_interop.rs` exactly (same lease protocol, heavy-tailed payload, per-frame
// integrity check), with OUR SIDE swapped from `zeromq` to `omq-tokio`; the worker side is
// byte-for- byte the libzmq code from `zmq_interop`.
//
// Run:
//   cargo run --release --example omq_interop
//   WORKERS=64 TASKS=20000 PAYLOAD_KB=64 cargo run --release --example omq_interop

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use bytes::Bytes;
use omq_tokio::{Message, Options, Socket, SocketType};

const ROUTER_PORT: u16 = 53711; // distinct from dispatcher_bench (53697/8) + bench_pipeline (53695/6)
const SINK_PORT: u16 = 53712;

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

fn frame_count_for(seq: u64) -> usize {
  let h = mix(seq);
  match h % 100 {
    0..=79 => 1 + (h % 3) as usize,
    80..=96 => 16 + (h % 32) as usize,
    _ => 80 + (h % 80) as usize,
  }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let workers = env_usize("WORKERS", 200).max(1);
  let tasks = env_usize("TASKS", 2000).max(1);
  let frame_bytes = env_usize("PAYLOAD_KB", 64).max(1) * 1024;

  println!(
    "omq interop spike: omq-tokio(pure-Rust) ROUTER+PULL  ↔  libzmq(zmq) {workers} DEALER+PUSH \
     workers, {tasks} tasks, heavy-tailed ({frame_bytes}B/frame)"
  );

  // OUR SIDE: omq-tokio PULL sink + ROUTER ventilator on fixed ports (workers connect to these).
  let sink = Socket::new(SocketType::Pull, Options::default());
  sink
    .bind(format!("tcp://127.0.0.1:{SINK_PORT}").parse()?)
    .await?;
  let router = Socket::new(SocketType::Router, Options::default());
  router
    .bind(format!("tcp://127.0.0.1:{ROUTER_PORT}").parse()?)
    .await?;

  // Sink receive task (PULL): count `tasks` results, verify per-frame integrity.
  let sink_recv = {
    let sink = sink.clone();
    tokio::spawn(async move {
      let mut received = 0usize;
      let mut bytes_total = 0u64;
      let mut anomalies: Vec<String> = Vec::new();
      while received < tasks {
        match sink.recv().await {
          Ok(msg) => {
            bytes_total += msg.iter().map(|f| f.len() as u64).sum::<u64>();
            let seq = u64::from_le_bytes(msg.get(0).unwrap()[0..8].try_into().unwrap());
            for (idx, frame) in msg.iter().skip(1).enumerate() {
              let fseq = u64::from_le_bytes(frame[0..8].try_into().unwrap());
              let fidx = u32::from_le_bytes(frame[8..12].try_into().unwrap());
              if fseq != seq || fidx as usize != idx {
                anomalies.push(format!(
                  "RESULT corruption: seq {seq} pos {idx} ({fseq},{fidx})"
                ));
              }
            }
            received += 1;
          },
          Err(e) => {
            anomalies.push(format!("sink recv error: {e}"));
            break;
          },
        }
      }
      (received, bytes_total, anomalies)
    })
  };

  // Concurrent ventilator tasks sharing ONE clonable ROUTER socket — omq's `&self` send/recv lets
  // us parallelize the request/reply round-trips (zmq.rs's split couldn't). `VENT_TASKS` controls
  // it.
  let vent_tasks = env_usize("VENT_TASKS", 1).max(1);
  let leased = Arc::new(AtomicUsize::new(0));
  let sentinels = Arc::new(AtomicUsize::new(0));
  let mut vent_handles = Vec::new();
  for _ in 0..vent_tasks {
    let router = router.clone();
    let leased = leased.clone();
    let sentinels = sentinels.clone();
    vent_handles.push(tokio::spawn(async move {
      let mut errors: Vec<String> = Vec::new();
      while sentinels.load(Ordering::Relaxed) < workers {
        match router.recv().await {
          Ok(req) => {
            let identity = req.get(0).unwrap().to_vec();
            let nonce = u64::from_le_bytes(req.get(1).unwrap()[0..8].try_into().unwrap());
            let seq = leased.fetch_add(1, Ordering::Relaxed) as u64;
            let mut parts: Vec<Bytes> = vec![Bytes::from(identity)];
            if (seq as usize) < tasks {
              let frames = frame_count_for(seq);
              let mut header = vec![0u8; 24];
              header[0..8].copy_from_slice(&seq.to_le_bytes());
              header[8..16].copy_from_slice(&nonce.to_le_bytes());
              header[16..24].copy_from_slice(&(frames as u64).to_le_bytes());
              parts.push(Bytes::from(header));
              for idx in 0..frames as u32 {
                let mut buf = vec![0u8; frame_bytes.max(24)];
                buf[0..8].copy_from_slice(&seq.to_le_bytes());
                buf[8..12].copy_from_slice(&idx.to_le_bytes());
                buf[12..20].copy_from_slice(&nonce.to_le_bytes());
                parts.push(Bytes::from(buf));
              }
            } else {
              let mut header = vec![0u8; 24];
              header[0..8].copy_from_slice(&u64::MAX.to_le_bytes());
              parts.push(Bytes::from(header));
              sentinels.fetch_add(1, Ordering::Relaxed);
            }
            if let Err(e) = router.send(Message::multipart(parts)).await {
              errors.push(format!("router send: {e}"));
            }
          },
          Err(e) => {
            errors.push(format!("router recv: {e}"));
            break;
          },
        }
      }
      errors
    }));
  }

  let start = Instant::now();

  // WORKERS: libzmq `zmq` crate — DEALER + PUSH on OS threads (byte-for-byte the pericortex shape,
  // copied verbatim from zmq_interop.rs — UNCHANGED, proving the workers keep the same
  // conventions).
  let ctx = Arc::new(zmq::Context::new());
  let anomaly_count = Arc::new(AtomicUsize::new(0));
  let mut handles = Vec::new();
  for w in 0..workers {
    let ctx = ctx.clone();
    let anomaly_count = anomaly_count.clone();
    handles.push(thread::spawn(move || {
      let nonce = w as u64;
      let dealer = ctx.socket(zmq::DEALER).expect("dealer");
      dealer
        .set_identity(format!("w{w}").as_bytes())
        .expect("identity");
      dealer
        .connect(&format!("tcp://127.0.0.1:{ROUTER_PORT}"))
        .expect("dealer connect");
      let push = ctx.socket(zmq::PUSH).expect("push");
      push
        .connect(&format!("tcp://127.0.0.1:{SINK_PORT}"))
        .expect("push connect");
      let mut done = 0usize;
      loop {
        let req_owned: Vec<Vec<u8>> = vec![
          nonce.to_le_bytes().to_vec(),
          (done as u64).to_le_bytes().to_vec(),
        ];
        let req_refs: Vec<&[u8]> = req_owned.iter().map(|v| v.as_slice()).collect();
        if dealer.send_multipart(&req_refs, 0).is_err() {
          break;
        }
        let src = match dealer.recv_multipart(0) {
          Ok(m) => m,
          Err(_) => break,
        };
        let header = &src[0];
        let seq = u64::from_le_bytes(header[0..8].try_into().unwrap());
        if seq == u64::MAX {
          break;
        }
        let echoed = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let k = u64::from_le_bytes(header[16..24].try_into().unwrap()) as usize;
        let mut bad = 0usize;
        if echoed != nonce || src.len() != k + 1 {
          bad += 1;
        }
        for (idx, frame) in src.iter().skip(1).enumerate() {
          let fseq = u64::from_le_bytes(frame[0..8].try_into().unwrap());
          let fidx = u32::from_le_bytes(frame[8..12].try_into().unwrap());
          let fnonce = u64::from_le_bytes(frame[12..20].try_into().unwrap());
          if fseq != seq || fidx as usize != idx || fnonce != nonce {
            bad += 1;
          }
        }
        if bad > 0 {
          anomaly_count.fetch_add(bad, Ordering::Relaxed);
        }
        let rk = frame_count_for(seq ^ 0x5555);
        let mut result: Vec<Vec<u8>> = Vec::with_capacity(rk + 1);
        let mut h = vec![0u8; 16];
        h[0..8].copy_from_slice(&seq.to_le_bytes());
        h[8..16].copy_from_slice(&(rk as u64).to_le_bytes());
        result.push(h);
        for idx in 0..rk as u32 {
          let mut buf = vec![0u8; frame_bytes.max(12)];
          buf[0..8].copy_from_slice(&seq.to_le_bytes());
          buf[8..12].copy_from_slice(&idx.to_le_bytes());
          result.push(buf);
        }
        let refs: Vec<&[u8]> = result.iter().map(|v| v.as_slice()).collect();
        if push.send_multipart(&refs, 0).is_err() {
          break;
        }
        done += 1;
      }
    }));
  }

  for h in handles {
    let _ = h.join();
  }
  let (received, bytes_total, anomalies) = sink_recv.await?;
  // The measurement is complete once the sink has all results; any leftover ventilator tasks are
  // blocked on `recv()` (no more worker requests) — abort them (omq recv is cancel-safe) rather
  // than await (which would hang). Send/recv errors would have shown up as missing results above.
  for handle in &vent_handles {
    handle.abort();
  }
  let worker_bad = anomaly_count.load(Ordering::Relaxed);
  let elapsed = start.elapsed();
  let secs = elapsed.as_secs_f64().max(1e-9);

  println!(
    "  {received}/{tasks} results in {:.3}s → {:.0} tasks/s, {:.1} MB/s; leased {}",
    secs,
    received as f64 / secs,
    bytes_total as f64 / secs / 1_048_576.0,
    leased.load(Ordering::Relaxed),
  );
  if anomalies.is_empty() && worker_bad == 0 && received == tasks {
    println!(
      "  ✓ omq-tokio ROUTER/PULL  ↔  libzmq DEALER/PUSH interoperate cleanly over ZMTP — no \
       interleaving/reordering/misrouting/loss across {received} tasks, {workers} workers"
    );
  } else {
    println!("  ✗ interop FAILED: {worker_bad} worker-side frame anomalies, {} sink/router anomalies, {received}/{tasks} delivered", anomalies.len());
    for a in anomalies.iter().take(8) {
      println!("    - {a}");
    }
  }
  Ok(())
}
