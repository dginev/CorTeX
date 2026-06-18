// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Dispatcher-rationalization SPIKE (throwaway; docs/archive/DISPATCHER_RATIONALIZATION.md). THE
// decisive interop test: if we move the **dispatcher** to the pure-Rust `zeromq` crate but leave
// the **workers** (`pericortex`, the external crate) on the libzmq `zmq` binding, the two must
// speak ZMTP to each other on the wire. zmq.rs claims it is "tested against the reference
// implementation" (libzmq); this spike *proves* it for our exact topology + a mixed arXiv-like
// payload, rather than trusting the README.
//
// Wire configuration mirrors a migrated production:
//   * OUR SIDE  — pure-Rust async `zeromq`: ROUTER ventilator + PULL sink (tokio).
//   * WORKERS   — libzmq `zmq` crate: DEALER (request source) + PUSH (return result), in OS
//     threads, each with an explicit ZMQ identity (as pericortex workers have).
// Same lease protocol + per-frame integrity (interleaving / reordering / misrouting detection) as
// `zmq_arxiv_workload`, so a green run here means a zeromq dispatcher and libzmq workers
// interoperate correctly under load.
//
// Run:
//   cargo run --release --example zmq_interop
//   WORKERS=200 TASKS=20000 PAYLOAD_KB=64 cargo run --release --example zmq_interop

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

use bytes::Bytes;
use zeromq::{PullSocket, RouterSocket, Socket, SocketRecv, SocketSend, ZmqMessage};

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
  let workers = env_usize("WORKERS", 20).max(1);
  let tasks = env_usize("TASKS", 2000).max(1);
  let frame_bytes = env_usize("PAYLOAD_KB", 64).max(1) * 1024;

  println!(
    "ZMTP interop spike: zeromq(pure-Rust) ROUTER+PULL  ↔  libzmq(zmq) {workers} DEALER+PUSH \
     workers, {tasks} tasks, heavy-tailed ({frame_bytes}B/frame)"
  );

  // OUR SIDE: zeromq PULL sink + ROUTER ventilator (fixed ports so the libzmq threads can connect).
  let mut sink = PullSocket::new();
  let sink_ep = sink.bind("tcp://127.0.0.1:0").await?.to_string();
  let mut router = RouterSocket::new();
  let router_ep = router.bind("tcp://127.0.0.1:0").await?.to_string();

  let sink_recv = tokio::spawn(async move {
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
  });

  let leased = Arc::new(AtomicUsize::new(0));
  let leased_v = leased.clone();
  let ventilator = tokio::spawn(async move {
    let mut errors: Vec<String> = Vec::new();
    let mut served_sentinels = 0usize;
    while served_sentinels < workers {
      match router.recv().await {
        Ok(req) => {
          let identity = req.get(0).cloned().unwrap();
          let nonce = u64::from_le_bytes(req.get(1).unwrap()[0..8].try_into().unwrap());
          let seq = leased_v.fetch_add(1, Ordering::Relaxed) as u64;
          let mut reply = ZmqMessage::from(identity.to_vec());
          if (seq as usize) < tasks {
            let frames = frame_count_for(seq);
            let mut header = vec![0u8; 24];
            header[0..8].copy_from_slice(&seq.to_le_bytes());
            header[8..16].copy_from_slice(&nonce.to_le_bytes());
            header[16..24].copy_from_slice(&(frames as u64).to_le_bytes());
            reply.push_back(Bytes::from(header));
            for idx in 0..frames as u32 {
              let mut buf = vec![0u8; frame_bytes.max(24)];
              buf[0..8].copy_from_slice(&seq.to_le_bytes());
              buf[8..12].copy_from_slice(&idx.to_le_bytes());
              buf[12..20].copy_from_slice(&nonce.to_le_bytes());
              reply.push_back(Bytes::from(buf));
            }
          } else {
            let mut header = vec![0u8; 24];
            header[0..8].copy_from_slice(&u64::MAX.to_le_bytes());
            reply.push_back(Bytes::from(header));
            served_sentinels += 1;
          }
          if let Err(e) = router.send(reply).await {
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
  });

  let start = Instant::now();

  // WORKERS: libzmq `zmq` crate — DEALER + PUSH on OS threads (the pericortex configuration).
  let ctx = Arc::new(zmq::Context::new());
  let anomaly_count = Arc::new(AtomicUsize::new(0));
  let mut handles = Vec::new();
  for w in 0..workers {
    let ctx = ctx.clone();
    let router_ep = router_ep.clone();
    let sink_ep = sink_ep.clone();
    let anomaly_count = anomaly_count.clone();
    handles.push(thread::spawn(move || {
      let nonce = w as u64;
      let dealer = ctx.socket(zmq::DEALER).expect("dealer");
      dealer
        .set_identity(format!("w{w}").as_bytes())
        .expect("identity");
      dealer.connect(&router_ep).expect("dealer connect");
      let push = ctx.socket(zmq::PUSH).expect("push");
      push.connect(&sink_ep).expect("push connect");
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
        // return result archive (heavy-tailed): [seq | k'] then k' frames [seq | idx]
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
  let (received, bytes_total, mut anomalies) = sink_recv.await?;
  if let Ok(mut v) = ventilator.await {
    anomalies.append(&mut v);
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
      "  ✓ zeromq ROUTER/PULL  ↔  libzmq DEALER/PUSH interoperate cleanly over ZMTP — no \
       interleaving/reordering/misrouting/loss across {received} tasks, {workers} workers"
    );
  } else {
    println!(
      "  ✗ interop FAILED: {worker_bad} worker-side frame anomalies, {} sink/router anomalies, {received}/{tasks} delivered",
      anomalies.len()
    );
    for a in anomalies.iter().take(8) {
      println!("    - {a}");
    }
  }
  Ok(())
}
