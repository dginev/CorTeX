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
use std::fs;

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
  let new_corpus = NewCorpus {
    name: name.to_string(),
    path: "tests/data/".to_string(),
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
    ..Importer::default()
  };

  println!("-- Testing simple import");
  let processed_result = importer.process();
  assert!(processed_result.is_ok());

  // Clean slate
  let clean_slate_post = delete(tasks::table)
    .filter(tasks::corpus_id.eq(corpus_id))
    .execute(&mut test_backend.connection);
  assert!(clean_slate_post.is_ok());
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
    ..Importer::default()
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
fn can_import_complex() {
  let mut test_backend = backend::testdb();
  let name = "complex import test";
  // Clean slate
  let clean_slate = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&mut test_backend.connection);
  assert!(clean_slate.is_ok());

  let new_corpus = NewCorpus {
    name: name.to_string(),
    path: "tests/data/".to_string(),
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
    ..Importer::default()
  };

  println!("-- Testing complex import");
  assert!(importer.process().is_ok());

  let mut repeat_importer = Importer {
    corpus,
    backend: backend::testdb(),
    ..Importer::default()
  };

  println!("-- Testing repeated complex import (successful and no-op)");
  assert!(repeat_importer.process().is_ok());

  let files_removed_ok = assert_files(&[
    "tests/data/9107/hep-lat9107001/hep-lat9107001.zip",
    "tests/data/9107/hep-lat9107002/hep-lat9107002.zip",
  ]);
  assert!(files_removed_ok.is_ok());
  let dirs_removed_ok = assert_dirs(&[
    "tests/data/9107/hep-lat9107001",
    "tests/data/9107/hep-lat9107002",
    "tests/data/9107",
  ]);
  assert!(dirs_removed_ok.is_ok());

  // Clean slate
  let clean_slate_post = delete(tasks::table)
    .filter(tasks::corpus_id.eq(corpus_id))
    .execute(&mut test_backend.connection);
  assert!(clean_slate_post.is_ok());
}
