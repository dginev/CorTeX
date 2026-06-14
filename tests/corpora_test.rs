// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! High-level contract test for the corpus-management capability (read side).

use cortex::backend::{self, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::helpers::TaskStatus;
use cortex::models::{
  Corpus, NewCorpus, NewLogError, NewLogWarning, NewService, NewTask, Service, Task,
};
use cortex::schema::{corpora, historical_runs, log_errors, log_warnings, services, tasks};
use diesel::prelude::*;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_corpora_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

fn api_corpora_lists_registered_corpora() {
  let name = "corpora_capability_test";
  let mut db = backend::testdb();
  let _ = diesel::delete(corpora::table.filter(corpora::name.eq(name))).execute(&mut db.connection);
  db.add(&NewCorpus {
    name: name.to_string(),
    path: "/tmp/corpora_capability_test".to_string(),
    complex: true,
    description: "a test corpus".to_string(),
  })
  .expect("insert a corpus");

  let client = client();
  let response = client.get("/api/corpora").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::JSON));

  let body: serde_json::Value = response.into_json().expect("a JSON body");
  let list = body.as_array().expect("a JSON array");
  let ours = list
    .iter()
    .find(|c| c["name"] == name)
    .expect("our corpus is listed");
  // Data contract:
  assert!(ours["path"].is_string());
  assert!(ours["description"].is_string());
  assert!(ours["complex"].is_boolean());

  let _ = diesel::delete(corpora::table.filter(corpora::name.eq(name))).execute(&mut db.connection);
}

fn cleanup(db: &mut backend::Backend, corpus_name: &str, service_name: &str) {
  if let Ok(corpus) = Corpus::find_by_name(corpus_name, &mut db.connection) {
    let _ = diesel::delete(tasks::table.filter(tasks::corpus_id.eq(corpus.id)))
      .execute(&mut db.connection);
    let _ =
      diesel::delete(corpora::table.filter(corpora::id.eq(corpus.id))).execute(&mut db.connection);
  }
  let _ = diesel::delete(services::table.filter(services::name.eq(service_name)))
    .execute(&mut db.connection);
}

fn api_corpus_detail_reports_services_and_counts() {
  let corpus_name = "corpus_detail_test";
  let service_name = "corpus_detail_svc";
  let mut db = backend::testdb();
  cleanup(&mut db, corpus_name, service_name);

  db.add(&NewCorpus {
    name: corpus_name.to_string(),
    path: "/tmp/corpus_detail_test".to_string(),
    complex: true,
    description: "d".to_string(),
  })
  .expect("insert corpus");
  let corpus = Corpus::find_by_name(corpus_name, &mut db.connection).unwrap();
  db.add(&NewService {
    name: service_name.to_string(),
    version: 0.1,
    inputformat: "tex".to_string(),
    outputformat: "html".to_string(),
    inputconverter: None,
    complex: true,
    description: "d".to_string(),
  })
  .expect("insert service");
  let service = Service::find_by_name(service_name, &mut db.connection).unwrap();
  db.add(&NewTask {
    service_id: service.id,
    corpus_id: corpus.id,
    status: -1, // no_problem
    entry: "/tmp/corpus_detail_test/1/1.zip".to_string(),
  })
  .expect("insert task");

  let client = client();
  let path = format!("/api/corpora/{corpus_name}");
  let response = client.get(path.as_str()).dispatch();
  assert_eq!(response.status(), Status::Ok);

  let body: serde_json::Value = response.into_json().expect("a JSON body");
  assert_eq!(body["name"], corpus_name);
  let services_arr = body["services"].as_array().expect("a services array");
  let svc = services_arr
    .iter()
    .find(|s| s["name"] == service_name)
    .expect("the activated service is listed");
  assert_eq!(svc["total"], 1);
  assert_eq!(svc["no_problem"], 1);

  cleanup(&mut db, corpus_name, service_name);
}

fn api_corpus_detail_is_404_for_unknown_corpus() {
  let client = client();
  let response = client.get("/api/corpora/no_such_corpus_xyz").dispatch();
  assert_eq!(response.status(), Status::NotFound);
  // The HTML twin 404s on an unknown corpus too.
  let response = client.get("/corpus/no_such_corpus_xyz").dispatch();
  assert_eq!(response.status(), Status::NotFound);
}

fn activate_service_requires_a_token() {
  // The activation route is a consequential write (starts processing): denied without a valid
  // rerun token (the Actor guard runs before any DB lookup, so even a bogus corpus is 401).
  let client = client();
  let response = client
    .post("/api/corpora/whatever/services/whatever")
    .dispatch();
  assert_eq!(
    response.status(),
    Status::Unauthorized,
    "activation without a token is 401"
  );
}

fn register_service_creates_tasks_and_attributes_the_run() {
  let corpus_name = "activate_effect_corpus";
  let corpus_path = "/tmp/activate_effect_corpus";
  let target_svc = "activate_target_svc";
  let mut db = backend::testdb();
  cleanup(&mut db, corpus_name, target_svc);

  db.add(&NewCorpus {
    name: corpus_name.to_string(),
    path: corpus_path.to_string(),
    complex: true,
    description: "d".to_string(),
  })
  .expect("corpus");
  let corpus = Corpus::find_by_name(corpus_name, &mut db.connection).unwrap();
  // The magic `import` service must exist (register_service reads its entries); reuse or create it.
  let import = match Service::find_by_name("import", &mut db.connection) {
    Ok(service) => service,
    Err(_) => {
      db.add(&NewService {
        name: "import".to_string(),
        version: 0.1,
        inputformat: "tex".to_string(),
        outputformat: "tex".to_string(),
        inputconverter: None,
        complex: true,
        description: "import".to_string(),
      })
      .expect("import service");
      Service::find_by_name("import", &mut db.connection).unwrap()
    },
  };
  db.add(&NewService {
    name: target_svc.to_string(),
    version: 0.1,
    inputformat: "tex".to_string(),
    outputformat: "html".to_string(),
    inputconverter: Some("import".to_string()),
    complex: true,
    description: "target".to_string(),
  })
  .expect("target service");
  let target = Service::find_by_name(target_svc, &mut db.connection).unwrap();
  // Two imported documents to activate the target service over.
  for entry in [
    "/tmp/activate_effect_corpus/1/1.zip",
    "/tmp/activate_effect_corpus/2/2.zip",
  ] {
    db.add(&NewTask {
      service_id: import.id,
      corpus_id: corpus.id,
      status: TaskStatus::NoProblem.raw(),
      entry: entry.to_string(),
    })
    .expect("import task");
  }

  db.register_service(
    &target,
    corpus_path,
    "activator-bob".to_string(),
    "test activation".to_string(),
  )
  .expect("register_service");

  // A TODO task now exists for the target service over each imported document.
  let target_todo: i64 = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .filter(tasks::service_id.eq(target.id))
    .filter(tasks::status.eq(TaskStatus::TODO.raw()))
    .count()
    .get_result(&mut db.connection)
    .unwrap();
  assert_eq!(target_todo, 2, "a TODO task per imported document");
  // The run is attributed to the activating actor (the owner+description threading).
  let owner: String = historical_runs::table
    .filter(historical_runs::corpus_id.eq(corpus.id))
    .filter(historical_runs::service_id.eq(target.id))
    .order(historical_runs::id.desc())
    .select(historical_runs::owner)
    .first(&mut db.connection)
    .expect("a run was recorded");
  assert_eq!(owner, "activator-bob", "run attributed to the actor");

  // --- Re-activation must not orphan the prior tasks' logs (log_* have no FK to tasks) ----------
  let a_target_task: Task = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .filter(tasks::service_id.eq(target.id))
    .first(&mut db.connection)
    .expect("a target task from the first activation");
  db.add(&NewLogWarning {
    task_id: a_target_task.id,
    category: "c".to_string(),
    what: "w".to_string(),
    details: "d".to_string(),
  })
  .expect("seed a log on the activated task");
  db.register_service(
    &target,
    corpus_path,
    "activator-bob".to_string(),
    "re-activation".to_string(),
  )
  .expect("re-activate");
  let orphaned_logs: i64 = log_warnings::table
    .filter(log_warnings::task_id.eq(a_target_task.id))
    .count()
    .get_result(&mut db.connection)
    .unwrap();
  assert_eq!(
    orphaned_logs, 0,
    "re-activation deletes the prior tasks' logs (no orphans)"
  );
  let target_todo_after: i64 = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .filter(tasks::service_id.eq(target.id))
    .filter(tasks::status.eq(TaskStatus::TODO.raw()))
    .count()
    .get_result(&mut db.connection)
    .unwrap();
  assert_eq!(
    target_todo_after, 2,
    "re-activation recreates a TODO task per imported document"
  );

  cleanup(&mut db, corpus_name, target_svc);
}

fn overview_and_corpus_pages_render_server_side() {
  let corpus_name = "corpus_html_test";
  let service_name = "corpus_html_svc";
  let mut db = backend::testdb();
  cleanup(&mut db, corpus_name, service_name);
  db.add(&NewCorpus {
    name: corpus_name.to_string(),
    path: "/tmp/corpus_html_test".to_string(),
    complex: true,
    description: "d".to_string(),
  })
  .expect("insert corpus");
  let corpus = Corpus::find_by_name(corpus_name, &mut db.connection).unwrap();
  db.add(&NewService {
    name: service_name.to_string(),
    version: 0.1,
    inputformat: "tex".to_string(),
    outputformat: "html".to_string(),
    inputconverter: None,
    complex: true,
    description: "d".to_string(),
  })
  .expect("insert service");
  let service = Service::find_by_name(service_name, &mut db.connection).unwrap();
  // A task makes the service appear in `corpus.select_services` (the screen's source).
  db.add(&NewTask {
    service_id: service.id,
    corpus_id: corpus.id,
    status: -1,
    entry: "/tmp/corpus_html_test/1/1.zip".to_string(),
  })
  .expect("insert task");

  let client = client();

  // Overview screen (HTML twin of /api/corpora): lists our corpus, pooled + server-rendered.
  let response = client.get("/").dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains(corpus_name),
    "overview lists the seeded corpus server-side"
  );

  // Corpus screen (HTML twin of /api/corpora/<name>): lists the activated service.
  let response = client.get(format!("/corpus/{corpus_name}")).dispatch();
  assert_eq!(response.status(), Status::Ok);
  assert_eq!(response.content_type(), Some(ContentType::HTML));
  let body = response.into_string().expect("html body");
  assert!(
    body.contains(service_name),
    "corpus screen lists the activated service server-side"
  );
  // Discoverability: the corpus screen links each service to its run-management screens, so the
  // run-history table is reachable without first drilling into a report (it was orphaned before).
  assert!(
    body.contains("Run history") && body.contains("/runs/"),
    "corpus screen exposes a run-history link per service"
  );
  // Progress dashboard: the corpus screen shows per-service severity counts (the same numbers the
  // agent api_corpus reports), not just service names.
  assert!(
    body.contains("No&nbsp;problem") && body.contains("Fatal"),
    "corpus screen shows the per-service progress columns"
  );

  cleanup(&mut db, corpus_name, service_name);
}

fn cleanup_corpus(db: &mut backend::Backend, corpus_name: &str) {
  if let Ok(corpus) = Corpus::find_by_name(corpus_name, &mut db.connection) {
    let _ = diesel::delete(tasks::table.filter(tasks::corpus_id.eq(corpus.id)))
      .execute(&mut db.connection);
    let _ =
      diesel::delete(corpora::table.filter(corpora::id.eq(corpus.id))).execute(&mut db.connection);
  }
}

fn post_corpora_registers_and_imports_via_a_job() {
  // A tiny non-complex corpus fixture: <root>/doc1/doc1.tex
  let root = std::env::temp_dir().join(format!("cortex_import_test_{}", std::process::id()));
  let entry_dir = root.join("doc1");
  std::fs::create_dir_all(&entry_dir).expect("create fixture dir");
  std::fs::write(
    entry_dir.join("doc1.tex"),
    "\\documentclass{article}\\begin{document}x\\end{document}",
  )
  .expect("write fixture entry");

  let name = "import_via_job_test";
  let mut db = backend::testdb();
  cleanup_corpus(&mut db, name);

  let client = client();
  let body = serde_json::json!({
    "name": name,
    "path": root.to_str().unwrap(),
    "complex": false,
    "description": "imported in a test",
  });
  // Token-gated: an untokened import is denied (no unauthenticated corpus creation + filesystem
  // job).
  let denied = client
    .post("/api/corpora")
    .header(ContentType::JSON)
    .body(body.to_string())
    .dispatch();
  assert_eq!(
    denied.status(),
    Status::Unauthorized,
    "import without a token is 401"
  );
  let response = client
    .post("/api/corpora?token=token1")
    .header(ContentType::JSON)
    .body(body.to_string())
    .dispatch();
  assert_eq!(response.status(), Status::Accepted);
  let job: serde_json::Value = response.into_json().expect("a job handle");
  assert_eq!(job["kind"], "corpus_import");
  assert_eq!(
    job["actor"], "username1",
    "the import job is attributed to the token owner"
  );
  let uuid = job["uuid"].as_str().expect("a uuid").to_string();

  // Poll the import job to a terminal state.
  let path = format!("/api/jobs/{uuid}");
  let mut last = serde_json::Value::Null;
  for _ in 0..500 {
    last = client
      .get(path.as_str())
      .dispatch()
      .into_json()
      .expect("job json");
    let status = last["status"].as_str().unwrap_or_default();
    if status == "succeeded" || status == "failed" || status == "interrupted" {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
  }
  assert_eq!(
    last["status"], "succeeded",
    "import job did not succeed: {}",
    last["message"]
  );

  // The corpus is registered and has import-service tasks.
  let corpus = Corpus::find_by_name(name, &mut db.connection).expect("corpus registered");
  let import_tasks: i64 = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .filter(tasks::service_id.eq(2))
    .count()
    .get_result(&mut db.connection)
    .expect("count import tasks");
  assert!(
    import_tasks >= 1,
    "import should create >=1 import-service task, got {import_tasks}"
  );

  cleanup_corpus(&mut db, name);
  let _ = std::fs::remove_dir_all(&root);
}

fn delete_corpus_removes_corpus_tasks_and_logs() {
  let name = "delete_corpus_test";
  let mut db = backend::testdb();
  cleanup_corpus(&mut db, name);
  db.add(&NewCorpus {
    name: name.to_string(),
    path: "/tmp/delete_corpus_test".to_string(),
    complex: true,
    description: String::new(),
  })
  .expect("insert corpus");
  let corpus = Corpus::find_by_name(name, &mut db.connection).unwrap();
  db.add(&NewTask {
    service_id: 2,
    corpus_id: corpus.id,
    status: -2,
    entry: "/tmp/delete_corpus_test/1/1.zip".to_string(),
  })
  .expect("insert task");
  let task: Task = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .first(&mut db.connection)
    .expect("the task row");
  db.add(&NewLogWarning {
    task_id: task.id,
    category: "c".to_string(),
    what: "w".to_string(),
    details: "d".to_string(),
  })
  .expect("insert log");
  // A second severity, to prove the transactional primitive cascades across *all* the log_* tables.
  db.add(&NewLogError {
    task_id: task.id,
    category: "c".to_string(),
    what: "w".to_string(),
    details: "d".to_string(),
  })
  .expect("insert error log");

  let client = client();
  // Token-gated: an untokened delete is denied before anything else (no unauthenticated wipe).
  let untokened_path = format!("/api/corpora/{name}?confirm={name}");
  let untokened = client.delete(untokened_path.as_str()).dispatch();
  assert_eq!(
    untokened.status(),
    Status::Unauthorized,
    "delete without a token is 401 (no unauthenticated corpus wipe)"
  );
  // Missing confirmation (with a valid token, so the guard passes to the confirm check) -> 400.
  let unconfirmed_path = format!("/api/corpora/{name}?token=token1");
  let unconfirmed = client.delete(unconfirmed_path.as_str()).dispatch();
  assert_eq!(unconfirmed.status(), Status::BadRequest);
  // Name echoed as confirmation + a valid token -> 204.
  let confirmed_path = format!("/api/corpora/{name}?confirm={name}&token=token1");
  let confirmed = client.delete(confirmed_path.as_str()).dispatch();
  assert_eq!(confirmed.status(), Status::NoContent);

  // The corpus, its tasks, and its logs are all gone (no orphans).
  assert!(
    Corpus::find_by_name(name, &mut db.connection).is_err(),
    "corpus should be deleted"
  );
  let task_count: i64 = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .count()
    .get_result(&mut db.connection)
    .unwrap();
  assert_eq!(task_count, 0, "tasks should be deleted");
  let log_count: i64 = log_warnings::table
    .filter(log_warnings::task_id.eq(task.id))
    .count()
    .get_result(&mut db.connection)
    .unwrap();
  assert_eq!(log_count, 0, "warning logs should be deleted (no orphans)");
  let error_log_count: i64 = log_errors::table
    .filter(log_errors::task_id.eq(task.id))
    .count()
    .get_result(&mut db.connection)
    .unwrap();
  assert_eq!(
    error_log_count, 0,
    "error logs should be deleted too (cascade covers every log_* table)"
  );
}

fn delete_corpus_is_404_for_unknown() {
  let client = client();
  let response = client
    .delete("/api/corpora/no_such_corpus?confirm=no_such_corpus&token=token1")
    .dispatch();
  assert_eq!(response.status(), Status::NotFound);
}

fn post_corpora_extend_adds_new_entries() {
  // Two entries on disk; the corpus initially knows only doc1.
  let root = std::env::temp_dir().join(format!("cortex_extend_test_{}", std::process::id()));
  for doc in ["doc1", "doc2"] {
    let dir = root.join(doc);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    std::fs::write(
      dir.join(format!("{doc}.tex")),
      "\\documentclass{article}\\begin{document}x\\end{document}",
    )
    .expect("write fixture entry");
  }

  let name = "extend_test_corpus";
  let mut db = backend::testdb();
  cleanup_corpus(&mut db, name);
  db.add(&NewCorpus {
    name: name.to_string(),
    path: root.to_str().unwrap().to_string(),
    complex: false,
    description: String::new(),
  })
  .expect("insert corpus");
  let corpus = Corpus::find_by_name(name, &mut db.connection).unwrap();
  // Seed the pre-existing import task for doc1 (so extend should add only doc2).
  let doc1_entry = root
    .join("doc1")
    .join("doc1.tex")
    .to_str()
    .unwrap()
    .to_string();
  db.add(&NewTask {
    service_id: 2,
    corpus_id: corpus.id,
    status: 0,
    entry: doc1_entry,
  })
  .expect("seed doc1 task");

  let client = client();
  // Token-gated: an untokened extend is denied.
  let denied_path = format!("/api/corpora/{name}/extend");
  let denied = client.post(denied_path.as_str()).dispatch();
  assert_eq!(
    denied.status(),
    Status::Unauthorized,
    "extend without a token is 401"
  );
  let extend_path = format!("/api/corpora/{name}/extend?token=token1");
  let response = client.post(extend_path.as_str()).dispatch();
  assert_eq!(response.status(), Status::Accepted);
  let job: serde_json::Value = response.into_json().expect("a job handle");
  assert_eq!(job["kind"], "corpus_extend");
  let uuid = job["uuid"].as_str().expect("a uuid").to_string();

  let job_path = format!("/api/jobs/{uuid}");
  let mut last = serde_json::Value::Null;
  for _ in 0..500 {
    last = client
      .get(job_path.as_str())
      .dispatch()
      .into_json()
      .expect("job json");
    let status = last["status"].as_str().unwrap_or_default();
    if status == "succeeded" || status == "failed" || status == "interrupted" {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
  }
  assert_eq!(
    last["status"], "succeeded",
    "extend job did not succeed: {}",
    last["message"]
  );

  // doc1 (pre-existing) + doc2 (newly imported) == 2 import tasks.
  let import_tasks: i64 = tasks::table
    .filter(tasks::corpus_id.eq(corpus.id))
    .filter(tasks::service_id.eq(2))
    .count()
    .get_result(&mut db.connection)
    .expect("count import tasks");
  assert_eq!(import_tasks, 2, "extend should add the newly-arrived entry");

  cleanup_corpus(&mut db, name);
  let _ = std::fs::remove_dir_all(&root);
}

fn human_corpus_forms_are_token_and_confirm_gated() {
  // The human screens' corpus-write forms (overview "Add a corpus", corpus-page extend/delete)
  // carry a token field (the Actor guard can't read a form body), resolved like the API guard.
  let name = "human_corpus_forms";
  let mut db = backend::testdb();
  let _ = diesel::delete(corpora::table.filter(corpora::name.eq(name))).execute(&mut db.connection);
  db.add(&NewCorpus {
    name: name.to_string(),
    path: "/tmp/human_corpus_forms".to_string(),
    complex: false,
    description: "d".to_string(),
  })
  .expect("corpus");

  let client = client();
  // Add-a-corpus form with a bad token -> 401 (no unauthenticated corpus creation via the UI).
  let r = client
    .post("/corpus/import")
    .header(ContentType::Form)
    .body("name=x&path=/tmp/x&complex=false&token=bogus")
    .dispatch();
  assert_eq!(
    r.status(),
    Status::Unauthorized,
    "human import bad token -> 401"
  );

  // Extend form with no valid token -> 401.
  let extend_path = format!("/corpus/{name}/extend");
  let r = client
    .post(extend_path.as_str())
    .header(ContentType::Form)
    .body("token=bogus")
    .dispatch();
  assert_eq!(
    r.status(),
    Status::Unauthorized,
    "human extend bad token -> 401"
  );

  // Activate form with a bad token -> 401 (the guard runs before the corpus/service lookup).
  let activate_path = format!("/corpus/{name}/activate");
  let r = client
    .post(activate_path.as_str())
    .header(ContentType::Form)
    .body("service=whatever&token=bogus")
    .dispatch();
  assert_eq!(
    r.status(),
    Status::Unauthorized,
    "human activate bad token -> 401"
  );

  // Delete form: bad token -> 401; valid token + wrong confirm -> 400; valid token + match -> 303.
  let delete_path = format!("/corpus/{name}/delete");
  let bad = client
    .post(delete_path.as_str())
    .header(ContentType::Form)
    .body(format!("confirm={name}&token=bogus"))
    .dispatch();
  assert_eq!(
    bad.status(),
    Status::Unauthorized,
    "human delete bad token -> 401"
  );
  let wrong = client
    .post(delete_path.as_str())
    .header(ContentType::Form)
    .body("confirm=WRONG&token=token1")
    .dispatch();
  assert_eq!(
    wrong.status(),
    Status::BadRequest,
    "human delete wrong confirmation -> 400"
  );
  let ok = client
    .post(delete_path.as_str())
    .header(ContentType::Form)
    .body(format!("confirm={name}&token=token1"))
    .dispatch();
  assert_eq!(
    ok.status(),
    Status::SeeOther,
    "human delete with token + matching confirm redirects (303)"
  );
  assert!(
    Corpus::find_by_name(name, &mut db.connection).is_err(),
    "the corpus was deleted via the human form"
  );
}

// Custom harness (see KNOWN_ISSUES L-1): run the cases then `_exit(0)`.
fn main() {
  api_corpora_lists_registered_corpora();
  api_corpus_detail_reports_services_and_counts();
  api_corpus_detail_is_404_for_unknown_corpus();
  activate_service_requires_a_token();
  register_service_creates_tasks_and_attributes_the_run();
  overview_and_corpus_pages_render_server_side();
  post_corpora_registers_and_imports_via_a_job();
  delete_corpus_removes_corpus_tasks_and_logs();
  delete_corpus_is_404_for_unknown();
  post_corpora_extend_adds_new_entries();
  human_corpus_forms_are_token_and_confirm_gated();
  eprintln!("corpora_test: all cases passed");
  unsafe { libc::_exit(0) }
}
