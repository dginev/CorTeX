// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract test for the Prometheus `/metrics` endpoint (`frontend::metrics`): token-gated (scrape
//! with `?token=` or the `X-Cortex-Token` header) and exposing current-state operational gauges in
//! Prometheus exposition format.

use cortex::backend::test_db_address;
use cortex::frontend::server::mount_api_with;
use rocket::http::{Header, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_metrics_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

fn metrics_is_token_gated_and_exposes_gauges() {
  let client = client();

  // Unauthenticated scrape is rejected (the gauges are not public).
  assert_eq!(
    client.get("/metrics").dispatch().status(),
    Status::Unauthorized,
    "/metrics requires a token"
  );

  // Prometheus-style scrape via ?token= (params in the scrape URL).
  let response = client.get("/metrics?token=token1").dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "a valid token scrapes /metrics"
  );
  let body = response.into_string().expect("a metrics body");
  // Prometheus exposition format + the operational gauges.
  assert!(
    body.contains("# TYPE cortex_pool_max gauge"),
    "Prometheus TYPE lines present"
  );
  assert!(
    body.contains("cortex_build_info{version="),
    "build info gauge present"
  );
  assert!(
    body.contains("cortex_db_reachable 1"),
    "the database is reachable in the test, got: {body}"
  );
  for metric in [
    "cortex_pool_in_use",
    "cortex_corpora_total",
    "cortex_services_total",
    "cortex_jobs_active",
    "cortex_jobs_failed_recent",
    "cortex_jobs_interrupted_recent",
    "cortex_sessions_active",
    "cortex_workers_total",
    "cortex_tasks_todo",
  ] {
    assert!(body.contains(metric), "the {metric} gauge is exposed");
  }
  // The pending-conversion backlog gauge parses as a valid non-negative count (not absent / -1).
  assert!(
    gauge_value(&body, "cortex_tasks_todo") >= 0,
    "cortex_tasks_todo is a valid non-negative gauge value"
  );

  // The header form (X-Cortex-Token) works too.
  let response = client
    .get("/metrics")
    .header(Header::new("X-Cortex-Token", "token1"))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "the X-Cortex-Token header also authenticates a scrape"
  );
}

/// The reading of an unlabeled Prometheus gauge line (`name <value>`), or -1 if absent/unparseable.
fn gauge_value(body: &str, name: &str) -> i64 {
  body
    .lines()
    .find(|line| line.starts_with(&format!("{name} ")))
    .and_then(|line| line.rsplit(' ').next())
    .and_then(|value| value.parse().ok())
    .unwrap_or(-1)
}

fn job_health_gauges_count_recent_failures() {
  use diesel::prelude::*;
  // Seed a recently-failed and a recently-interrupted job (created_at defaults to now() → inside
  // the 24h rolling window). Other failed/interrupted jobs in the window only increase the count,
  // so we assert >= 1 (robust against the shared test DB).
  let mut db = cortex::backend::testdb();
  for status in ["failed", "interrupted"] {
    diesel::sql_query(format!(
      "INSERT INTO jobs (kind, actor, status, params) \
       VALUES ('metrics_{status}_probe', 'tester', '{status}', '{{}}'::jsonb)"
    ))
    .execute(&mut db.connection)
    .expect("seed a terminal job");
  }

  let client = client();
  let body = client
    .get("/metrics?token=token1")
    .dispatch()
    .into_string()
    .expect("a metrics body");
  assert!(
    gauge_value(&body, "cortex_jobs_failed_recent") >= 1,
    "the recent-failed gauge counts the seeded failure, got: {}",
    gauge_value(&body, "cortex_jobs_failed_recent")
  );
  assert!(
    gauge_value(&body, "cortex_jobs_interrupted_recent") >= 1,
    "the recent-interrupted gauge counts the seeded interruption, got: {}",
    gauge_value(&body, "cortex_jobs_interrupted_recent")
  );

  diesel::sql_query(
    "DELETE FROM jobs WHERE kind IN ('metrics_failed_probe', 'metrics_interrupted_probe')",
  )
  .execute(&mut db.connection)
  .ok();
}

// Custom harness (see KNOWN_ISSUES L-1): run the cases then `_exit(0)`.
fn main() {
  metrics_is_token_gated_and_exposes_gauges();
  job_health_gauges_count_recent_failures();
  eprintln!("metrics_test: all cases passed");
  unsafe { libc::_exit(0) }
}
