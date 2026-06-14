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
  assert!(
    report.admin_token_configured,
    "the test config.json provides token1, so an admin token is configured"
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

#[test]
fn set_admin_token_scaffolds_merges_and_updates() {
  let mut config_path = std::env::temp_dir();
  config_path.push("cortex_set_token_test.toml");
  let _ = std::fs::remove_file(&config_path);

  // No file yet → scaffolds a complete config AND adds the token.
  let outcome = bootstrap::set_admin_token(&config_path, "tok-aaa", "alice").expect("add token");
  assert!(
    !outcome.replaced,
    "the first write of a token is an add, not an update"
  );
  assert_eq!(outcome.token_count, 1);
  let written = std::fs::read_to_string(&config_path).expect("written");
  assert!(
    written.contains("[dispatcher]"),
    "a fresh file is scaffolded with the operational sections too"
  );

  // A second token MERGES — the first token and the operational sections survive.
  let outcome = bootstrap::set_admin_token(&config_path, "tok-bbb", "bob").expect("add token 2");
  assert!(!outcome.replaced);
  assert_eq!(
    outcome.token_count, 2,
    "tokens accumulate (merge, not clobber)"
  );

  // Re-setting an existing token UPDATES its owner (no new entry).
  let outcome = bootstrap::set_admin_token(&config_path, "tok-aaa", "alice2").expect("update");
  assert!(outcome.replaced, "an existing token is an update");
  assert_eq!(
    outcome.token_count, 2,
    "the count is unchanged on an update"
  );

  // The result parses as valid TOML with both tokens under [auth].rerun_tokens, owners correct.
  let written = std::fs::read_to_string(&config_path).expect("written");
  let document: toml::Table = written.parse().expect("valid toml");
  let tokens = document["auth"]["rerun_tokens"]
    .as_table()
    .expect("auth.rerun_tokens table");
  assert_eq!(
    tokens["tok-aaa"].as_str(),
    Some("alice2"),
    "the re-set token's owner was updated"
  );
  assert_eq!(tokens["tok-bbb"].as_str(), Some("bob"));
  assert!(
    written.contains("[dispatcher]"),
    "operational sections are preserved across merges"
  );

  let _ = std::fs::remove_file(&config_path);
}

#[test]
fn generate_token_is_long_random_and_url_safe() {
  let first = bootstrap::generate_token();
  let second = bootstrap::generate_token();
  assert_eq!(first.len(), 32, "32-character token");
  assert!(
    first.chars().all(|c| c.is_ascii_alphanumeric()),
    "URL-safe alphanumeric, no escaping needed in a token header/query"
  );
  assert_ne!(first, second, "successive tokens differ (randomness)");
}

#[test]
fn db_tuning_guidance_points_at_pgtune_for_a_mixed_workload() {
  let guidance = bootstrap::db_tuning_guidance();
  // It guides + links (the owner decision) rather than reimplementing the heuristic.
  assert!(
    guidance.contains("pgtune.leopard.in.ua"),
    "links the pgtune service"
  );
  assert!(
    guidance.contains("mixed") || guidance.contains("Mixed"),
    "names the Mixed workload type"
  );
  assert!(
    guidance.contains("docs/DB_TUNING.md"),
    "points at the verified example block"
  );
}
