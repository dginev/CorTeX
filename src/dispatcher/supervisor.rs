// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Fail-fast supervision for the dispatcher's **async** components (dispatcher rationalization
//! phase 5; `docs/DISPATCHER_RATIONALIZATION.md`).
//!
//! Today the std-thread manager (`manager.rs`) supervises by `join()`-ing the ventilator and
//! polling `sink/finalize.is_finished()`; any component death → `Err(ETERM)` → process abort →
//! external restart (CLAUDE.md "process abort → external restart"; KNOWN_ISSUES D-3/D-9). When the
//! ventilator/sink move onto tokio tasks (`zeromq`), a component **panic** no longer surfaces as a
//! `JoinHandle::join` error on a worker thread — it surfaces as a [`tokio::task::JoinError`]. This
//! module re-expresses the *same* contract over tokio tasks: spawn the perpetual components, await
//! the **first** to end for any reason, and report which + how so the manager can fail-fast — a
//! panic is caught here rather than silently unwinding a task and leaving the pipeline stalled.
//!
//! This is the phase-5 supervision substrate the async sink (5a) and ventilator (5b) plug into; the
//! *decision* (first-end detection) is unit-tested, while the actual `abort`/`ETERM` is the thin,
//! deliberately-untestable shell — the same split as the std-thread manager.

use std::collections::HashMap;
use std::future::Future;

use tokio::task::{Id, JoinSet};
use tracing::error;

/// How a supervised dispatcher component ended. The dispatcher's components are **perpetual** (they
/// should run for the life of the process), so *every* one of these is a fault the manager
/// fail-fasts on — there is no "expected" end during normal operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentEnd {
  /// The component's future returned `Ok(())` — unexpected for a perpetual component (it shouldn't
  /// return at all), so still a fault.
  Completed,
  /// The component surfaced a handled error (`Err(_)`); the string is its message.
  Failed(String),
  /// The component **panicked**. Under std threads this was `join().is_err()`; under tokio it is a
  /// `JoinError` — mapped here to the same fail-fast trigger.
  Panicked,
  /// The task was cancelled/aborted (not expected in normal operation; treated as a fault).
  Cancelled,
}

/// Fail-fast supervisor for the dispatcher's async components. Spawn each perpetual component, then
/// `await` [`Supervisor::first_end`]; the first component to end (complete, fail, **or panic**) is
/// a fault, and the caller aborts the process for a supervised restart.
#[derive(Default)]
pub struct Supervisor {
  set: JoinSet<Result<(), String>>,
  names: HashMap<Id, String>,
}

impl Supervisor {
  /// A new, empty supervisor.
  pub fn new() -> Self { Self::default() }

  /// Spawn a perpetual component `future` under supervision, tagged `name` for the fault log. The
  /// future's `Err(_)` is a surfaced error and its panic is caught — both end up at
  /// [`Supervisor::first_end`].
  pub fn spawn<F>(&mut self, name: &str, future: F)
  where F: Future<Output = Result<(), String>> + Send + 'static {
    let handle = self.set.spawn(future);
    self.names.insert(handle.id(), name.to_string());
  }

  /// Await the **first** supervised component to end and report which (by `name`) and how
  /// ([`ComponentEnd`]). For a perpetual component every outcome is a fault, so the caller
  /// fail-fasts (log + abort). Returns `None` only when no components were spawned.
  pub async fn first_end(&mut self) -> Option<(String, ComponentEnd)> {
    let (id, end) = match self.set.join_next_with_id().await? {
      Ok((id, Ok(()))) => (id, ComponentEnd::Completed),
      Ok((id, Err(message))) => (id, ComponentEnd::Failed(message)),
      Err(join_error) => {
        let end = if join_error.is_panic() {
          ComponentEnd::Panicked
        } else {
          ComponentEnd::Cancelled
        };
        (join_error.id(), end)
      },
    };
    let name = self
      .names
      .get(&id)
      .cloned()
      .unwrap_or_else(|| "<unknown>".to_string());
    Some((name, end))
  }
}

/// Log a component death as the fail-fast trigger it is. The manager pairs this with the actual
/// process abort / `ETERM` return; it is split out so the first-end *detection* is unit-testable
/// without killing the test process (the same split as the std-thread manager, whose
/// `is_finished()` detection is tested while the `ETERM` return is not).
pub fn log_component_death(name: &str, end: &ComponentEnd) {
  error!(
    "dispatcher component {name:?} ended unexpectedly ({end:?}); aborting for a supervised restart"
  );
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  #[tokio::test]
  async fn first_end_detects_a_panicking_component() {
    // The one genuinely-new phase-5 risk: a panicking async component must still surface as the
    // fail-fast trigger (under std threads a panic was `join().is_err()`; under tokio it's a
    // JoinError). A steady component runs well past the test; the other panics immediately.
    let mut supervisor = Supervisor::new();
    supervisor.spawn("steady", async {
      tokio::time::sleep(Duration::from_secs(30)).await;
      Ok(())
    });
    supervisor.spawn("boom", async {
      panic!("kaboom");
    });
    let (name, end) = supervisor.first_end().await.expect("a component ended");
    assert_eq!(name, "boom", "the panicking component is the first to end");
    assert_eq!(
      end,
      ComponentEnd::Panicked,
      "a panic is the fail-fast trigger"
    );
  }

  #[tokio::test]
  async fn first_end_reports_a_graceful_error() {
    let mut supervisor = Supervisor::new();
    supervisor.spawn("failer", async { Err("db gone".to_string()) });
    let (name, end) = supervisor.first_end().await.expect("a component ended");
    assert_eq!(name, "failer");
    assert_eq!(end, ComponentEnd::Failed("db gone".to_string()));
  }

  #[tokio::test]
  async fn first_end_treats_clean_completion_as_a_fault() {
    // A perpetual component returning Ok(()) shouldn't happen; it is still reported (the caller
    // fail-fasts on it just the same).
    let mut supervisor = Supervisor::new();
    supervisor.spawn("quitter", async { Ok(()) });
    let (name, end) = supervisor.first_end().await.expect("a component ended");
    assert_eq!(name, "quitter");
    assert_eq!(end, ComponentEnd::Completed);
  }

  #[tokio::test]
  async fn first_end_on_an_empty_supervisor_is_none() {
    let mut supervisor = Supervisor::new();
    assert!(supervisor.first_end().await.is_none());
  }
}
