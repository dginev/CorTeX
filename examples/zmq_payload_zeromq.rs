// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Dispatcher-rationalization SPIKE (throwaway; docs/archive/DISPATCHER_RATIONALIZATION.md).
// Exercises the **pure-Rust, async-native `zeromq` crate** (zmq.rs — escapes the libzmq C FFI) on
// the questions the owner raised: does it reassemble **large multi-frame** messages without
// interleaving/corruption under concurrent senders (the "rare large response interrupting other
// messages" bug), what is its throughput across a variety of payloads, and does async `tokio::fs`
// work for the archive write.
//
// Run (vary the payload freely):
//   cargo run --release --example zmq_payload_zeromq
//   MSG_COUNT=5000 SENDERS=8 FRAMES=60 FRAME_BYTES=131072 LARGE_EVERY=4 \
//     cargo run --release --example zmq_payload_zeromq
//
// A companion `zmq_payload_libzmq` runs the SAME workload over the current libzmq `zmq` crate, for
// an apples-to-apples comparison.

use std::time::Instant;

use bytes::Bytes;
use zeromq::{PullSocket, PushSocket, Socket, SocketRecv, SocketSend, ZmqMessage};

fn env_usize(key: &str, default: usize) -> usize {
  std::env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

/// Builds one message: frame 0..N each carries `[seq u64-le | frame_idx u32-le | filler]`, so the
/// receiver can detect cross-message frame contamination (wrong seq) or reordering (wrong index).
fn build_message(seq: u64, frames: usize, frame_bytes: usize) -> ZmqMessage {
  let frame = |idx: u32| -> Bytes {
    let mut buf = vec![0u8; frame_bytes.max(12)];
    buf[0..8].copy_from_slice(&seq.to_le_bytes());
    buf[8..12].copy_from_slice(&idx.to_le_bytes());
    Bytes::from(buf)
  };
  let mut msg = ZmqMessage::from(frame(0).to_vec());
  for idx in 1..frames as u32 {
    msg.push_back(frame(idx));
  }
  msg
}

/// Verifies a received message's frames all carry the same seq (no interleaving) and ascending
/// indices (no reordering). Returns `Ok(frame_count)` or an error describing the anomaly.
fn verify_message(msg: &ZmqMessage) -> Result<usize, String> {
  let first = msg.get(0).ok_or("empty message")?;
  if first.len() < 12 {
    return Err("frame too short for header".into());
  }
  let seq = u64::from_le_bytes(first[0..8].try_into().unwrap());
  for (idx, frame) in msg.iter().enumerate() {
    if frame.len() < 12 {
      return Err(format!("seq {seq} frame {idx} too short"));
    }
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
  Ok(msg.len())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let msg_count = env_usize("MSG_COUNT", 2000);
  let senders = env_usize("SENDERS", 4).max(1);
  let frames = env_usize("FRAMES", 40).max(1);
  let frame_bytes = env_usize("FRAME_BYTES", 65536).max(12);
  let large_every = env_usize("LARGE_EVERY", 8).max(1);
  let endpoint = "tcp://127.0.0.1:0"; // ephemeral port; the receiver reports the bound endpoint

  println!(
    "zeromq (pure-Rust) spike: {msg_count} msgs, {senders} senders, large every {large_every} \
     ({frames}×{frame_bytes}B), small = 1 frame"
  );

  let mut pull = PullSocket::new();
  let bound = pull.bind(endpoint).await?;
  let connect_to = bound.to_string();

  // Receiver: count to msg_count, verify each message, track bytes + anomalies, async-write one
  // large.
  let recv = tokio::spawn(async move {
    let mut received = 0usize;
    let mut bytes_total = 0u64;
    let mut anomalies: Vec<String> = Vec::new();
    let mut wrote_sample = false;
    let start = Instant::now();
    while received < msg_count {
      match pull.recv().await {
        Ok(msg) => {
          bytes_total += msg.iter().map(|f| f.len() as u64).sum::<u64>();
          match verify_message(&msg) {
            Ok(n) if n > 1 && !wrote_sample => {
              // Demonstrate async file I/O on a reassembled large archive.
              let reassembled: Vec<u8> = msg.iter().flat_map(|f| f.to_vec()).collect();
              let path = std::env::temp_dir().join("cortex_zeromq_spike_sample.bin");
              if tokio::fs::write(&path, &reassembled).await.is_ok() {
                wrote_sample = true;
              }
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
    (
      received,
      bytes_total,
      anomalies,
      start.elapsed(),
      wrote_sample,
    )
  });

  // Senders connect + each sends its share (a mix of large multipart + small messages).
  let per = msg_count.div_ceil(senders);
  let mut sender_tasks = Vec::new();
  for s in 0..senders {
    let connect_to = connect_to.clone();
    sender_tasks.push(tokio::spawn(async move {
      let mut push = PushSocket::new();
      push.connect(&connect_to).await.expect("connect");
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
        push
          .send(build_message(seq, n, frame_bytes))
          .await
          .expect("send");
      }
    }));
  }
  for t in sender_tasks {
    let _ = t.await;
  }

  let (received, bytes_total, anomalies, elapsed, wrote) = recv.await?;
  let secs = elapsed.as_secs_f64().max(1e-9);
  println!(
    "  received {received}/{msg_count} in {:.3}s → {:.0} msg/s, {:.1} MB/s; async-fs write: {}",
    secs,
    received as f64 / secs,
    bytes_total as f64 / secs / 1_048_576.0,
    if wrote { "ok" } else { "(no large msg seen)" },
  );
  if anomalies.is_empty() {
    println!("  ✓ no interleaving/reordering/corruption across {received} messages");
  } else {
    println!("  ✗ {} anomalies (first few):", anomalies.len());
    for a in anomalies.iter().take(5) {
      println!("    - {a}");
    }
  }
  Ok(())
}
