// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract tests for the self-install / diagnostics library behind `cortex init` / `cortex
//! doctor`.

use cortex::backend::test_db_address;
use cortex::bootstrap;

#[test]
fn doctor_is_healthy_against_a_migrated_db() {
  let report = bootstrap::doctor(test_db_address());
  assert!(report.database_reachable, "test db should be reachable");
  assert!(
    report.migrations_current,
    "test db should be fully migrated"
  );
  assert!(
    report.services_seeded,
    "built-in init/import services should be seeded"
  );
  assert!(report.ok, "overall status should be healthy");
}

#[test]
fn init_is_idempotent_and_scaffolds_config() {
  let mut config_path = std::env::temp_dir();
  config_path.push("cortex_bootstrap_test.toml");
  let _ = std::fs::remove_file(&config_path);

  let outcome = bootstrap::init(test_db_address(), &config_path).expect("init should succeed");

  // The test database is already migrated, so init applies nothing...
  assert!(
    outcome.migrations_applied.is_empty(),
    "an already-migrated db applies nothing"
  );
  // ...but it scaffolds a config file when one is missing.
  assert!(
    outcome.config_created,
    "a missing config file should be scaffolded"
  );
  assert!(config_path.exists());

  let written = std::fs::read_to_string(&config_path).expect("scaffold written");
  assert!(
    written.contains("[dispatcher]"),
    "scaffold has the operational sections"
  );
  assert!(
    !written.contains("rerun_tokens"),
    "scaffold must not contain secrets"
  );

  let _ = std::fs::remove_file(&config_path);
}
