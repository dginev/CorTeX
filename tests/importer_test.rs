// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
extern crate diesel;
use cortex::importer::*;
use cortex::backend;
use cortex::models::{Corpus, NewCorpus};
use diesel::delete;
use diesel::prelude::*;
use cortex::schema::{corpora, tasks};
use std::fs;

fn assert_files(files: &[&str]) -> Result<(), std::io::Error> {
  for file in files {
    let meta = fs::metadata(file);
    assert!(meta.is_ok());
    assert!(meta.unwrap().is_file());
    // They're also temporary, so delete them
    try!(fs::remove_file(file));
  }
  Ok(())
}

fn assert_dirs(dirs: &[&str]) -> Result<(), std::io::Error> {
  for dir in dirs {
    let meta = fs::metadata(dir);
    assert!(meta.is_ok());
    assert!(meta.unwrap().is_dir());
    // They're also temporary, so delete them
    try!(fs::remove_dir(dir));
  }
  Ok(())
}

#[test]
fn can_import_simple() {
  let test_backend = backend::testdb();
  let name = "simple import test";
  // Clean slate
  let clean_slate = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&test_backend.connection);
  assert!(clean_slate.is_ok());
  let new_corpus = NewCorpus {
    name: name.to_string(),
    path: "tests/data/".to_string(),
    complex: false,
  };
  let add_corpus_result = test_backend.add(&new_corpus);
  assert!(add_corpus_result.is_ok());
  let corpus_result = Corpus::find_by_name(name, &test_backend.connection);
  assert!(corpus_result.is_ok());
  let corpus = corpus_result.unwrap();
  let corpus_id = corpus.id;
  // had a failing test where path and name were being swapped - diesel seems picky about struct
  // layouts matching table column order
  assert_eq!(corpus.name, name);
  let importer = Importer {
    corpus,
    backend: backend::testdb(),
    cwd: Importer::cwd(),
  };

  println!("-- Testing simple import");
  let processed_result = importer.process();
  assert!(processed_result.is_ok());

  // Clean slate
  let clean_slate_post = delete(tasks::table)
    .filter(tasks::corpus_id.eq(corpus_id))
    .execute(&test_backend.connection);
  assert!(clean_slate_post.is_ok());
}

#[test]
fn can_import_complex() {
  let test_backend = backend::testdb();
  let name = "complex import test";
  // Clean slate
  let clean_slate = delete(corpora::table)
    .filter(corpora::name.eq(name))
    .execute(&test_backend.connection);
  assert!(clean_slate.is_ok());

  let new_corpus = NewCorpus {
    name: name.to_string(),
    path: "tests/data/".to_string(),
    complex: true,
  };
  let add_corpus_result = test_backend.add(&new_corpus);
  assert!(add_corpus_result.is_ok());
  let corpus_result = Corpus::find_by_name(name, &test_backend.connection);
  assert!(corpus_result.is_ok());
  let corpus = corpus_result.unwrap();
  let corpus_id = corpus.id;
  let importer = Importer {
    corpus: corpus.clone(),
    backend: backend::testdb(),
    cwd: Importer::cwd(),
  };

  println!("-- Testing complex import");
  assert!(importer.process().is_ok());

  let repeat_importer = Importer {
    corpus,
    backend: backend::testdb(),
    cwd: Importer::cwd(),
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
    .execute(&test_backend.connection);
  assert!(clean_slate_post.is_ok());
}
