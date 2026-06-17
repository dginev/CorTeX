// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
use cortex::config::config;
use cortex::dispatcher::manager::TaskManager;

/// A dispatcher executable for `CorTeX` distributed processing with ZMQ
fn main() {
  // Leveled logging (RUST_LOG, default info); the hot-path narration is trace/debug — see
  // `cortex::observability`.
  cortex::observability::init_tracing();
  // All operational parameters come from the runtime configuration
  // (defaults → cortex.toml → CORTEX_ env); see `cortex::config`.
  let cfg = config();
  let manager = TaskManager {
    source_port: cfg.dispatcher.source_port,
    result_port: cfg.dispatcher.result_port,
    // Note that queue_size must never be larger than postgresql's max_locks_per_transaction setting
    //   (typically specified in /etc/postgresql/9.1/main/postgresql.conf or similar)
    queue_size: cfg.dispatcher.queue_size,
    message_size: cfg.dispatcher.message_size,
    max_in_flight: cfg.dispatcher.max_in_flight,
    backend_address: cfg.database.url.clone(),
  };
  // Graceful shutdown (O-1): on SIGTERM/SIGINT, stop leasing new work and drain the in-flight set +
  // finalize batch before exiting, instead of the supervisor hard-killing in-flight tasks.
  // Unexpected failures still fail-fast (panic → abort). Production-only — bounded test runs
  // don't install this.
  cortex::dispatcher::server::install_shutdown_handlers();
  manager
    .start(None)
    .unwrap_or_else(|_| panic!("Failed to start TaskManager"));
}
