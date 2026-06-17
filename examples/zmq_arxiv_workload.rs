// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Dispatcher-rationalization SPIKE (throwaway; docs/archive/DISPATCHER_RATIONALIZATION.md).
// Validates the **pure-Rust async `zeromq` crate** against CorTeX's *full* dispatch topology under
// a **mixed, arXiv-like (heavy-tailed) payload distribution** — the validation the owner asked for
// before committing to drop the libzmq C dependency.
//
// CorTeX's real ZMQ topology (confirmed from src/): the ventilator is a **ROUTER** (:51695) that
// leases source archives to **DEALER** workers (`src/worker.rs`), which return result archives via
// **PUSH** to the dispatcher's **PULL** sink (:51696). zeromq 0.6 implements all four socket types;
// this spike exercises every one of them at once, the way production does — which the earlier
// PUSH/PULL-only `zmq_payload_*` spikes did not.
//
// Modeled faithfully:
//   * ROUTER ventilator ← N concurrent DEALER workers (lease-on-request), reply routed by identity.
//   * Heavy-tailed payload mix (≈80% small 64–192 KB, ≈17% medium 1–3 MB, ≈3% large 5–10 MB) — the
//     arXiv reality where most papers are tiny and a rare few are huge.
//   * Integrity: every source frame is stamped `[seq | frame_idx | echoed worker-nonce]`, so a
//     worker detects **interleaving** (wrong seq), **reordering** (wrong idx), and **misrouting**
//     (a reply meant for another worker — the ROUTER's core guarantee). Results carry `[seq |
//     frame_idx]` and the PULL sink re-verifies them.
//
// Run (vary the workload freely):
//   cargo run --release --example zmq_arxiv_workload
//   WORKERS=200 TASKS=20000 PAYLOAD_KB=64 cargo run --release --example zmq_arxiv_workload

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use bytes::Bytes;
use zeromq::{
  DealerSocket, PullSocket, PushSocket, RouterSocket, Socket, SocketRecv, SocketSend, ZmqMessage,
};

fn env_usize(key: &str, default: usize) -> usize {
  std::env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

/// Deterministic splitmix64 — a reproducible "mixed workload" without `rand`/`Math::random`
/// (unavailable here, and we want re-runnable size draws keyed by the task seq).
fn mix(mut x: u64) -> u64 {
  x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
  let mut z = x;
  z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  z ^ (z >> 31)
}

/// arXiv-like heavy-tailed frame count for task `seq`: most papers tiny, a rare few huge.
fn frame_count_for(seq: u64) -> usize {
  let h = mix(seq);
  match h % 100 {
    0..=79 => 1 + (h % 3) as usize,    // small: 1–3 frames (≈64–192 KB)
    80..=96 => 16 + (h % 32) as usize, // medium: 16–47 frames (≈1–3 MB)
    _ => 80 + (h % 80) as usize,       // large: 80–159 frames (≈5–10 MB)
  }
}

/// Builds the source payload the ventilator streams to a worker: header frame `[seq | nonce | k]`
/// then `k` body frames each `[seq | frame_idx | nonce | filler]`. The echoed `nonce` lets the
/// worker confirm the reply was routed to *it* (misrouting detection); `seq`/`frame_idx` catch
/// interleaving/reordering.
fn source_payload(seq: u64, nonce: u64, frames: usize, frame_bytes: usize) -> Vec<Bytes> {
  let mut out = Vec::with_capacity(frames + 1);
  let mut header = vec![0u8; 24];
  header[0..8].copy_from_slice(&seq.to_le_bytes());
  header[8..16].copy_from_slice(&nonce.to_le_bytes());
  header[16..24].copy_from_slice(&(frames as u64).to_le_bytes());
  out.push(Bytes::from(header));
  for idx in 0..frames as u32 {
    let mut buf = vec![0u8; frame_bytes.max(24)];
    buf[0..8].copy_from_slice(&seq.to_le_bytes());
    buf[8..12].copy_from_slice(&idx.to_le_bytes());
    buf[12..20].copy_from_slice(&nonce.to_le_bytes());
    out.push(Bytes::from(buf));
  }
  out
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let workers = env_usize("WORKERS", 20).max(1);
  let tasks = env_usize("TASKS", 2000).max(1);
  let frame_bytes = env_usize("PAYLOAD_KB", 64).max(1) * 1024;

  println!(
    "zeromq arXiv-like full-topology spike: ROUTER↔{workers} DEALER workers + PUSH→PULL sink, \
     {tasks} tasks, heavy-tailed payloads ({frame_bytes}B/frame)"
  );

  // Sink: PULL, binds first. Verifies each returned result's frames, counts bytes + anomalies.
  let mut sink = PullSocket::new();
  let sink_ep = sink.bind("tcp://127.0.0.1:0").await?.to_string();
  let sink_recv = tokio::spawn(async move {
    let mut received = 0usize;
    let mut bytes_total = 0u64;
    let mut anomalies: Vec<String> = Vec::new();
    while received < tasks {
      match sink.recv().await {
        Ok(msg) => {
          bytes_total += msg.iter().map(|f| f.len() as u64).sum::<u64>();
          // result = [seq | k] then k frames [seq | idx]
          let seq = u64::from_le_bytes(msg.get(0).unwrap()[0..8].try_into().unwrap());
          for (idx, frame) in msg.iter().skip(1).enumerate() {
            let fseq = u64::from_le_bytes(frame[0..8].try_into().unwrap());
            let fidx = u32::from_le_bytes(frame[8..12].try_into().unwrap());
            if fseq != seq {
              anomalies.push(format!(
                "RESULT INTERLEAVING: seq {seq} frame {idx} carries {fseq}"
              ));
            }
            if fidx as usize != idx {
              anomalies.push(format!("RESULT REORDER: seq {seq} pos {idx} idx {fidx}"));
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

  // Ventilator: ROUTER, binds; leases tasks on request, replies routed to the requesting identity.
  let mut router = RouterSocket::new();
  let router_ep = router.bind("tcp://127.0.0.1:0").await?.to_string();
  let leased = Arc::new(AtomicUsize::new(0));
  let leased_v = leased.clone();
  let ventilator = tokio::spawn(async move {
    let mut errors: Vec<String> = Vec::new();
    // Each task is handed out once; after `tasks` are leased, every further request gets a 1-frame
    // "drain" sentinel (header seq = u64::MAX) so its worker stops.
    let mut served_sentinels = 0usize;
    loop {
      if served_sentinels >= workers {
        break; // every worker has been told to stop
      }
      match router.recv().await {
        Ok(req) => {
          // req = [identity | nonce | req_seq]
          let identity = req.get(0).cloned().unwrap();
          let nonce = u64::from_le_bytes(req.get(1).unwrap()[0..8].try_into().unwrap());
          let seq = leased_v.fetch_add(1, Ordering::Relaxed) as u64;
          let mut reply = ZmqMessage::from(identity.to_vec());
          if (seq as usize) < tasks {
            for frame in source_payload(seq, nonce, frame_count_for(seq), frame_bytes) {
              reply.push_back(frame);
            }
          } else {
            // drain sentinel
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

  // Workers: DEALER (request source) + PUSH (return result). Each verifies routing + integrity.
  let mut worker_tasks = Vec::new();
  for w in 0..workers {
    let router_ep = router_ep.clone();
    let sink_ep = sink_ep.clone();
    worker_tasks.push(tokio::spawn(async move {
      let nonce = w as u64;
      let mut dealer = DealerSocket::new();
      dealer.connect(&router_ep).await.expect("dealer connect");
      let mut push = PushSocket::new();
      push.connect(&sink_ep).await.expect("push connect");
      let mut done = 0usize;
      let mut anomalies: Vec<String> = Vec::new();
      loop {
        // request a task: [nonce | req_seq]
        let mut req = ZmqMessage::from(nonce.to_le_bytes().to_vec());
        req.push_back(Bytes::from((done as u64).to_le_bytes().to_vec()));
        if dealer.send(req).await.is_err() {
          break;
        }
        let src = match dealer.recv().await {
          Ok(m) => m,
          Err(_) => break,
        };
        // header = [seq | echoed_nonce | k]
        let header = src.get(0).unwrap();
        let seq = u64::from_le_bytes(header[0..8].try_into().unwrap());
        if seq == u64::MAX {
          break; // drain sentinel — no more work
        }
        let echoed = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let k = u64::from_le_bytes(header[16..24].try_into().unwrap()) as usize;
        if echoed != nonce {
          anomalies.push(format!(
            "MISROUTE: worker {nonce} got reply for {echoed} (seq {seq})"
          ));
        }
        if src.len() != k + 1 {
          anomalies.push(format!(
            "FRAME LOSS: seq {seq} expected {k} got {}",
            src.len() - 1
          ));
        }
        for (idx, frame) in src.iter().skip(1).enumerate() {
          let fseq = u64::from_le_bytes(frame[0..8].try_into().unwrap());
          let fidx = u32::from_le_bytes(frame[8..12].try_into().unwrap());
          let fnonce = u64::from_le_bytes(frame[12..20].try_into().unwrap());
          if fseq != seq {
            anomalies.push(format!(
              "SOURCE INTERLEAVING: seq {seq} frame {idx} carries {fseq}"
            ));
          }
          if fidx as usize != idx {
            anomalies.push(format!("SOURCE REORDER: seq {seq} pos {idx} idx {fidx}"));
          }
          if fnonce != nonce {
            anomalies.push(format!(
              "FRAME MISROUTE: worker {nonce} frame nonce {fnonce}"
            ));
          }
        }
        // return a result archive (also heavy-tailed): [seq | k'] then k' frames [seq | idx]
        let rk = frame_count_for(seq ^ 0x5555);
        let mut result = ZmqMessage::from({
          let mut h = vec![0u8; 16];
          h[0..8].copy_from_slice(&seq.to_le_bytes());
          h[8..16].copy_from_slice(&(rk as u64).to_le_bytes());
          h
        });
        for idx in 0..rk as u32 {
          let mut buf = vec![0u8; frame_bytes.max(12)];
          buf[0..8].copy_from_slice(&seq.to_le_bytes());
          buf[8..12].copy_from_slice(&idx.to_le_bytes());
          result.push_back(Bytes::from(buf));
        }
        if push.send(result).await.is_err() {
          break;
        }
        done += 1;
      }
      anomalies
    }));
  }

  // Collect.
  let mut all_anomalies: Vec<String> = Vec::new();
  for t in worker_tasks {
    if let Ok(mut a) = t.await {
      all_anomalies.append(&mut a);
    }
  }
  let (received, bytes_total, mut sink_anom) = sink_recv.await?;
  all_anomalies.append(&mut sink_anom);
  if let Ok(mut v) = ventilator.await {
    all_anomalies.append(&mut v);
  }
  let elapsed = start.elapsed();
  let secs = elapsed.as_secs_f64().max(1e-9);

  println!(
    "  {received}/{tasks} results in {:.3}s → {:.0} tasks/s, {:.1} MB/s (result bytes); leased {}",
    secs,
    received as f64 / secs,
    bytes_total as f64 / secs / 1_048_576.0,
    leased.load(Ordering::Relaxed),
  );
  if all_anomalies.is_empty() {
    println!(
      "  ✓ no interleaving / reordering / misrouting across {received} tasks over {workers} workers \
       (ROUTER+DEALER+PUSH+PULL, heavy-tailed payloads)"
    );
  } else {
    println!("  ✗ {} anomalies (first few):", all_anomalies.len());
    for a in all_anomalies.iter().take(8) {
      println!("    - {a}");
    }
  }
  Ok(())
}
