// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Dispatcher-rationalization SPIKE (throwaway; docs/archive/DISPATCHER_RATIONALIZATION.md). The
// **libzmq** baseline — the current `zmq` crate (C FFI) — running the SAME workload as
// `zmq_payload_zeromq`, so the pure-Rust crate's large-multipart correctness + throughput can be
// compared apples-to-apples. Uses `send_multipart`/`recv_multipart` (atomic multi-frame send/recv)
// + concurrent PUSH senders.
//
// Run (same env knobs as the zeromq spike; libzmq is synchronous, so this uses threads):
//   cargo run --release --example zmq_payload_libzmq
//   MSG_COUNT=5000 SENDERS=8 FRAMES=60 FRAME_BYTES=131072 LARGE_EVERY=4 \
//     cargo run --release --example zmq_payload_libzmq

use std::sync::Arc;
use std::thread;
use std::time::Instant;

fn env_usize(key: &str, default: usize) -> usize {
  std::env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

/// One message as a Vec of frames, each `[seq u64-le | frame_idx u32-le | filler]`.
fn build_frames(seq: u64, frames: usize, frame_bytes: usize) -> Vec<Vec<u8>> {
  (0..frames as u32)
    .map(|idx| {
      let mut buf = vec![0u8; frame_bytes.max(12)];
      buf[0..8].copy_from_slice(&seq.to_le_bytes());
      buf[8..12].copy_from_slice(&idx.to_le_bytes());
      buf
    })
    .collect()
}

/// Verifies all frames carry the same seq (no interleaving) + ascending indices (no reordering).
fn verify(frames: &[Vec<u8>]) -> Result<usize, String> {
  let first = frames.first().ok_or("empty message")?;
  if first.len() < 12 {
    return Err("frame too short".into());
  }
  let seq = u64::from_le_bytes(first[0..8].try_into().unwrap());
  for (idx, frame) in frames.iter().enumerate() {
    let fseq = u64::from_le_bytes(frame[0..8].try_into().unwrap());
    let fidx = u32::from_le_bytes(frame[8..12].try_into().unwrap());
    if fseq != seq {
      return Err(format!(
        "INTERLEAVING: msg seq {seq} frame {idx} carries seq {fseq}"
      ));
    }
    if fidx as usize != idx {
      return Err(format!(
        "REORDER: seq {seq} frame at pos {idx} has index {fidx}"
      ));
    }
  }
  Ok(frames.len())
}

fn main() {
  let msg_count = env_usize("MSG_COUNT", 2000);
  let senders = env_usize("SENDERS", 4).max(1);
  let frames = env_usize("FRAMES", 40).max(1);
  let frame_bytes = env_usize("FRAME_BYTES", 65536).max(12);
  let large_every = env_usize("LARGE_EVERY", 8).max(1);
  let port = env_usize("PORT", 58111);
  let endpoint = format!("tcp://127.0.0.1:{port}");

  println!(
    "libzmq (zmq crate) spike: {msg_count} msgs, {senders} senders, large every {large_every} \
     ({frames}×{frame_bytes}B), small = 1 frame"
  );

  let ctx = Arc::new(zmq::Context::new());
  let pull = ctx.socket(zmq::PULL).expect("pull");
  pull.bind(&endpoint).expect("bind");

  // Senders: each a thread with its own PUSH socket sending its share (a mix of large + small).
  let per = msg_count.div_ceil(senders);
  let mut handles = Vec::new();
  for s in 0..senders {
    let ctx = ctx.clone();
    let endpoint = endpoint.clone();
    handles.push(thread::spawn(move || {
      let push = ctx.socket(zmq::PUSH).expect("push");
      push.connect(&endpoint).expect("connect");
      for i in 0..per {
        let seq = (s * per + i) as u64;
        if seq as usize >= msg_count {
          break;
        }
        let n = if (seq as usize).is_multiple_of(large_every) {
          frames
        } else {
          1
        };
        let frames = build_frames(seq, n, frame_bytes);
        let parts: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        push.send_multipart(&parts, 0).expect("send");
      }
    }));
  }

  // Receiver: count to msg_count, verify each, track bytes + anomalies.
  let mut received = 0usize;
  let mut bytes_total = 0u64;
  let mut anomalies: Vec<String> = Vec::new();
  let mut wrote_sample = false;
  let start = Instant::now();
  while received < msg_count {
    match pull.recv_multipart(0) {
      Ok(parts) => {
        bytes_total += parts.iter().map(|f| f.len() as u64).sum::<u64>();
        match verify(&parts) {
          Ok(n) if n > 1 && !wrote_sample => {
            let reassembled: Vec<u8> = parts.concat();
            let path = std::env::temp_dir().join("cortex_libzmq_spike_sample.bin");
            wrote_sample = std::fs::write(&path, &reassembled).is_ok();
          },
          Ok(_) => {},
          Err(e) => anomalies.push(e),
        }
        received += 1;
      },
      Err(e) => {
        anomalies.push(format!("recv error: {e}"));
        break;
      },
    }
  }
  let elapsed = start.elapsed();
  for h in handles {
    let _ = h.join();
  }

  let secs = elapsed.as_secs_f64().max(1e-9);
  println!(
    "  received {received}/{msg_count} in {:.3}s → {:.0} msg/s, {:.1} MB/s; sync-fs write: {}",
    secs,
    received as f64 / secs,
    bytes_total as f64 / secs / 1_048_576.0,
    if wrote_sample {
      "ok"
    } else {
      "(no large msg seen)"
    },
  );
  if anomalies.is_empty() {
    println!("  ✓ no interleaving/reordering/corruption across {received} messages");
  } else {
    println!("  ✗ {} anomalies (first few):", anomalies.len());
    for a in anomalies.iter().take(5) {
      println!("    - {a}");
    }
  }
}
