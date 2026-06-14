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
    "cortex_sessions_active",
    "cortex_workers_total",
  ] {
    assert!(body.contains(metric), "the {metric} gauge is exposed");
  }

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

// Custom harness (see KNOWN_ISSUES L-1): run the case then `_exit(0)`.
fn main() {
  metrics_is_token_gated_and_exposes_gauges();
  eprintln!("metrics_test: all cases passed");
  unsafe { libc::_exit(0) }
}
