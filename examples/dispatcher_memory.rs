// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Dispatcher-rationalization SPIKE (throwaway; docs/DISPATCHER_RATIONALIZATION.md).
// Memory-discipline audit (owner, 2026-06-14): the dispatcher is co-resident with the workers on
// one box and must stay light on RAM (≤32 GB, ideally a few GB) while up to **300 jobs** are
// concurrently in flight with a heavy-tailed size mix (median 800 KB, mean ~1.5 MB, max 200 MB).
//
// It isolates the single decision that governs the dispatcher's footprint: **do we hold a whole
// archive per in-flight job, or do we stream it in bounded chunks?** No ZMQ / workers here (those
// would contaminate the measurement) — it just materializes the resident set each design would hold
// for `JOBS` concurrent jobs and reports the process VmRSS, so the budget is grounded in a real
// number rather than asserted.
//
// Run:
//   cargo run --release --example dispatcher_memory                 # whole-archive, no giant burst
//   MODE=chunked cargo run --release --example dispatcher_memory    # chunked streaming
//   MODE=whole GIANT_BURST=40 cargo run --release --example dispatcher_memory   # adversarial burst

use std::f64::consts::PI;

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
fn u01(h: u64) -> f64 { ((h >> 11) as f64 / (1u64 << 53) as f64).clamp(1e-12, 1.0 - 1e-12) }

/// log-normal job size (median 800 KB, mean ~1.5 MB), clamped [500 KB, 200 MB].
fn job_bytes(seq: u64) -> usize {
  let z = (-2.0 * u01(mix(seq)).ln()).sqrt() * (2.0 * PI * u01(mix(seq ^ 0xABCD))).cos();
  let bytes = ((800.0 * 1024.0_f64).ln() + 1.121 * z).exp();
  (bytes as usize).clamp(500 * 1024, 200 * 1024 * 1024)
}

/// Current resident set size (KB) from /proc/self/status — the real memory the OS has backed.
fn vmrss_kb() -> u64 {
  std::fs::read_to_string("/proc/self/status")
    .ok()
    .and_then(|s| {
      s.lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
    })
    .unwrap_or(0)
}

fn main() {
  let mode = std::env::var("MODE").unwrap_or_else(|_| "whole".into());
  let jobs = env_usize("JOBS", 300);
  let chunk = env_usize("CHUNK_KB", 1024) * 1024;
  let giant_burst = env_usize("GIANT_BURST", 0); // force this many concurrent 200 MB jobs

  let base = vmrss_kb();
  // Materialize what the dispatcher would hold for `jobs` concurrent jobs, touching every page so
  // the RSS reflects real backing (Vec::with_capacity alone would not fault the pages in).
  let mut resident: Vec<Vec<u8>> = Vec::with_capacity(jobs);
  let mut total_job_bytes: u64 = 0;
  for j in 0..jobs {
    let size = if j < giant_burst {
      200 * 1024 * 1024
    } else {
      job_bytes(j as u64)
    };
    total_job_bytes += size as u64;
    // WHOLE: the entire archive resident per in-flight job. CHUNKED: only one streaming chunk.
    let held = if mode == "chunked" {
      chunk.min(size)
    } else {
      size
    };
    resident.push(vec![1u8; held]);
  }
  let peak = vmrss_kb();
  let held_mb = resident.iter().map(|b| b.len() as u64).sum::<u64>() as f64 / 1_048_576.0;

  println!(
    "dispatcher_memory: MODE={mode} JOBS={jobs} chunk={}KB giant_burst={giant_burst}",
    chunk / 1024
  );
  println!(
    "  job-data the WHOLE design would face: {:.1} GB ({} jobs, mean {:.1} MB)",
    total_job_bytes as f64 / 1_073_741_824.0,
    jobs,
    total_job_bytes as f64 / jobs as f64 / 1_048_576.0,
  );
  println!(
    "  resident held this run: {held_mb:.0} MB  →  process RSS {:.2} GB (Δ {:.2} GB over baseline)",
    peak as f64 / 1_048_576.0,
    (peak - base) as f64 / 1_048_576.0,
  );
  // Keep `resident` alive across the measurement.
  std::hint::black_box(&resident);
}
