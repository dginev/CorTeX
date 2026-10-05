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

use cortex::backend::{self, Backend, test_db_address};
use cortex::frontend::server::mount_api_with;
use cortex::importer::Importer;
use cortex::models::{Corpus, NewCorpus, NewSandboxCorpus, NewTask};
use cortex::schema::{corpora, tasks};
use diesel::prelude::*;
use rocket::http::{ContentType, Status};
use rocket::local::blocking::Client;
use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

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
/// parent's path but must point at a real parent, and goes away with it.
fn root_paths_are_unique_and_sandboxes_hang_off_a_real_parent(_: &Client) {
  let names = [
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
  add_sandbox(&mut db, "layout_unique_sandbox", path, parent.id)
    .expect("a sandbox may share its parent's path");
  let orphan = add_sandbox(&mut db, "layout_unique_orphan", path, i32::MAX);
  cleanup(&mut db, &["layout_unique_orphan"]);
  assert!(
    orphan.is_err(),
    "a sandbox must reference an existing parent corpus (FK)"
  );
  diesel::delete(corpora::table.filter(corpora::id.eq(parent.id)))
    .execute(&mut db.connection)
    .expect("delete the parent");
  let survivor = Corpus::find_by_name("layout_unique_sandbox", &mut db.connection);
  cleanup(&mut db, &names);
  let _ = std::fs::remove_dir_all(&root);
  assert!(
    survivor.is_err(),
    "a sandbox is deleted with its parent, never left dangling"
  );
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

type Case = (&'static str, fn(&Client));

fn main() {
  let client = client();
  let cases: [Case; 5] = [
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
      "import_at_a_registered_root_path_is_409",
      import_at_a_registered_root_path_is_409,
    ),
  ];
  let mut failed = 0;
  for (name, case) in cases {
    let ok = catch_unwind(AssertUnwindSafe(|| case(&client))).is_ok();
    eprintln!("{} {name}", if ok { "PASS" } else { "FAIL" });
    failed += usize::from(!ok);
  }
  eprintln!("sandbox_layout_test: {failed} of 5 failed");
  unsafe { libc::_exit(i32::from(failed > 0)) }
}
