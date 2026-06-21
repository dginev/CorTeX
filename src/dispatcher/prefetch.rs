// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Input-archive prefetcher (D-20): warm the next batch of task input archives into the OS **page
//! cache** ahead of dispatch, so the ventilator's inline `/data` read is served from RAM instead of
//! the cold QLC-RAID6 platter.
//!
//! Measured cold read on `/data` is ~10 ms median per ~685 KB archive (p99 ~34 ms), which caps the
//! single-threaded ventilator at ~100 dispatches/s — the binding bottleneck at full-arXiv scale,
//! where the ~1 TB working set ≫ RAM so nearly every dispatch reads cold. (The 6.7 GB sandbox fits
//! in cache and hides this entirely.) A pool of warmer threads `open + read → discard` the upcoming
//! archives; the bytes land in **reclaimable page cache** — not dispatcher RSS — so this can never
//! OOM (the kernel drops the clean cache before the workers' anonymous memory). It is the read-side
//! mirror of the sink's D-7 writer fan-out, but leaves the dispatch loop **untouched** (D-4
//! ordering preserved): the ventilator still does its own `File::open + read`, now served warm.
//!
//! Two bounds keep cache use sane (the prefetch is pure cache hygiene, not a correctness path — a
//! warm that lags, is skipped, or fails just leaves a cold read exactly as before):
//! - a **per-entry cap** (`prefetch_max_entry_mb`): a >cap monster is left for the ventilator's
//!   existing chunk-streaming read (O(chunk) resident), not read twice into cache;
//! - a **per-batch byte budget** (`prefetch_budget_mb`): a batch that clusters large entries stops
//!   warming at the budget and cold-streams its tail, so it can't churn out Postgres's cache.

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::{self, JoinHandle};

/// Pure warm-or-skip decision (unit-tested): warm an entry iff it is within the per-entry cap
/// **and** the batch's warm budget still has room for it.
fn should_warm(
  size: usize,
  max_entry_bytes: usize,
  budget_used: usize,
  budget_bytes: usize,
) -> bool {
  size <= max_entry_bytes && budget_used + size <= budget_bytes
}

/// One warmer thread: drain entry paths and pull each (within the caps) into the page cache by
/// reading it to a sink. `metadata`/`open` failures are ignored — the ventilator will hit the same
/// path and handle it (a cold read, or the missing-input warning), so a warmer is never a failure
/// path of its own.
fn warm_loop(
  rx: &Receiver<String>,
  budget: &Arc<AtomicUsize>,
  max_entry_bytes: usize,
  budget_bytes: usize,
) {
  while let Ok(path) = rx.recv() {
    let Ok(meta) = std::fs::metadata(&path) else {
      continue;
    };
    let size = meta.len() as usize;
    if !should_warm(
      size,
      max_entry_bytes,
      budget.load(Ordering::Relaxed),
      budget_bytes,
    ) {
      continue;
    }
    budget.fetch_add(size, Ordering::Relaxed);
    // Warm into page cache: read + discard. `io::copy` uses a small internal buffer, so the
    // warmer's own RSS stays O(buffer); only the kernel retains the (reclaimable) file pages.
    if let Ok(mut f) = File::open(&path) {
      let _ = std::io::copy(&mut f, &mut std::io::sink());
    }
  }
}

/// A pool of page-cache warmer threads fed a batch of input-archive paths per refetch. Disabled
/// (`input_prefetchers = 0`) it is an inert no-op — [`Self::warm_batch`] returns immediately and
/// the ventilator reads inline exactly as before D-20. Held on the ventilator's stack; dropping it
/// (on ventilator shutdown/restart) disconnects the warmers and joins them.
pub struct Prefetcher {
  /// One bounded command channel per warmer (round-robin fed, like the sink's writer pool). Empty
  /// when disabled.
  senders: Vec<SyncSender<String>>,
  /// Cumulative bytes warmed for the current batch; reset at the start of each
  /// [`Self::warm_batch`] and shared across the warmers to enforce the per-batch budget.
  budget_used: Arc<AtomicUsize>,
  handles: Vec<JoinHandle<()>>,
}

impl Prefetcher {
  /// Spawn `threads` warmers (0 ⇒ disabled no-op). `channel_cap` bounds each warmer's pending-warm
  /// queue; `max_entry_bytes`/`budget_bytes` are the per-entry and per-batch caps.
  #[must_use]
  pub fn new(
    threads: usize,
    channel_cap: usize,
    max_entry_bytes: usize,
    budget_bytes: usize,
  ) -> Self {
    let budget_used = Arc::new(AtomicUsize::new(0));
    let mut senders = Vec::with_capacity(threads);
    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
      let (tx, rx) = sync_channel::<String>(channel_cap.max(1));
      let budget = budget_used.clone();
      senders.push(tx);
      handles.push(thread::spawn(move || {
        warm_loop(&rx, &budget, max_entry_bytes, budget_bytes)
      }));
    }
    Prefetcher {
      senders,
      budget_used,
      handles,
    }
  }

  /// Feed a fetch batch's input-archive paths (in **dispatch order**) to the warmers, resetting the
  /// per-batch budget first. Non-blocking: a full warmer channel drops the path (it stays a cold
  /// read — graceful). A disabled pool returns immediately. The ventilator calls this right after
  /// `fetch_tasks` so the batch warms over the seconds before its tasks are dispatched.
  pub fn warm_batch<I: IntoIterator<Item = String>>(&self, paths: I) {
    if self.senders.is_empty() {
      return;
    }
    // New batch: the previous batch's warmers have long since drained (they outpace dispatch ~Nx),
    // so reset the cumulative-bytes budget for this one.
    self.budget_used.store(0, Ordering::Relaxed);
    for (i, path) in paths.into_iter().enumerate() {
      let _ = self.senders[i % self.senders.len()].try_send(path);
    }
  }
}

impl Drop for Prefetcher {
  fn drop(&mut self) {
    // Drop the senders so each warmer's `recv` disconnects and the thread exits, then join — no
    // orphaned warmers across a ventilator restart.
    self.senders.clear();
    for handle in self.handles.drain(..) {
      let _ = handle.join();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn should_warm_respects_per_entry_cap() {
    let cap = 50 * 1024 * 1024; // 50 MiB
    let budget = 8192 * 1024 * 1024; // 8 GiB
    assert!(
      should_warm(685 * 1024, cap, 0, budget),
      "a typical small entry warms"
    );
    assert!(
      should_warm(cap, cap, 0, budget),
      "exactly at the cap still warms"
    );
    assert!(
      !should_warm(cap + 1, cap, 0, budget),
      "a 1-byte-over-cap monster is skipped (cold-streamed)"
    );
  }

  #[test]
  fn should_warm_respects_batch_budget() {
    let cap = 50 * 1024 * 1024;
    let budget = 100 * 1024 * 1024; // 100 MiB batch budget
    assert!(
      should_warm(40 * 1024 * 1024, cap, 50 * 1024 * 1024, budget),
      "fits in remaining budget"
    );
    assert!(
      !should_warm(40 * 1024 * 1024, cap, 70 * 1024 * 1024, budget),
      "would exceed the batch budget → skip (tail cold-streams)"
    );
    assert!(
      should_warm(0, cap, budget, budget),
      "a zero-byte entry never trips the budget"
    );
  }

  #[test]
  fn disabled_pool_is_an_inert_no_op() {
    let pf = Prefetcher::new(0, 64, 1, 1);
    pf.warm_batch(vec![
      "/nonexistent/a".to_string(),
      "/nonexistent/b".to_string(),
    ]);
    // No panic, no threads, nothing warmed — the ventilator path is unchanged when disabled.
    assert!(pf.senders.is_empty());
  }

  #[test]
  fn enabled_pool_warms_real_files_then_shuts_down_cleanly() {
    // Warm two temp files; the pool must accept them and join cleanly on drop (the warm itself is a
    // page-cache side effect we can't assert portably, but the lifecycle + feed must be sound).
    let dir = std::env::temp_dir();
    let mut paths = Vec::new();
    for i in 0..2 {
      let p = dir.join(format!("cortex_prefetch_test_{i}.bin"));
      std::fs::write(&p, vec![7u8; 4096]).unwrap();
      paths.push(p.to_string_lossy().into_owned());
    }
    let pf = Prefetcher::new(2, 64, 50 * 1024 * 1024, 8192 * 1024 * 1024);
    pf.warm_batch(paths.clone());
    drop(pf); // joins the warmers
    for p in paths {
      std::fs::remove_file(&p).ok();
    }
  }
}
