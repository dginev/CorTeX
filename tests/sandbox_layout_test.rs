// Copyright 2015-2026 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Contract tests for the sandbox corpus layout: a sandbox is a **view** of a root corpus, never a
//! second corpus at the same location. Regression for the 2026-10-05 monthly release, where
//! `extend_corpora /data/arxmliv/ 2609` imported a whole month into `sandbox-arxiv-2605` because
//! `Corpus::find_by_path` picked an arbitrary one of the three corpora sharing that path.
//!
//! Every fixture lives under a throwaway `/tmp/cortex_sandbox_layout_*` root (never a production
//! `/data` path) and every row goes to the test database only. The custom harness runs **all**
//! cases and reports each PASS/FAIL (so a red run shows every gap at once), then `_exit`s non-zero
//! on any failure (KNOWN_ISSUES L-1).

use cortex::backend::{self, Backend, build_pool, test_db_address};
use cortex::dispatcher::server::{InFlightSet, SandboxCache, ServiceCache};
use cortex::dispatcher::sink::Sink;
use cortex::frontend::server::mount_api_with;
use cortex::helpers::{TaskProgress, TaskReport};
use cortex::importer::Importer;
use cortex::models::{
  Corpus, NewCorpus, NewSandboxCorpus, NewService, NewTask, Service, Task, start_metadata_writer,
};
use cortex::schema::{corpora, services, tasks};
use diesel::prelude::*;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;
use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

/// Sink port for the dispatcher case (distinct from every other dispatcher test's ports).
const SINK_PORT: usize = 57696;

fn client() -> Client {
  let figment = rocket::Config::figment().merge(("template_dir", "templates"));
  let config_file = std::env::temp_dir().join("cortex_sandbox_layout_test.toml");
  Client::tracked(mount_api_with(
    rocket::custom(figment),
    config_file,
    test_db_address(),
  ))
  .expect("a valid rocket instance")
}

/// A throwaway corpus root under `/tmp` holding one `doc1/doc1.tex` entry, so an extend over it
/// has something to (wrongly) import.
fn layout_root(tag: &str) -> PathBuf {
  let root = PathBuf::from(format!(
    "/tmp/cortex_sandbox_layout_{tag}_{}",
    std::process::id()
  ));
  let dir = root.join("doc1");
  std::fs::create_dir_all(&dir).expect("create fixture dir");
  std::fs::write(
    dir.join("doc1.tex"),
    "\\documentclass{article}\\begin{document}x\\end{document}",
  )
  .expect("write fixture entry");
  root
}

fn cleanup(db: &mut Backend, names: &[&str]) {
  for name in names {
    if let Ok(corpus) = Corpus::find_by_name(name, &mut db.connection) {
      let _ = diesel::delete(tasks::table.filter(tasks::corpus_id.eq(corpus.id)))
        .execute(&mut db.connection);
      let _ = diesel::delete(corpora::table.filter(corpora::id.eq(corpus.id)))
        .execute(&mut db.connection);
    }
  }
}

fn add_root(db: &mut Backend, name: &str, path: &str) -> QueryResult<Corpus> {
  db.add(&NewCorpus {
    name: name.to_string(),
    path: path.to_string(),
    complex: false,
    description: String::new(),
  })?;
  Corpus::find_by_name(name, &mut db.connection)
}

/// Inserts a sandbox row of `parent_id` sharing `path` — the carve's own row shape, minus the task
/// copy.
fn add_sandbox(db: &mut Backend, name: &str, path: &str, parent_id: i32) -> QueryResult<Corpus> {
  diesel::insert_into(corpora::table)
    .values(&NewSandboxCorpus {
      path: path.to_string(),
      name: name.to_string(),
      complex: false,
      description: String::new(),
      parent_corpus_id: Some(parent_id),
      selection: None,
    })
    .execute(&mut db.connection)?;
  Corpus::find_by_name(name, &mut db.connection)
}

fn import_task_count(db: &mut Backend, corpus_id: i32) -> i64 {
  tasks::table
    .filter(tasks::corpus_id.eq(corpus_id))
    .filter(tasks::service_id.eq(2))
    .count()
    .get_result(&mut db.connection)
    .expect("count import tasks")
}

/// The database itself enforces the layout: one ROOT corpus per path; a sandbox may share its
/// parent's path but must point at a real parent, and goes away (with its tasks) when the parent is
/// destroyed.
fn root_paths_are_unique_and_sandboxes_hang_off_a_real_parent(_: &Client) {
  let names = [
    "layout_unique_nested",
    "layout_unique_sandbox",
    "layout_unique_orphan",
    "layout_unique_twin",
    "layout_unique_root",
  ];
  let root = layout_root("unique");
  let path = root.to_str().unwrap();
  let mut db = backend::testdb();
  cleanup(&mut db, &names);

  let parent = add_root(&mut db, "layout_unique_root", path).expect("the first root at a path");
  let twin = add_root(&mut db, "layout_unique_twin", path);
  cleanup(&mut db, &["layout_unique_twin"]);
  assert!(
    twin.is_err(),
    "a second ROOT corpus at an already-registered path must be refused by the database"
  );
  let sandbox = add_sandbox(&mut db, "layout_unique_sandbox", path, parent.id)
    .expect("a sandbox may share its parent's path");
  db.add(&NewTask {
    service_id: 2,
    corpus_id: sandbox.id,
    status: 0,
    entry: path.to_string(),
  })
  .expect("sandbox task");
  // Carved from the sandbox, not the root: the cascade (and the delete preview) reach it too.
  let nested =
    add_sandbox(&mut db, "layout_unique_nested", path, sandbox.id).expect("a sandbox of a sandbox");
  let orphan = add_sandbox(&mut db, "layout_unique_orphan", path, i32::MAX);
  cleanup(&mut db, &["layout_unique_orphan"]);
  assert!(
    orphan.is_err(),
    "a sandbox must reference an existing parent corpus (FK)"
  );
  let listed = parent
    .sandboxes(&mut db.connection)
    .expect("list sandboxes");
  parent
    .destroy(&mut db.connection)
    .expect("destroy the parent");
  let survivor = Corpus::find_by_name("layout_unique_sandbox", &mut db.connection);
  let nested_survivor = Corpus::find_by_name(&nested.name, &mut db.connection);
  let sandbox_tasks: i64 = tasks::table
    .filter(tasks::corpus_id.eq(sandbox.id))
    .count()
    .get_result(&mut db.connection)
    .expect("count sandbox tasks");
  cleanup(&mut db, &names);
  let _ = std::fs::remove_dir_all(&root);
  assert_eq!(
    listed,
    ["layout_unique_nested", "layout_unique_sandbox"],
    "the delete preview lists the sandboxes that go with the parent"
  );
  assert!(
    survivor.is_err(),
    "a sandbox is deleted with its parent, never left dangling"
  );
  assert!(
    nested_survivor.is_err(),
    "a sandbox carved from a sandbox goes too"
  );
  assert_eq!(sandbox_tasks, 0, "the sandbox's tasks go with it");
}

/// Path lookup resolves the ROOT corpus, whatever the heap order of the rows sharing the path.
fn find_by_path_resolves_the_root_never_a_sandbox(_: &Client) {
  let names = [
    "layout_lookup_sb1",
    "layout_lookup_sb2",
    "layout_lookup_root",
  ];
  let root = layout_root("lookup");
  let path = root.to_str().unwrap();
  let mut db = backend::testdb();
  cleanup(&mut db, &names);

  let parent = add_root(&mut db, "layout_lookup_root", path).expect("root");
  add_sandbox(&mut db, "layout_lookup_sb1", path, parent.id).expect("sandbox 1");
  add_sandbox(&mut db, "layout_lookup_sb2", path, parent.id).expect("sandbox 2");
  // Rewriting the root moves its tuple behind the sandboxes' in the heap — the shape production
  // reached, where an unordered `.first()` returned a sandbox.
  for i in 0..3 {
    diesel::update(corpora::table.filter(corpora::id.eq(parent.id)))
      .set(corpora::description.eq(format!("touched {i}")))
      .execute(&mut db.connection)
      .expect("touch root");
  }
  let found = Corpus::find_by_path(path, &mut db.connection).map(|c| c.name);
  cleanup(&mut db, &names);
  let _ = std::fs::remove_dir_all(&root);
  assert_eq!(
    found.as_deref(),
    Ok("layout_lookup_root"),
    "find_by_path must return the root corpus, never one of its sandboxes"
  );
}

/// A sandbox is a frozen snapshot: extending it is refused on the library path (shared by the CLI
/// `cortex extend` and `extend_corpora`) and with a synchronous `409` on the agent API — and no
/// entries are imported into it either way.
fn extend_refuses_a_sandbox(client: &Client) {
  let names = ["layout_extend_sandbox", "layout_extend_root"];
  let root = layout_root("extend");
  let path = root.to_str().unwrap();
  let mut db = backend::testdb();
  cleanup(&mut db, &names);

  let parent = add_root(&mut db, "layout_extend_root", path).expect("root");
  let sandbox = add_sandbox(&mut db, "layout_extend_sandbox", path, parent.id).expect("sandbox");

  let mut importer = Importer {
    corpus: sandbox.clone(),
    backend: backend::testdb(),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };
  let library = importer.extend_corpus();
  let imported_by_library = import_task_count(&mut db, sandbox.id);

  let response = client
    .post("/api/corpora/layout_extend_sandbox/extend?token=token1")
    .dispatch();
  let api_status = response.status();

  cleanup(&mut db, &names);
  let _ = std::fs::remove_dir_all(&root);
  assert!(
    library.is_err(),
    "Importer::extend_corpus must refuse a sandbox"
  );
  assert_eq!(
    imported_by_library, 0,
    "no entries may be imported into a sandbox"
  );
  assert_eq!(
    api_status,
    Status::Conflict,
    "POST /api/corpora/<sandbox>/extend is a synchronous 409, not a spawned job"
  );
}

/// Deleting a sandbox removes only the sandbox's rows — never the parent's init task, which shares
/// the same `entry` (the corpus path).
fn destroying_a_sandbox_keeps_the_parents_init_task(_: &Client) {
  let names = ["layout_destroy_sandbox", "layout_destroy_root"];
  let root = layout_root("destroy");
  let path = root.to_str().unwrap();
  let mut db = backend::testdb();
  cleanup(&mut db, &names);

  let parent = add_root(&mut db, "layout_destroy_root", path).expect("root");
  db.add(&NewTask {
    service_id: 1,
    corpus_id: parent.id,
    status: 0,
    entry: path.to_string(),
  })
  .expect("parent init task");
  let sandbox = add_sandbox(&mut db, "layout_destroy_sandbox", path, parent.id).expect("sandbox");
  sandbox
    .destroy(&mut db.connection)
    .expect("destroy the sandbox");
  let parent_init: i64 = tasks::table
    .filter(tasks::corpus_id.eq(parent.id))
    .filter(tasks::service_id.eq(1))
    .count()
    .get_result(&mut db.connection)
    .expect("count parent init tasks");

  cleanup(&mut db, &names);
  let _ = std::fs::remove_dir_all(&root);
  assert_eq!(
    parent_init, 1,
    "destroying a sandbox must not delete its parent's init task"
  );
}

/// Deleting a ROOT also clears an init task queued for its path under a placeholder corpus id (the
/// `examples/tex_to_html_import.rs` flow), or a re-import trips the tasks unique key.
fn destroying_a_root_clears_its_placeholder_init_task(_: &Client) {
  let names = ["layout_placeholder_root", "layout_placeholder_other"];
  let root = layout_root("placeholder");
  let other = layout_root("placeholder_other");
  let path = root.to_str().unwrap();
  let mut db = backend::testdb();
  cleanup(&mut db, &names);

  let placeholder = add_root(&mut db, "layout_placeholder_other", other.to_str().unwrap())
    .expect("placeholder corpus");
  let corpus = add_root(&mut db, "layout_placeholder_root", path).expect("root");
  db.add(&NewTask {
    service_id: 1,
    corpus_id: placeholder.id,
    status: 0,
    entry: path.to_string(),
  })
  .expect("placeholder init task");
  corpus
    .destroy(&mut db.connection)
    .expect("destroy the root");
  let stray: i64 = tasks::table
    .filter(tasks::entry.eq(path))
    .filter(tasks::service_id.eq(1))
    .count()
    .get_result(&mut db.connection)
    .expect("count init tasks at the path");

  cleanup(&mut db, &names);
  let _ = std::fs::remove_dir_all(&root);
  let _ = std::fs::remove_dir_all(&other);
  assert_eq!(
    stray, 0,
    "destroying a root must clear the init task queued for its path"
  );
}

/// Registering a second root corpus at an already-registered path is a `409` up front (the same
/// courtesy as a name clash), not a 500 from the unique index or a duplicate import.
fn import_at_a_registered_root_path_is_409(client: &Client) {
  let names = ["layout_import_dup", "layout_import_root"];
  let root = layout_root("import");
  let path = root.to_str().unwrap();
  let mut db = backend::testdb();
  cleanup(&mut db, &names);

  add_root(&mut db, "layout_import_root", path).expect("root");
  let body = serde_json::json!({
    "name": "layout_import_dup", "path": path, "complex": false, "description": "dup",
  });
  let status = client
    .post("/api/corpora?token=token1")
    .header(ContentType::JSON)
    .body(body.to_string())
    .dispatch()
    .status();

  cleanup(&mut db, &names);
  let _ = std::fs::remove_dir_all(&root);
  assert_eq!(
    status,
    Status::Conflict,
    "a second root corpus at a registered path is a 409"
  );
}

/// A result the sink cannot place — its corpus's sandbox status is unknown (the ventilator's lookup
/// failed) — is dropped but its task goes **back in flight**, so the reaper retries it (re-running
/// the lookup) or dead-letters it. It is never stranded `Queued` until a dispatcher restart.
fn an_unplaceable_result_goes_back_to_the_reaper(_: &Client) {
  const SERVICE: &str = "layout_sink_svc";
  let root = layout_root("sink");
  let mut db = backend::testdb();
  let _ =
    diesel::delete(services::table.filter(services::name.eq(SERVICE))).execute(&mut db.connection);
  db.add(&NewService {
    name: SERVICE.into(),
    version: 0.1,
    inputformat: "tex".into(),
    outputformat: "html".into(),
    inputconverter: Some("import".into()),
    complex: true,
    description: "sandbox layout sink case".into(),
  })
  .expect("add service");
  let service = Service::find_by_name(SERVICE, &mut db.connection).expect("service");

  // Two synthetic in-flight tasks (the sink never reads their rows): `unknown` on a corpus the
  // sandbox cache has no answer for, `ordinary` on a corpus it knows is not a sandbox.
  let entry = root.join("doc1/doc1.zip").to_string_lossy().into_owned();
  let progress = |id: i64, corpus_id: i32| TaskProgress {
    task: Task {
      id,
      service_id: service.id,
      corpus_id,
      status: 1,
      entry: entry.clone(),
    },
    created_at: chrono::Utc::now().timestamp(),
    retries: 0,
    lease_timeout_seconds: 3600,
  };
  let (unknown, ordinary) = (i64::from(i32::MAX) + 7, i64::from(i32::MAX) + 8);
  let in_flight = Arc::new(InFlightSet::new());
  // Non-default lease bookkeeping, which the re-insert must keep (or the task never dead-letters).
  in_flight.insert(TaskProgress {
    retries: 1,
    created_at: 1,
    ..progress(unknown, i32::MAX - 1)
  });
  in_flight.insert(progress(ordinary, i32::MAX - 2));
  let sandboxes = Arc::new(SandboxCache::new());
  sandboxes.insert(i32::MAX - 2, None);
  let services_cache = Arc::new(ServiceCache::new());
  services_cache.insert(SERVICE.to_string(), Some(service));

  let (done_tx, done_rx) = sync_channel::<TaskReport>(8);
  let metadata = start_metadata_writer(build_pool(test_db_address(), 1));
  let (sink_in_flight, sink_sandboxes) = (Arc::clone(&in_flight), Arc::clone(&sandboxes));
  // Detached: with the task back in flight the sink never drains, and `_exit` ends the process.
  std::thread::spawn(move || {
    let _ = Sink {
      port: SINK_PORT,
      queue_size: 8,
      message_size: 100_000,
      backend_address: test_db_address().to_string(),
      metadata,
    }
    .start(
      &services_cache,
      &sink_sandboxes,
      &sink_in_flight,
      &done_tx,
      None,
      &Arc::new(AtomicBool::new(false)),
    );
  });

  let ctx = zmq::Context::new();
  let push = ctx.socket(zmq::PUSH).expect("push socket");
  push
    .connect(&format!("tcp://127.0.0.1:{SINK_PORT}"))
    .expect("connect to sink");
  // A real result archive (a `cortex.log` → NoProblem), so the placeable result is reported.
  let mut zipped = std::io::Cursor::new(Vec::new());
  let mut zw = zip::ZipWriter::new(&mut zipped);
  zw.start_file("cortex.log", zip::write::SimpleFileOptions::default())
    .expect("zip entry");
  std::io::Write::write_all(&mut zw, b"info:conversion:0\n").expect("zip write");
  zw.finish().expect("zip finish");
  let zipped = zipped.into_inner();
  for id in [unknown, ordinary] {
    let taskid = id.to_string();
    push
      .send_multipart(
        [
          b"layout-worker".as_ref(),
          SERVICE.as_bytes(),
          taskid.as_bytes(),
          zipped.as_slice(),
        ],
        0,
      )
      .expect("send result");
  }
  // The sink handles results in order, so the `ordinary` report means `unknown` was handled too.
  let reported = done_rx
    .recv_timeout(Duration::from_secs(30))
    .map(|r| r.task.id);
  let still_in_flight = in_flight.len();
  let back_in_flight = in_flight.remove(unknown).map(|p| (p.retries, p.created_at));

  let _ =
    diesel::delete(services::table.filter(services::name.eq(SERVICE))).execute(&mut db.connection);
  let _ = std::fs::remove_dir_all(&root);
  assert_eq!(
    reported,
    Ok(ordinary),
    "only the placeable result is reported"
  );
  assert_eq!(
    back_in_flight,
    Some((1, 1)),
    "an unplaceable result's task must go back in flight for the reaper, retry count intact — \
     not be stranded Queued"
  );
  assert_eq!(
    still_in_flight, 1,
    "a reported task must not go back in flight"
  );
}

type Case = (&'static str, fn(&Client));

fn main() {
  let client = client();
  let cases: [Case; 7] = [
    (
      "root_paths_are_unique_and_sandboxes_hang_off_a_real_parent",
      root_paths_are_unique_and_sandboxes_hang_off_a_real_parent,
    ),
    (
      "find_by_path_resolves_the_root_never_a_sandbox",
      find_by_path_resolves_the_root_never_a_sandbox,
    ),
    ("extend_refuses_a_sandbox", extend_refuses_a_sandbox),
    (
      "destroying_a_sandbox_keeps_the_parents_init_task",
      destroying_a_sandbox_keeps_the_parents_init_task,
    ),
    (
      "destroying_a_root_clears_its_placeholder_init_task",
      destroying_a_root_clears_its_placeholder_init_task,
    ),
    (
      "import_at_a_registered_root_path_is_409",
      import_at_a_registered_root_path_is_409,
    ),
    (
      "an_unplaceable_result_goes_back_to_the_reaper",
      an_unplaceable_result_goes_back_to_the_reaper,
    ),
  ];
  let mut failed = 0;
  for (name, case) in cases {
    let ok = catch_unwind(AssertUnwindSafe(|| case(&client))).is_ok();
    eprintln!("{} {name}", if ok { "PASS" } else { "FAIL" });
    failed += usize::from(!ok);
  }
  eprintln!("sandbox_layout_test: {failed} of {} failed", cases.len());
  unsafe { libc::_exit(i32::from(failed > 0)) }
}
