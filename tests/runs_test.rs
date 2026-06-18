// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract tests for the historical-runs API: list a `(corpus, service)`'s runs and the current
//! (open) run, as the agent twin of the human history screen.

use chrono::{NaiveDate, NaiveDateTime};
use cortex::backend::{self, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::helpers::TaskStatus;
use cortex::models::{Corpus, HistoricalRun, NewCorpus, NewService, NewTask, Service};
use cortex::schema::{corpora, historical_runs, historical_tasks, services, tasks};
use diesel::prelude::*;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;
use serde_json::Value;

// URL-safe (no spaces): the name travels in the request path.
const CORPUS_NAME: &str = "runs-api-corpus";
const SERVICE_NAME: &str = "runs_api_svc";

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_runs_api_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

/// Clean slate, then seed a corpus + service with two historical runs (the first auto-completed
/// when the second starts, so exactly one run is left open).
fn seed() {
  let mut backend = backend::testdb();
  if let Ok(existing) = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection) {
    diesel::delete(historical_runs::table.filter(historical_runs::corpus_id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
    diesel::delete(tasks::table.filter(tasks::corpus_id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
    diesel::delete(corpora::table.filter(corpora::id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
  }
  diesel::delete(services::table.filter(services::name.eq(SERVICE_NAME)))
    .execute(&mut backend.connection)
    .ok();

  backend
    .add(&NewCorpus {
      name: CORPUS_NAME.to_string(),
      path: "/tmp/runs-api".to_string(),
      complex: true,
      description: String::new(),
    })
    .expect("add corpus");
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("corpus");
  backend
    .add(&NewService {
      name: SERVICE_NAME.to_string(),
      version: 0.1,
      inputformat: "tex".to_string(),
      outputformat: "html".to_string(),
      inputconverter: Some("import".to_string()),
      complex: true,
      description: String::from("runs api service"),
    })
    .expect("add service");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");

  backend
    .mark_new_run(&corpus, &service, "tester".into(), "first run".into())
    .expect("first run");
  backend
    .mark_new_run(&corpus, &service, "tester".into(), "second run".into())
    .expect("second run");
}

// Custom harness (`harness = false` in Cargo.toml): run the cases, then `process::exit(0)` to skip
// the racy diesel/libpq/Tokio teardown that SIGSEGVs *after* assertions pass (KNOWN_ISSUES L-1).
// A panic in any case still aborts the process non-zero, so real failures fail CI.
/// Security regression: a run `description` is user-controlled (rerun/snapshot `--description`) and
/// is serialized into the history chart's `<script>` block; `/history` is **public**. A `</script>`
/// payload must NOT break out of the script tag (stored XSS) — the handler escapes `<`/`>`/`&` to
/// their JSON `\uXXXX` forms, which `JSON.parse` decodes back so the chart is unchanged.
fn history_chart_escapes_script_breakout_in_a_description(client: &Client) {
  let mut backend = backend::testdb();
  let corpus_name = "xss_history_test_corpus";
  let service_name = "xss_history_test_svc";
  if let Ok(existing) = Corpus::find_by_name(corpus_name, &mut backend.connection) {
    diesel::delete(historical_runs::table.filter(historical_runs::corpus_id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
    diesel::delete(corpora::table.filter(corpora::id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
  }
  diesel::delete(services::table.filter(services::name.eq(service_name)))
    .execute(&mut backend.connection)
    .ok();
  backend
    .add(&NewCorpus {
      name: corpus_name.to_string(),
      path: "/tmp/xss-history".to_string(),
      complex: false,
      description: String::new(),
    })
    .expect("corpus");
  let corpus = Corpus::find_by_name(corpus_name, &mut backend.connection).unwrap();
  backend
    .add(&NewService {
      name: service_name.to_string(),
      version: 0.1,
      inputformat: "tex".to_string(),
      outputformat: "html".to_string(),
      inputconverter: None,
      complex: false,
      description: String::new(),
    })
    .expect("service");
  let service = Service::find_by_name(service_name, &mut backend.connection).unwrap();
  // Give the run live tallies (two warning tasks) so it appears in the chart series carrying its
  // description — an empty run produces no bars and would not exercise the serialization.
  for i in 0..2 {
    backend
      .add(&NewTask {
        entry: format!("/tmp/xss-history/{i}.zip"),
        service_id: service.id,
        corpus_id: corpus.id,
        status: TaskStatus::Warning.raw(),
      })
      .expect("add task");
  }
  const PAYLOAD: &str = "XSSPROBE</script><svg onload=alert(1)>";
  backend
    .mark_new_run(&corpus, &service, "tester".into(), PAYLOAD.to_string())
    .expect("run with an XSS payload description");
  // A second run completes the payload run (sets its end_time), so it counts toward
  // `history_length` and renders in the chart series (the chart skips when no run is completed).
  backend
    .mark_new_run(
      &corpus,
      &service,
      "tester".into(),
      "closing run".to_string(),
    )
    .expect("closing run");

  // The chart is now merged into the /runs page (the standalone /history redirects there).
  let body = client
    .get(format!("/runs/{corpus_name}/{service_name}"))
    .dispatch()
    .into_string()
    .expect("runs html");
  // The chart series is present (a completed run with tallies) and carries the payload description.
  assert!(
    body.contains("XSSPROBE"),
    "the payload run's description is serialized into the chart"
  );
  // The raw breakout sequence must NOT appear — the `<` was escaped to `<`.
  assert!(
    !body.contains("XSSPROBE</script>"),
    "a </script> in a run description must NOT break out of the chart <script> (stored XSS)"
  );
  assert!(
    body.contains("XSSPROBE\\u003c"),
    "the payload's `<` is escaped to \\u003c (JSON.parse decodes it back; the chart is unchanged)"
  );

  diesel::delete(corpora::table.filter(corpora::name.eq(corpus_name)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(services::table.filter(services::name.eq(service_name)))
    .execute(&mut backend.connection)
    .ok();
}

/// Run-completion-on-drain at the model boundary (`HistoricalRun::complete_if_drained`, what the
/// dispatcher's finalize loop calls on idle). It must close the open run for a `(corpus, service)`
/// pair EXACTLY when its work is exhausted: never while a `TODO`/`Queued`/`Blocked` task could
/// still produce a result, never for the bookkeeping services (`init`/`import`), and idempotently —
/// freezing the run's final tallies as it closes.
fn run_completion_on_drain_closes_only_a_fully_drained_run() {
  let mut backend = backend::testdb();
  let corpus_name = "drain_complete_corpus";
  let service_name = "drain_complete_svc";
  if let Ok(existing) = Corpus::find_by_name(corpus_name, &mut backend.connection) {
    diesel::delete(historical_runs::table.filter(historical_runs::corpus_id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
    diesel::delete(tasks::table.filter(tasks::corpus_id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
    diesel::delete(corpora::table.filter(corpora::id.eq(existing.id)))
      .execute(&mut backend.connection)
      .ok();
  }
  diesel::delete(services::table.filter(services::name.eq(service_name)))
    .execute(&mut backend.connection)
    .ok();
  backend
    .add(&NewCorpus {
      name: corpus_name.to_string(),
      path: "/tmp/drain-complete".to_string(),
      complex: true,
      description: String::new(),
    })
    .expect("corpus");
  let corpus = Corpus::find_by_name(corpus_name, &mut backend.connection).expect("corpus");
  backend
    .add(&NewService {
      name: service_name.to_string(),
      version: 0.1,
      inputformat: "tex".to_string(),
      outputformat: "html".to_string(),
      inputconverter: Some("import".to_string()),
      complex: true,
      description: String::from("drain-completion service"),
    })
    .expect("service");
  let service = Service::find_by_name(service_name, &mut backend.connection).expect("service");

  // Open a run, as a rerun would.
  backend
    .mark_new_run(&corpus, &service, "tester".into(), "drain run".into())
    .expect("open run");

  // Seed a mixed distribution: 2 NoProblem + 1 Error are terminal; 1 TODO + 1 Blocked are NOT —
  // either alone must keep the run open.
  for i in 0..2 {
    add_task(
      &mut backend.connection,
      &format!("/tmp/drain-complete/ok{i}.zip"),
      service.id,
      corpus.id,
      TaskStatus::NoProblem.raw(),
    );
  }
  add_task(
    &mut backend.connection,
    "/tmp/drain-complete/err.zip",
    service.id,
    corpus.id,
    TaskStatus::Error.raw(),
  );
  let todo_id = add_task(
    &mut backend.connection,
    "/tmp/drain-complete/todo.zip",
    service.id,
    corpus.id,
    TaskStatus::TODO.raw(),
  );
  // Blocked = a status below the terminal band (< -5), gated on a prerequisite service.
  let blocked_id = add_task(
    &mut backend.connection,
    "/tmp/drain-complete/blocked.zip",
    service.id,
    corpus.id,
    TaskStatus::Blocked(-6).raw(),
  );

  // (A) TODO + Blocked present → NOT drained.
  let closed = HistoricalRun::complete_if_drained(corpus.id, service.id, &mut backend.connection)
    .expect("drain check A");
  assert!(!closed, "a run with a TODO + a Blocked task is not drained");
  assert!(
    HistoricalRun::find_current(&corpus, &service, &mut backend.connection)
      .expect("current A")
      .is_some(),
    "the run is still open"
  );

  // (B) Clear the TODO; the lone Blocked task must still hold the run open (the `status < -5`
  // branch — a run gated on a prerequisite is not finished).
  diesel::update(tasks::table.filter(tasks::id.eq(todo_id)))
    .set(tasks::status.eq(TaskStatus::NoProblem.raw()))
    .execute(&mut backend.connection)
    .expect("clear todo");
  let closed = HistoricalRun::complete_if_drained(corpus.id, service.id, &mut backend.connection)
    .expect("drain check B");
  assert!(!closed, "a lone Blocked task still keeps the run open");
  assert!(
    HistoricalRun::find_current(&corpus, &service, &mut backend.connection)
      .expect("current B")
      .is_some(),
    "the run is still open while a task is blocked"
  );

  // (C) Resolve the Blocked task to a terminal Error → fully drained → the run closes, freezing its
  // final tallies (3 NoProblem: the 2 seeded + the cleared TODO; 2 Error: the seed + the resolved
  // block).
  diesel::update(tasks::table.filter(tasks::id.eq(blocked_id)))
    .set(tasks::status.eq(TaskStatus::Error.raw()))
    .execute(&mut backend.connection)
    .expect("resolve block");
  let closed = HistoricalRun::complete_if_drained(corpus.id, service.id, &mut backend.connection)
    .expect("drain check C");
  assert!(closed, "a fully-terminal pair drains → the run is closed");
  assert!(
    HistoricalRun::find_current(&corpus, &service, &mut backend.connection)
      .expect("current C")
      .is_none(),
    "no open run remains after the close"
  );
  let run = HistoricalRun::find_by(&corpus, &service, &mut backend.connection)
    .expect("runs")
    .into_iter()
    .next()
    .expect("the closed run");
  assert!(run.end_time.is_some(), "the closed run has an end_time");
  assert_eq!(run.no_problem, 3, "frozen tallies: 3 no_problem");
  assert_eq!(run.error, 2, "frozen tallies: 2 error");
  assert_eq!(run.total, 5, "frozen total excludes invalids (3 + 2)");

  // (D) Idempotent: re-checking a drained pair closes nothing (no open run left).
  let closed = HistoricalRun::complete_if_drained(corpus.id, service.id, &mut backend.connection)
    .expect("drain check D");
  assert!(
    !closed,
    "re-checking a drained pair closes nothing (idempotent)"
  );

  // (E) Bookkeeping skip: open a run for the `import` service (id 2) with NO tasks — drained by the
  // task count — and assert the `service <= 2` guard refuses to close it (init/import are not
  // user-facing conversion runs, so without the guard a zero-task pair would close immediately).
  let import = Service::find_by_name("import", &mut backend.connection).expect("import service");
  assert!(import.id <= 2, "import is a bookkeeping service id");
  backend
    .mark_new_run(
      &corpus,
      &import,
      "tester".into(),
      "import bookkeeping".into(),
    )
    .expect("open import run");
  let closed = HistoricalRun::complete_if_drained(corpus.id, import.id, &mut backend.connection)
    .expect("drain check E");
  assert!(
    !closed,
    "the bookkeeping import service is skipped even when drained"
  );
  assert!(
    HistoricalRun::find_current(&corpus, &import, &mut backend.connection)
      .expect("current E")
      .is_some(),
    "the import bookkeeping run stays open (guarded)"
  );

  // Clean up so a re-run starts fresh.
  diesel::delete(historical_runs::table.filter(historical_runs::corpus_id.eq(corpus.id)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(tasks::table.filter(tasks::corpus_id.eq(corpus.id)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(corpora::table.filter(corpora::id.eq(corpus.id)))
    .execute(&mut backend.connection)
    .ok();
  diesel::delete(services::table.filter(services::name.eq(service_name)))
    .execute(&mut backend.connection)
    .ok();
}

fn main() {
  // Own the Client in `main` and `process::exit(0)` while it is still alive — never dropping it, so
  // the racy libpq/Tokio/r2d2 teardown never runs (the bench's `process::exit` trick).
  let client = client();
  api_lists_runs_and_reports_current(&client);
  current_run_reports_live_tallies(&client);
  api_task_diff_over_real_snapshots(&client);
  api_runs_is_404_for_unknown_corpus(&client);
  overview_overlays_live_tallies_via_batch(&client);
  overview_lists_runs_system_wide(&client);
  history_chart_escapes_script_breakout_in_a_description(&client);
  run_completion_on_drain_closes_only_a_fully_drained_run();
  eprintln!("runs_test: all cases passed");
  // `_exit` (not `process::exit`): skip C atexit handlers — libpq/OpenSSL global cleanup races with
  // the still-live Tokio/r2d2 threads and SIGSEGVs (L-1). The OS reclaims everything cleanly.
  unsafe { libc::_exit(0) }
}

// An OPEN (current) run must report **live** task tallies, not the zeros it carries until its
// completion freezes them (the "live + historical run state" north star — most visible on the
// current run + the dashboard's last-run card). Seeds known-status tasks, then asserts the
// current-run API surfaces them rather than a row of zeros.
fn current_run_reports_live_tallies(client: &Client) {
  seed(); // clean slate: corpus + service + two runs (the second left open), no tasks yet
  let mut backend = backend::testdb();
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("corpus");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");
  // 3 NoProblem, 1 Warning, 1 Error, 1 Invalid, 2 still-TODO — `total` excludes the invalid but
  // *includes* the unfinished TODO (3+1+1+2 = 7); `in_progress` is the TODO+Queued remainder (2).
  let mut n = 0;
  for (status, count) in [
    (TaskStatus::NoProblem, 3),
    (TaskStatus::Warning, 1),
    (TaskStatus::Error, 1),
    (TaskStatus::Invalid, 1),
    (TaskStatus::TODO, 2),
  ] {
    for _ in 0..count {
      backend
        .add(&NewTask {
          entry: format!("/tmp/runs-api/live-{n}.zip"),
          service_id: service.id,
          corpus_id: corpus.id,
          status: status.raw(),
        })
        .expect("add task");
      n += 1;
    }
  }

  let current: Value = client
    .get(format!("/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/current"))
    .dispatch()
    .into_json()
    .expect("current run json");
  assert_eq!(current["completed"], false, "the second run is still open");
  assert_eq!(
    current["no_problem"], 3,
    "the open run overlays LIVE no_problem, not the frozen zero"
  );
  assert_eq!(
    current["warning"], 1,
    "live warning overlaid on the open run"
  );
  assert_eq!(current["error"], 1, "live error overlaid on the open run");
  assert_eq!(
    current["invalid"], 1,
    "live invalid overlaid on the open run"
  );
  assert_eq!(
    current["in_progress"], 2,
    "the open run reports its LIVE remaining (TODO + Queued) work"
  );
  assert_eq!(
    current["total"], 7,
    "total counts the non-invalid live tasks incl. unfinished TODO (3 + 1 + 1 + 2)"
  );

  // The human run-history screen renders the same live state (symmetry contract): the open run
  // shows as ongoing with its live remaining-work count, not a frozen row of zeros.
  let body = client
    .get(format!("/runs/{CORPUS_NAME}/{SERVICE_NAME}"))
    .dispatch()
    .into_string()
    .expect("runs page html");
  assert!(body.contains("ongoing"), "the open run renders as ongoing");
  assert!(
    body.contains("in&nbsp;progress"),
    "the open run surfaces its live in-progress count on the human screen"
  );
}

// The system-wide run-management overview (`/admin/runs` + `GET /api/runs`). Runs after the seed.
fn overview_lists_runs_system_wide(client: &Client) {
  // The management screen is signed-in-only (with a return path).
  let response = client.get("/admin/runs").dispatch();
  assert!(
    response
      .headers()
      .get_one("Location")
      .unwrap_or("")
      .starts_with("/admin/login?next="),
    "the historical-runs overview requires sign-in"
  );

  // The agent twin lists runs across all corpora/services and includes the seeded one.
  let response = client.get("/api/runs").dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "GET /api/runs lists runs system-wide"
  );
  let body = response.into_string().expect("a JSON body");
  assert!(
    body.contains(CORPUS_NAME),
    "the system-wide run list includes the seeded corpus"
  );

  // Filter-driven: a matching corpus filter keeps the run; an unknown corpus narrows to nothing.
  let body = client
    .get(format!("/api/runs?corpus={CORPUS_NAME}"))
    .dispatch()
    .into_string()
    .expect("json");
  assert!(
    body.contains(CORPUS_NAME),
    "the corpus filter keeps matching runs"
  );
  let body = client
    .get("/api/runs?corpus=no-such-corpus-xyz")
    .dispatch()
    .into_string()
    .expect("json");
  assert!(
    !body.contains(CORPUS_NAME),
    "an unknown corpus filter narrows to nothing"
  );
  // The owner filter (the seed ran as 'tester').
  let body = client
    .get("/api/runs?owner=tester")
    .dispatch()
    .into_string()
    .expect("json");
  assert!(
    body.contains(CORPUS_NAME),
    "the owner filter keeps tester's runs"
  );
  let body = client
    .get("/api/runs?owner=nobody-xyz")
    .dispatch()
    .into_string()
    .expect("json");
  assert!(
    !body.contains(CORPUS_NAME),
    "an owner with no runs narrows to nothing"
  );

  // Signed in, the human screen renders with the seeded run.
  client
    .post("/admin/login")
    .header(ContentType::Form)
    .body("token=token1")
    .dispatch();
  let response = client.get("/admin/runs").dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "signed-in /admin/runs renders"
  );
  let html = response.into_string().expect("an html body");
  assert!(
    html.contains("Historical runs") && html.contains(CORPUS_NAME),
    "the overview lists the seeded run"
  );
}

// The system-wide overview overlays LIVE tallies onto open runs via a single BATCHED query (the
// N+1 fix, KNOWN_ISSUES P-1). Seed a known live distribution and assert the open run in the
// system-wide list reflects it exactly — pinning the batched path to `progress_report`'s numbers.
fn overview_overlays_live_tallies_via_batch(client: &Client) {
  seed(); // corpus + service + two runs (the second left open), no tasks yet
  let mut backend = backend::testdb();
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("corpus");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");
  // 3 NoProblem, 2 Error, 1 TODO: no invalids so total = 6; in_progress = the lone TODO.
  let mut n = 0;
  for (status, count) in [
    (TaskStatus::NoProblem, 3),
    (TaskStatus::Error, 2),
    (TaskStatus::TODO, 1),
  ] {
    for _ in 0..count {
      backend
        .add(&NewTask {
          entry: format!("/tmp/runs-api/batch-{n}.zip"),
          service_id: service.id,
          corpus_id: corpus.id,
          status: status.raw(),
        })
        .expect("add task");
      n += 1;
    }
  }

  let runs: Value = client
    .get(format!("/api/runs?corpus={CORPUS_NAME}"))
    .dispatch()
    .into_json()
    .expect("system-wide runs json");
  let open = runs
    .as_array()
    .expect("array")
    .iter()
    .find(|run| run["corpus"] == CORPUS_NAME && run["completed"] == false)
    .expect("the open run for the seeded corpus");
  assert!(
    open["public_id"]
      .as_str()
      .is_some_and(|h| uuid::Uuid::parse_str(h).is_ok()),
    "the system-wide run overview also carries the UUIDv7 handle"
  );
  assert_eq!(
    open["no_problem"], 3,
    "batched overlay surfaces live no_problem"
  );
  assert_eq!(open["error"], 2, "batched overlay surfaces live error");
  assert_eq!(
    open["in_progress"], 1,
    "batched overlay surfaces live in_progress (the TODO)"
  );
  assert_eq!(
    open["total"], 6,
    "batched total = all non-invalid live tasks (3 + 2 + 1)"
  );
}

// Both assertions live in one case so the shared seed isn't raced by parallel threads.
fn api_lists_runs_and_reports_current(client: &Client) {
  seed();

  // --- List: two runs, exactly one still open -------------------------------------------------
  let response = client
    .get(format!("/api/runs/{CORPUS_NAME}/{SERVICE_NAME}"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));
  let runs: Value = response.into_json().expect("a JSON array");
  let runs = runs.as_array().expect("array");

  assert_eq!(runs.len(), 2, "two historical runs were seeded");
  let open: Vec<&Value> = runs.iter().filter(|r| r["completed"] == false).collect();
  let done: Vec<&Value> = runs.iter().filter(|r| r["completed"] == true).collect();
  assert_eq!(open.len(), 1, "exactly one open run");
  assert_eq!(done.len(), 1, "exactly one completed run");

  let current = open[0];
  assert_eq!(current["description"], "second run");
  assert_eq!(current["owner"], "tester");
  assert!(current["id"].is_number(), "runs carry a stable id handle");
  // The run also carries a UUIDv7 external handle (Arm 3 / D8).
  assert!(
    current["public_id"]
      .as_str()
      .is_some_and(|h| uuid::Uuid::parse_str(h).is_ok()),
    "runs carry a valid UUIDv7 public_id handle"
  );
  assert_eq!(current["end_time"], Value::Null, "open run has no end_time");
  assert!(
    done[0]["end_time"].is_string(),
    "completed run has an end_time"
  );

  // --- Current: the open run -------------------------------------------------------------------
  let response = client
    .get(format!("/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/current"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let current: Value = response.into_json().expect("a JSON value");
  assert_eq!(current["completed"], false);
  assert_eq!(current["description"], "second run");

  // --- Diff: well-formed even with no saved snapshots ------------------------------------------
  let response = client
    .get(format!("/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/diff"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let diff: Value = response.into_json().expect("diff json");
  assert!(
    diff["available_dates"].is_array(),
    "diff carries the available snapshot dates"
  );
  assert!(
    diff["transitions"].is_array(),
    "diff carries a transition matrix"
  );

  // Guard: a malformed snapshot date is a 400, not a panic (the legacy HTML route panics here).
  let response = client
    .get(format!(
      "/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/diff?previous=not-a-date"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "malformed date -> 400, not a panic"
  );

  // --- Per-task diff: well-formed + paginated; an unknown status filter is a 400 ---------------
  let response = client
    .get(format!(
      "/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?page_size=5"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let tasks: Value = response.into_json().expect("tasks json");
  assert!(tasks.is_array(), "per-task diff is a JSON array");

  let response = client
    .get(format!(
      "/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?previous_status=not-a-status"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "unknown status filter -> 400"
  );

  // --- HTML twin: the human run-history screen renders the same runs server-side ---------------
  let response = client
    .get(format!("/runs/{CORPUS_NAME}/{SERVICE_NAME}"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("Run history"),
    "renders the run-history screen"
  );
  assert!(
    body.contains("second run"),
    "renders the seeded run rows server-side"
  );

  // --- HTML twin: the human task-diff screen (the filter-driven heart of run management) --------
  let response = client
    .get(format!("/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("Task severity changes"),
    "renders the task-diff screen"
  );
  assert!(
    body.contains("select-previous-status") && body.contains("select-current-status"),
    "renders the status-transition filter form"
  );

  // Guard: an unknown status filter is a 400 on the HTML twin too (the legacy route panics here).
  let response = client
    .get(format!(
      "/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?previous_status=not-a-status"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "unknown status filter -> 400 on the HTML screen, not a panic"
  );
  // Guard: a malformed snapshot date is a 400, not a panic.
  let response = client
    .get(format!(
      "/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?previous=not-a-date"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "malformed date -> 400 on the HTML screen, not a panic"
  );

  // --- HTML twin: the diff-summary matrix screen (links into the task drill-down) --------------
  let response = client
    .get(format!("/runs/{CORPUS_NAME}/{SERVICE_NAME}/diff"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("Run differences"),
    "renders the diff-summary matrix screen"
  );
  // This seed has runs but no saved task snapshots, so the matrix gracefully reports nothing to
  // compare (the empty-state robustness path) rather than erroring or showing an empty table.
  assert!(
    body.contains("No saved snapshots yet"),
    "diff matrix degrades gracefully when there are no snapshots to compare"
  );

  // Guard: a malformed snapshot date is a 400 on the matrix screen too (legacy route panics here).
  let response = client
    .get(format!(
      "/runs/{CORPUS_NAME}/{SERVICE_NAME}/diff?previous=not-a-date"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::BadRequest,
    "malformed date -> 400 on the matrix screen, not a panic"
  );

  // --- HTML twin: the run-history Vega chart, now merged inline into the /runs screen ------
  let response = client
    .get(format!("/runs/{CORPUS_NAME}/{SERVICE_NAME}"))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("Success rates from"),
    "renders the run-history chart inline on the merged /runs screen"
  );
  assert!(
    body.contains("first run"),
    "renders the seeded completed run in the breakdown table"
  );
}

/// Inserts a task and returns its id (mirrors the backend's import insert).
fn add_task(conn: &mut PgConnection, entry: &str, service: i32, corpus: i32, status: i32) -> i64 {
  diesel::insert_into(tasks::table)
    .values((
      tasks::entry.eq(entry),
      tasks::service_id.eq(service),
      tasks::corpus_id.eq(corpus),
      tasks::status.eq(status),
    ))
    .returning(tasks::id)
    .get_result(conn)
    .expect("insert task")
}

/// Inserts a historical snapshot row at an *explicit* `saved_at` (the `NewHistoricalTask`
/// insertable has no `saved_at`, defaulting to `now()`, so we set it directly to control the two
/// snapshot dates).
fn add_snapshot(conn: &mut PgConnection, task_id: i64, status: i32, saved_at: NaiveDateTime) {
  diesel::insert_into(historical_tasks::table)
    .values((
      historical_tasks::task_id.eq(task_id),
      historical_tasks::status.eq(status),
      historical_tasks::saved_at.eq(saved_at),
    ))
    .execute(conn)
    .expect("insert historical snapshot");
}

/// Regression for the run-diff drill-down over **real saved snapshots** — the path the existing
/// cases never reach (they seed runs but no snapshots, so `report_for` early-returns). Two distinct
/// snapshots are seeded so the diff query actually runs, guarding two fixed bugs:
///   * F-2: the *unfiltered* task-diff `.expect()`ed the status filters and **panicked** (500 that
///     killed the worker) whenever the screen was opened without picking a transition.
///   * the *filtered* drill-down's outer query forgot to restrict to the two requested snapshots,
///     so it returned a task's *entire* snapshot history and the paired rows spanned wrong dates.
fn api_task_diff_over_real_snapshots(client: &Client) {
  seed();
  let mut backend = backend::testdb();
  let corpus = Corpus::find_by_name(CORPUS_NAME, &mut backend.connection).expect("corpus");
  let service = Service::find_by_name(SERVICE_NAME, &mut backend.connection).expect("service");

  // Two snapshots on distinct dates: at d1 every task is an Error; at d2 the first two flip to
  // Warning (a real change) while the last two stay Error (no change).
  let d1 = NaiveDate::from_ymd_opt(2025, 1, 1)
    .unwrap()
    .and_hms_opt(0, 0, 0)
    .unwrap();
  let d2 = NaiveDate::from_ymd_opt(2025, 6, 1)
    .unwrap()
    .and_hms_opt(0, 0, 0)
    .unwrap();
  for i in 0..4 {
    let entry = format!("/tmp/runs-api/doc{i}.zip");
    let task_id = add_task(
      &mut backend.connection,
      &entry,
      service.id,
      corpus.id,
      TaskStatus::Warning.raw(),
    );
    add_snapshot(
      &mut backend.connection,
      task_id,
      TaskStatus::Error.raw(),
      d1,
    );
    let later = if i < 2 {
      TaskStatus::Warning.raw()
    } else {
      TaskStatus::Error.raw()
    };
    add_snapshot(&mut backend.connection, task_id, later, d2);
  }

  // --- Unfiltered: no transition selected. Was a 500 panic (F-2); now 200 listing exactly the two
  // tasks that changed, each pairing the two seeded snapshots.
  let response = client
    .get(format!(
      "/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?page_size=50"
    ))
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Ok,
    "unfiltered task-diff must not panic over real snapshots (regression: F-2)"
  );
  let rows: Value = response.into_json().expect("tasks json");
  let rows = rows.as_array().expect("array");
  assert_eq!(
    rows.len(),
    2,
    "exactly the two status-changed tasks are listed"
  );
  for row in rows {
    assert_eq!(row["previous_status"], "error");
    assert_eq!(row["current_status"], "warning");
    assert_eq!(row["previous_saved_at"], "2025-01-01");
    assert_eq!(row["current_saved_at"], "2025-06-01");
  }

  // --- Filtered drill-down: only the two requested snapshots may appear per task (regression: the
  // outer query previously returned a task's whole snapshot history).
  let response = client
    .get(format!(
      "/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?previous=2025-01-01%2000:00:00&current=2025-06-01%2000:00:00&previous_status=error&current_status=warning"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let rows: Value = response.into_json().expect("tasks json");
  let rows = rows.as_array().expect("array");
  assert_eq!(
    rows.len(),
    2,
    "filtered drill-down lists exactly the matching error->warning tasks"
  );
  for row in rows {
    assert_eq!(
      row["previous_saved_at"], "2025-01-01",
      "only the requested previous snapshot is paired"
    );
    assert_eq!(
      row["current_saved_at"], "2025-06-01",
      "only the requested current snapshot is paired"
    );
  }

  // --- HTML twin of the filtered drill-down: the "Task severity changes" table colour-codes the
  // status cells (by their TaskStatus key) and offers per-row Preview (result) + Source (download)
  // links right after the Entry cell.
  let response = client
    .get(format!(
      "/runs/{CORPUS_NAME}/{SERVICE_NAME}/tasks?previous=2025-01-01%2000:00:00&current=2025-06-01%2000:00:00&previous_status=error&current_status=warning"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let body = response.into_string().expect("html body");
  assert!(
    body.contains("sev-error") && body.contains("sev-warning"),
    "status cells are severity-colour-coded by their TaskStatus key"
  );
  assert!(
    body.contains("/preview/") && body.contains("/entry/import/"),
    "each row offers a result-preview link and a source-download link"
  );

  // --- Summary matrix: now aggregated in SQL (KNOWN_ISSUES R-8) instead of loading every snapshot
  // row. With 2 tasks error->warning and 2 staying error->error, the transition matrix must report
  // exactly those counts — pinning the rewrite to the prior load-and-count-in-Rust behaviour.
  let response = client
    .get(format!(
      "/api/runs/{CORPUS_NAME}/{SERVICE_NAME}/diff?previous=2025-01-01%2000:00:00&current=2025-06-01%2000:00:00"
    ))
    .dispatch();
  assert_eq!(response.status(), Status::Ok);
  let diff: Value = response.into_json().expect("diff json");
  let transitions = diff["transitions"].as_array().expect("transitions array");
  let cell = |prev: &str, cur: &str| -> i64 {
    transitions
      .iter()
      .find(|t| t["previous_status"] == prev && t["current_status"] == cur)
      .and_then(|t| t["task_count"].as_i64())
      .unwrap_or(-1)
  };
  assert_eq!(
    cell("error", "warning"),
    2,
    "two tasks moved error->warning"
  );
  assert_eq!(cell("error", "error"), 2, "two tasks stayed error->error");
  assert_eq!(
    cell("warning", "error"),
    0,
    "no task moved warning->error in this seed"
  );
}

fn api_runs_is_404_for_unknown_corpus(client: &Client) {
  // The agent API and its human twin both 404 on an unknown corpus/service.
  let response = client
    .get("/api/runs/no-such-corpus-xyz/no_such_service")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
  let response = client
    .get("/runs/no-such-corpus-xyz/no_such_service")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
  let response = client
    .get("/runs/no-such-corpus-xyz/no_such_service/tasks")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
  let response = client
    .get("/runs/no-such-corpus-xyz/no_such_service/diff")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
  // /history is now a permanent alias redirecting to /runs; it redirects unconditionally, so an
  // unknown corpus/service yields the redirect (the /runs target then 404s on its own).
  let response = client
    .get("/history/no-such-corpus-xyz/no_such_service")
    .dispatch();
  assert_eq!(response.status(), Status::SeeOther);
}
