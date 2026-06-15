// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Observability bootstrap (Arm 8): one `tracing` subscriber for the binaries.
//!
//! The dispatcher's hot path emits leveled `tracing` events instead of raw `eprintln!`/`println!`,
//! so per-dispatched-task narration is `trace`/`debug` and is **filtered out at the default `info`
//! level** — a high task rate no longer pays a synchronous, locked-`stderr` write per event
//! (KNOWN_ISSUES D-11). Verbosity is runtime-controlled via `RUST_LOG` (e.g. `RUST_LOG=debug`,
//! `RUST_LOG=cortex=trace`), so the detail is available on demand without a rebuild.

/// Initializes the process-wide `tracing` subscriber: a plain `stderr` formatter filtered by
/// `RUST_LOG` (default `info`). Idempotent and panic-free — uses `try_init`, so a second call (or a
/// test that already installed a subscriber) is a no-op rather than a panic. Call once at the top
/// of each binary's `main`.
pub fn init_tracing() {
  use tracing_subscriber::{fmt, EnvFilter};
  let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
  // `try_init` returns `Err` if a global subscriber is already set — ignore it (idempotent).
  let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}
