// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
use cortex::backend;
use cortex::importer::*;
use cortex::models::{Corpus, NewCorpus};
use cortex::schema::{corpora, tasks};
use diesel::delete;
use diesel::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

fn assert_files(files: &[&str]) -> Result<(), std::io::Error> {
  for file in files {
    let meta = fs::metadata(file);
    assert!(meta.is_ok());
    assert!(meta.unwrap().is_file());
    // They're also temporary, so delete them
    fs::remove_file(file)?;
  }
  Ok(())
}

/// A private copy of `tests/data` under `/tmp`, one per test: each corpus gets its own root path
/// (`corpora_root_path_key`), and the complex import's unpacking never writes into the repo or
/// races a sibling test.
fn fixture_copy(tag: &str) -> PathBuf {
  let root = PathBuf::from(format!("/tmp/cortex_importer_{tag}_{}", std::process::id()));
  let _ = fs::remove_dir_all(&root);
  let copied = std::process::Command::new("cp")
    .arg("-r")
    .arg("tests/data")
    .arg(&root)
    .status()
    .expect("spawn cp");
  assert!(copied.success(), "copy the tests/data fixture to {root:?}");
  root
}

fn assert_dirs(dirs: &[&str]) -> Result<(), std::io::Error> {
  for dir in dirs {
    let meta = fs::metadata(dir);
    assert!(meta.is_ok());
    assert!(meta.unwrap().is_dir());
    // They're also temporary, so delete them
    fs::remove_dir(dir)?;
  }
  Ok(())
}

#[test]
fn can_import_simple() {
  let mut test_backend = backend::testdb();
  let name = "simple import test";
  // Clean slate
  let clean_slate = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
  assert!(clean_slate.is_ok());
  let root = fixture_copy("simple");
  let new_corpus = NewCorpus {
    name: name.to_string(),
    path: format!("{}/", root.display()),
    complex: false,
    description: String::new(),
  };
  let add_corpus_result = test_backend.add(&new_corpus);
  assert!(add_corpus_result.is_ok());
  let corpus_result = Corpus::find_by_name(name, &mut test_backend.connection);
  assert!(corpus_result.is_ok());
  let corpus = corpus_result.unwrap();
  let corpus_id = corpus.id;
  // had a failing test where path and name were being swapped - diesel seems picky about struct
  // layouts matching table column order
  assert_eq!(corpus.name, name);
  let mut importer = Importer {
    corpus,
    backend: backend::testdb(),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };

  println!("-- Testing simple import");
  let processed_result = importer.process();
  assert!(processed_result.is_ok());

  // Clean slate
  let clean_slate_post = delete(tasks::table)
    .filter(tasks::corpus_id.eq(corpus_id))
    .execute(&mut test_backend.connection);
  assert!(clean_slate_post.is_ok());
  let _ = fs::remove_dir_all(&root);
}

#[test]
fn import_skips_unreadable_paths_instead_of_aborting() {
  // Hostile-data resilience: a single unreadable path (here a broken symlink, whose `metadata()`
  // errors) must not sink the whole import — the valid entries are still imported. Before the walk
  // was hardened, the `fs::metadata(..)?` on the broken symlink aborted the entire walk.
  use std::os::unix::fs::symlink;
  let root = std::env::temp_dir().join("cortex_import_faulttolerance");
  let _ = fs::remove_dir_all(&root);
  let entry_dir = root.join("validentry");
  fs::create_dir_all(&entry_dir).expect("create the valid entry dir");
  fs::write(
    entry_dir.join("validentry.tex"),
    b"\\documentclass{article}",
  )
  .expect("write entry");
  // A broken symlink sibling: `fs::metadata` follows it and errors.
  let _ = symlink("/nonexistent/cortex/import/target", root.join("brokenlink"));

  let mut test_backend = backend::testdb();
  let name = "fault tolerance import test";
  let _ = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
  let new_corpus = NewCorpus {
    name: name.to_string(),
    path: format!("{}/", root.to_str().expect("temp path is UTF-8")),
    complex: false,
    description: String::new(),
  };
  test_backend.add(&new_corpus).expect("add corpus");
  let corpus = Corpus::find_by_name(name, &mut test_backend.connection).expect("corpus");
  let corpus_id = corpus.id;
  let mut importer = Importer {
    corpus,
    backend: backend::testdb(),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };

  let imported = importer
    .walk_import()
    .expect("the broken symlink must not abort the import");
  assert_eq!(
    imported, 1,
    "the one valid entry is imported despite the broken symlink sibling"
  );

  // cleanup
  let _ = delete(tasks::table)
    .filter(tasks::corpus_id.eq(corpus_id))
    .execute(&mut test_backend.connection);
  let _ = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
  let _ = fs::remove_dir_all(&root);
}

#[test]
fn import_walk_terminates_on_a_symlink_loop() {
  // Hostile-data resilience: a symlink loop in the corpus (`loop -> root`) must not make the walk
  // recurse forever (unbounded paths → unbounded tasks + a job that keeps "progressing", so the
  // heartbeat-keyed stale-reap never fires). The depth bound caps it, so the import TERMINATES with
  // a bounded count rather than hanging. (Without the bound this test would hang, not just fail.)
  use std::os::unix::fs::symlink;
  let root = std::env::temp_dir().join("cortex_import_symlink_loop");
  let _ = fs::remove_dir_all(&root);
  let entry_dir = root.join("validentry");
  fs::create_dir_all(&entry_dir).expect("create the valid entry dir");
  fs::write(
    entry_dir.join("validentry.tex"),
    b"\\documentclass{article}",
  )
  .expect("write entry");
  // The self-loop: `root/loop` points back at `root`, so a naive walk would recurse forever.
  symlink(&root, root.join("loop")).expect("create the loop symlink");

  let mut test_backend = backend::testdb();
  let name = "symlink loop import test";
  let _ = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
  let new_corpus = NewCorpus {
    name: name.to_string(),
    path: format!("{}/", root.to_str().expect("temp path is UTF-8")),
    complex: false,
    description: String::new(),
  };
  test_backend.add(&new_corpus).expect("add corpus");
  let corpus = Corpus::find_by_name(name, &mut test_backend.connection).expect("corpus");
  let corpus_id = corpus.id;
  let mut importer = Importer {
    corpus,
    backend: backend::testdb(),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };

  // The key property: this RETURNS (does not hang) — the depth bound terminates the loop.
  let imported = importer
    .walk_import()
    .expect("the symlink loop must not hang the import");
  assert!(
    (1..1000).contains(&imported),
    "the loop is depth-bounded, not infinite (got {imported} imported entries)"
  );

  let _ = delete(tasks::table)
    .filter(tasks::corpus_id.eq(corpus_id))
    .execute(&mut test_backend.connection);
  let _ = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
  let _ = fs::remove_dir_all(&root);
}

#[test]
fn import_does_not_panic_on_glob_metacharacter_path() {
  // A corpus path containing glob metacharacters (here an unterminated `[` character class) makes
  // the complex-import `glob(path/*.tar)` pattern fail to compile. It must fail *gracefully* (an
  // Err out of `process`), not `.unwrap()`-panic the import (the I-1 unpack-path hardening).
  let mut test_backend = backend::testdb();
  let name = "glob metachar import test";
  let _ = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
  let new_corpus = NewCorpus {
    name: name.to_string(),
    path: "/tmp/cortex_glob_[unclosed/".to_string(),
    complex: true,
    description: String::new(),
  };
  test_backend.add(&new_corpus).expect("add corpus");
  let corpus = Corpus::find_by_name(name, &mut test_backend.connection).expect("corpus");
  let mut importer = Importer {
    corpus,
    backend: backend::testdb(),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };

  let result = importer.process();
  assert!(
    result.is_err(),
    "a glob-metacharacter corpus path fails gracefully (Err), not via panic"
  );

  let _ = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
}

#[test]
fn can_import_complex() {
  let mut test_backend = backend::testdb();
  let name = "complex import test";
  // Clean slate
  let clean_slate = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
  assert!(clean_slate.is_ok());

  let root = fixture_copy("complex");
  let new_corpus = NewCorpus {
    name: name.to_string(),
    path: format!("{}/", root.display()),
    complex: true,
    description: String::new(),
  };
  let add_corpus_result = test_backend.add(&new_corpus);
  assert!(add_corpus_result.is_ok());
  let corpus_result = Corpus::find_by_name(name, &mut test_backend.connection);
  assert!(corpus_result.is_ok());
  let corpus = corpus_result.unwrap();
  let corpus_id = corpus.id;
  let mut importer = Importer {
    corpus: corpus.clone(),
    backend: backend::testdb(),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };

  println!("-- Testing complex import");
  assert!(importer.process().is_ok());

  let mut repeat_importer = Importer {
    corpus,
    backend: backend::testdb(),
    cwd: Importer::cwd(),
    active_prefixes: HashSet::new(),
  };

  println!("-- Testing repeated complex import (successful and no-op)");
  assert!(repeat_importer.process().is_ok());

  let under_root = |rel: &[&str]| -> Vec<String> {
    rel
      .iter()
      .map(|r| root.join(r).to_string_lossy().into_owned())
      .collect()
  };
  let files = under_root(&[
    "9107/hep-lat9107001/hep-lat9107001.zip",
    "9107/hep-lat9107002/hep-lat9107002.zip",
  ]);
  let files_removed_ok = assert_files(&files.iter().map(String::as_str).collect::<Vec<_>>());
  assert!(files_removed_ok.is_ok());
  let dirs = under_root(&["9107/hep-lat9107001", "9107/hep-lat9107002", "9107"]);
  let dirs_removed_ok = assert_dirs(&dirs.iter().map(String::as_str).collect::<Vec<_>>());
  assert!(dirs_removed_ok.is_ok());

  // Clean slate
  let clean_slate_post = delete(tasks::table)
    .filter(tasks::corpus_id.eq(corpus_id))
    .execute(&mut test_backend.connection);
  assert!(clean_slate_post.is_ok());
  let _ = fs::remove_dir_all(&root);
}

#[test]
fn find_by_name_is_case_insensitive_and_preserves_stored_case() {
  // A mixed-case display name (e.g. `arXiv`) must resolve at any URL case, and the stored case is
  // preserved so the report title reads `arXiv`, not `arxiv`.
  let mut test_backend = backend::testdb();
  let name = "ArXiv-CaseTest";
  let _ = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
  test_backend
    .add(&NewCorpus {
      name: name.to_string(),
      path: "/tmp/cortex_importer_case_test/".to_string(),
      complex: false,
      description: String::new(),
    })
    .expect("add mixed-case corpus");

  for query in ["arxiv-casetest", "ARXIV-CASETEST", "ArXiv-CaseTest"] {
    let found = Corpus::find_by_name(query, &mut test_backend.connection)
      .unwrap_or_else(|_| panic!("corpus must resolve for query {query:?}"));
    assert_eq!(found.name, name, "the stored case is preserved");
  }

  let _ = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
}
