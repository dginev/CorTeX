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

/// Initializes the process-wide `tracing` subscriber: a plain **stderr** formatter filtered by
/// `RUST_LOG` (default `info`). Idempotent and panic-free — uses `try_init`, so a second call (or a
/// test that already installed a subscriber) is a no-op rather than a panic. Call once at the top
/// of each binary's `main` (the dispatcher and frontend; the CLI uses [`init_cli_tracing`]).
pub fn init_tracing() {
  use tracing_subscriber::{EnvFilter, fmt};
  // Default (when `RUST_LOG` is unset): app events at `info`, but Rocket's per-request internals at
  // `warn` only. The frontend shares this subscriber (Rocket 0.5 logs via `tracing`), and its
  // live-ops dashboard polls `/admin/status.json` every few seconds — at `info` Rocket would log
  // ~4 lines per poll forever, drowning the app's own events. `rocket=warn` keeps the launch banner
  // (emitted at `warn`) + any Rocket warnings/errors while silencing the per-request flood; the
  // app's `info`/`warn` events (e.g. the P-2 slow-report warning) still show. The dispatcher has no
  // Rocket, so the directive is inert there. Override anytime with `RUST_LOG` (e.g.
  // `RUST_LOG=rocket=info` to restore per-request traces, `RUST_LOG=cortex=debug` for app detail).
  let filter =
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,rocket=warn"));
  // Write to **stderr**, not stdout: a log stream belongs on stderr (stdout is reserved for program
  // output), and `fmt()`'s default writer is `io::stdout` — so this `.with_writer` is load-bearing,
  // not cosmetic. `try_init` returns `Err` if a global subscriber is already set — ignore it.
  let _ = fmt()
    .with_env_filter(filter)
    .with_target(false)
    .with_writer(std::io::stderr)
    .try_init();
}

/// Initializes the `tracing` subscriber for the **`cortex` CLI**. Like [`init_tracing`] (stderr,
/// `RUST_LOG`-filtered, idempotent), but when `RUST_LOG` is unset the level is driven by the CLI's
/// `-v`/`-q` flags rather than defaulting to `info`:
///
/// | flags        | level   |
/// |--------------|---------|
/// | `-q`         | `error` |
/// | *(none)*     | `warn`  |
/// | `-v`         | `info`  |
/// | `-vv`        | `debug` |
/// | `-vvv`+      | `trace` |
///
/// The default is `warn` (not `info`) because the CLI's *normal* output is each subcommand's own
/// stdout / `--json`; `tracing` events are opt-in diagnostics. Events always go to **stderr** so
/// they never corrupt a `--json` subcommand's machine output on stdout. An explicit `RUST_LOG`
/// always wins over the flags (so `RUST_LOG=cortex=trace cortex status` works regardless of `-v`).
pub fn init_cli_tracing(verbose: u8, quiet: bool) {
  use tracing_subscriber::{EnvFilter, fmt};
  let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
    let level = if quiet {
      "error"
    } else {
      match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
      }
    };
    // Keep Rocket quiet for parity with `init_tracing`; the directive is inert in the CLI (no
    // Rocket), but harmless and consistent if a shared filter is ever logged.
    EnvFilter::new(format!("{level},rocket=warn"))
  });
  let _ = fmt()
    .with_env_filter(filter)
    .with_target(false)
    .with_writer(std::io::stderr)
    .try_init();
}
