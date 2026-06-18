// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract tests for the jobs HTTP surface: poll a job (agent), and the human progress page.

use cortex::backend::{build_pool, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::jobs;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_jobs_api_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

/// Signs the tracked client in as an admin (the `/jobs` screens require the `AdminSession` cookie;
/// the `/api/jobs` twins stay token-based and are unaffected).
fn sign_in(client: &Client) {
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
}

fn api_job_polls_a_spawned_job() {
  let pool = build_pool(test_db_address(), 4);
  let uuid = jobs::spawn_job(
    pool,
    "api_test_job",
    "tester",
    serde_json::json!({}),
    |progress| {
      progress.step(1, Some(1), "done");
      Ok(serde_json::json!({ "done": true }))
    },
  )
  .expect("spawn the job");

  let client = client();
  let path = format!("/api/jobs/{uuid}?token=token1");
  let mut body = serde_json::Value::Null;
  for _ in 0..200 {
    let response = client.get(path.as_str()).dispatch();
    assert_eq!(response.status(), Status::Ok);
    assert_eq!(response.content_type(), Some(ContentType::JSON));
    body = response.into_json().expect("a JSON body");
    if body["status"] == "succeeded" {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
  }

  // Data contract:
  assert_eq!(body["status"], "succeeded");
  assert_eq!(body["kind"], "api_test_job");
  assert_eq!(body["uuid"], uuid.to_string());
  assert!(body["progress_current"].is_number());
  assert_eq!(body["result"], serde_json::json!({ "done": true }));
}

fn api_job_is_404_for_unknown_uuid() {
  let client = client();
  // Token-gated (jobs carry admin attribution + params): no token → 401, even for an unknown uuid
  // (the guard runs before the lookup). X-10's sibling read-twin gap.
  assert_eq!(
    client
      .get("/api/jobs/00000000-0000-0000-0000-000000000000")
      .dispatch()
      .status(),
    Status::Unauthorized,
    "GET /api/jobs/<uuid> without a token is 401 (jobs are admin-only)"
  );
  // With a token, an unknown uuid is a clean 404.
  let response = client
    .get("/api/jobs/00000000-0000-0000-0000-000000000000?token=token1")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
}

fn job_progress_page_renders_html() {
  let client = client();
  // Admin-only: unauthenticated → redirect to sign-in; after signing in it renders.
  let response = client
    .get("/jobs/00000000-0000-0000-0000-000000000000")
    .dispatch();
  assert!(
    response
      .headers()
      .get_one("Location")
      .unwrap_or("")
      .starts_with("/admin/login?next="),
    "the job progress page requires sign-in (with a return path)"
  );
  sign_in(&client);
  let response = client
    .get("/jobs/00000000-0000-0000-0000-000000000000")
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
}

fn jobs_list_carries_health_and_duration_and_supports_pending() {
  // Spawn a job and let it finish, so the list has a known recent entry.
  let pool = build_pool(test_db_address(), 4);
  let uuid = jobs::spawn_job(
    pool,
    "list_test_job",
    "tester",
    serde_json::json!({}),
    |p| {
      p.step(1, Some(1), "done");
      Ok(serde_json::json!({ "ok": true }))
    },
  )
  .expect("spawn the job");
  // Give the worker thread a moment to reach a terminal state.
  for _ in 0..100 {
    let mut db = cortex::backend::testdb();
    if let Some(job) = jobs::find_job(&mut db.connection, uuid)
      && job.status == "succeeded"
    {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
  }

  let client = client();
  sign_in(&client); // the /jobs HTML dashboard below is admin-only

  // The fleet-wide list carries the observability metadata (health + duration) for every job.
  let response = client.get("/api/jobs?limit=100&token=token1").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let body: serde_json::Value = response.into_json().expect("a JSON array");
  let ours = body
    .as_array()
    .expect("array")
    .iter()
    .find(|j| j["uuid"] == uuid.to_string())
    .expect("our job is listed");
  assert_eq!(ours["health"], "ok", "succeeded -> ok health");
  assert!(
    ours["duration_seconds"].is_number(),
    "duration metadata present"
  );
  assert!(
    ours["seconds_since_update"].is_number(),
    "heartbeat-age metadata present"
  );

  // The HTML dashboard renders.
  let response = client.get("/jobs").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let dashboard = response.into_string().expect("html");
  assert!(dashboard.contains("Background jobs"));
  // The persistent nav is present on every page (not just the landing overview); the admin actions
  // are consolidated behind a single sign-in-gated /admin entry (Admin-UX cohesion contract).
  assert!(
    dashboard.contains("cortex-admin-nav"),
    "the persistent admin nav renders on non-landing pages"
  );
  for link in ["/", "/admin"] {
    assert!(
      dashboard.contains(&format!("href=\"{link}\"")),
      "the nav links to {link}"
    );
  }

  // The pending filter excludes our now-terminal job.
  let response = client
    .get("/api/jobs?active=true&limit=100&token=token1")
    .dispatch();
  let pending: serde_json::Value = response.into_json().expect("a JSON array");
  assert!(
    !pending
      .as_array()
      .expect("array")
      .iter()
      .any(|j| j["uuid"] == uuid.to_string()),
    "a finished job is not pending"
  );
}

fn jobs_dashboard_auto_refreshes_while_a_job_is_active() {
  use diesel::prelude::*;
  // Seed a job stuck `running`. (Tests build via `mount_api_with`, which — unlike the production
  // `mount_api` — does NOT interrupt orphans, so the running row persists for the assertion.)
  let mut db = cortex::backend::testdb();
  diesel::sql_query(
    "INSERT INTO jobs (kind, actor, status, params) \
     VALUES ('autorefresh_active_test', 'tester', 'running', '{}'::jsonb)",
  )
  .execute(&mut db.connection)
  .expect("seed a running job");

  let client = client();
  sign_in(&client); // the /jobs dashboard is admin-only
  let body = client
    .get("/jobs")
    .dispatch()
    .into_string()
    .expect("html body");
  assert!(
    body.contains("http-equiv=\"refresh\""),
    "the dashboard auto-refreshes (meta-refresh) while a job is in flight"
  );

  diesel::sql_query("DELETE FROM jobs WHERE kind = 'autorefresh_active_test'")
    .execute(&mut db.connection)
    .ok();
}

fn stalled_running_job_reports_a_large_heartbeat_age() {
  use diesel::prelude::*;
  // Seed a job stuck `running` whose last update is well in the past — a stalled body (the W-4
  // residual). `mount_api_with` does not interrupt orphans, so the running row survives the probe.
  let mut db = cortex::backend::testdb();
  diesel::sql_query(
    "INSERT INTO jobs (kind, actor, status, params, created_at, updated_at) \
     VALUES ('stall_probe_test', 'tester', 'running', '{}'::jsonb, \
             LOCALTIMESTAMP - interval '1 hour', LOCALTIMESTAMP - interval '1 hour')",
  )
  .execute(&mut db.connection)
  .expect("seed a stalled running job");

  let client = client();
  let response = client.get("/api/jobs?limit=200&token=token1").dispatch();
  let body: serde_json::Value = response.into_json().expect("a JSON array");
  let ours = body
    .as_array()
    .expect("array")
    .iter()
    .find(|j| j["kind"] == "stall_probe_test")
    .expect("the stalled job is listed");
  assert_eq!(ours["health"], "running");
  let idle = ours["seconds_since_update"].as_i64().expect("a number");
  assert!(
    idle >= 3000,
    "a job idle for ~1h reports a large heartbeat age, got {idle}s"
  );

  diesel::sql_query("DELETE FROM jobs WHERE kind = 'stall_probe_test'")
    .execute(&mut db.connection)
    .ok();
}

fn stale_running_job_is_reaped_but_fresh_one_survives() {
  use diesel::prelude::*;
  let mut db = cortex::backend::testdb();
  // A hung job: running, no heartbeat for 3h (past the 2h reap threshold).
  diesel::sql_query(
    "INSERT INTO jobs (kind, actor, status, params, created_at, updated_at) \
     VALUES ('reap_stale_test', 'tester', 'running', '{}'::jsonb, \
             LOCALTIMESTAMP - interval '3 hours', LOCALTIMESTAMP - interval '3 hours')",
  )
  .execute(&mut db.connection)
  .expect("seed a hung job");
  // A healthy job: running with a fresh heartbeat — must NOT be reaped.
  diesel::sql_query(
    "INSERT INTO jobs (kind, actor, status, params) \
     VALUES ('reap_fresh_test', 'tester', 'running', '{}'::jsonb)",
  )
  .execute(&mut db.connection)
  .expect("seed a fresh job");

  // Any jobs listing runs the W-4 runtime reaper first (here via the agent API).
  let client = client();
  client.get("/api/jobs?limit=200&token=token1").dispatch();

  let status_of = |kind: &str, db: &mut cortex::backend::Backend| -> String {
    cortex::schema::jobs::table
      .filter(cortex::schema::jobs::kind.eq(kind))
      .select(cortex::schema::jobs::status)
      .first::<String>(&mut db.connection)
      .expect("job present")
  };
  assert_eq!(
    status_of("reap_stale_test", &mut db),
    "interrupted",
    "a 3h-silent running job is reaped to interrupted (W-4 runtime reaper)"
  );
  assert_eq!(
    status_of("reap_fresh_test", &mut db),
    "running",
    "a running job with a fresh heartbeat is NOT reaped"
  );

  diesel::sql_query("DELETE FROM jobs WHERE kind IN ('reap_stale_test', 'reap_fresh_test')")
    .execute(&mut db.connection)
    .ok();
}

// Custom harness (see KNOWN_ISSUES L-1): run the cases then `_exit(0)`.
/// `interrupt_orphans` — called once on frontend startup — marks every non-terminal job (`queued` /
/// `running`) as `interrupted` so a job that died with a previous process is not left looking live
/// (the MANUAL §8 "cleaned at restart" guarantee, the startup complement to the runtime
/// `reap_stale` above). Run inside a rolled-back `test_transaction`: the function is a GLOBAL
/// update over all non-terminal jobs, so isolation keeps it from disturbing a parallel test
/// binary's live jobs.
fn interrupt_orphans_cleans_non_terminal_jobs_on_restart() {
  use diesel::prelude::*;
  let mut db = cortex::backend::testdb();
  db.connection
    .test_transaction::<_, diesel::result::Error, _>(|conn| {
      // Two non-terminal jobs (as if a previous process died mid-flight) + one terminal job that
      // must be left untouched.
      diesel::sql_query(
        "INSERT INTO jobs (kind, actor, status, params) VALUES \
         ('orphan_running_probe', 'tester', 'running', '{}'::jsonb), \
         ('orphan_queued_probe', 'tester', 'queued', '{}'::jsonb), \
         ('orphan_done_probe', 'tester', 'succeeded', '{}'::jsonb)",
      )
      .execute(conn)?;

      let reaped = jobs::interrupt_orphans(conn);
      assert!(
        reaped >= 2,
        "both non-terminal probe jobs are interrupted (got {reaped})"
      );

      let status_of = |kind: &str, conn: &mut PgConnection| -> String {
        cortex::schema::jobs::table
          .filter(cortex::schema::jobs::kind.eq(kind))
          .select(cortex::schema::jobs::status)
          .first::<String>(conn)
          .expect("probe job present")
      };
      assert_eq!(
        status_of("orphan_running_probe", conn),
        "interrupted",
        "a running job is interrupted on restart"
      );
      assert_eq!(
        status_of("orphan_queued_probe", conn),
        "interrupted",
        "a queued job is interrupted on restart"
      );
      assert_eq!(
        status_of("orphan_done_probe", conn),
        "succeeded",
        "a terminal (succeeded) job is left untouched"
      );

      // The message records the cause (distinguishing a restart-interrupt from a stale-heartbeat
      // reap).
      let message: String = cortex::schema::jobs::table
        .filter(cortex::schema::jobs::kind.eq("orphan_running_probe"))
        .select(cortex::schema::jobs::message)
        .first(conn)
        .expect("message present");
      assert!(
        message.contains("restart"),
        "the interrupt records the restart cause, got {message:?}"
      );
      Ok(())
    });
}

fn main() {
  api_job_polls_a_spawned_job();
  api_job_is_404_for_unknown_uuid();
  job_progress_page_renders_html();
  jobs_list_carries_health_and_duration_and_supports_pending();
  jobs_dashboard_auto_refreshes_while_a_job_is_active();
  stalled_running_job_reports_a_large_heartbeat_age();
  stale_running_job_is_reaped_but_fresh_one_survives();
  interrupt_orphans_cleans_non_terminal_jobs_on_restart();
  eprintln!("jobs_api_test: all cases passed");
  unsafe { libc::_exit(0) }
}
