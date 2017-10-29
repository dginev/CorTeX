// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate cortex;
use cortex::importer::*;
use cortex::backend;
use cortex::data::Corpus;

use std::vec::Vec;
use std::fs;
// use std::io::{Error};

fn assert_files(files:Vec<&str>) -> Result<(),std::io::Error> {
  for file in &files {
    let meta = fs::metadata(file);
    assert!(meta.is_ok());
    assert!(meta.unwrap().is_file());
    // They're also temporary, so delete them
    try!(fs::remove_file(file));
  }
  Ok(()) }

fn assert_dirs(dirs : Vec<&str>) -> Result<(),std::io::Error> {
  for dir in &dirs {
    let meta = fs::metadata(dir);
    assert!(meta.is_ok());
    assert!(meta.unwrap().is_dir());
    // They're also temporary, so delete them
    try!(fs::remove_dir(dir));
  }
  Ok(()) }

#[test]
fn can_import_simple() {
  let test_backend = backend::testdb();
  let importer = Importer {
    corpus: test_backend.add(
        Corpus {
          id: None,
          path : "tests/data/".to_string(),
          name : "simple import test".to_string(),
          complex : false }).unwrap(),
    backend: test_backend,
    cwd : Importer::cwd() };

  println!("-- Testing simple import");
  assert!( importer.process().is_ok());
}

#[test]
fn can_import_complex() {
  let test_backend = backend::testdb();
  let importer = Importer {
    corpus: test_backend.add(
      Corpus {
        id: None,
        path : "tests/data/".to_string(),
        name : "complex import test".to_string(),
        complex : true }).unwrap(),
    backend: backend::testdb(),
    cwd : Importer::cwd() };


  println!("-- Testing complex import");
  assert!( importer.process().is_ok() );

  let repeat_importer = Importer {
    corpus: test_backend.add(
      Corpus {
        id: None,
        path : "tests/data/".to_string(),
        name : "complex import test".to_string(),
        complex : true }).unwrap(),
    backend: backend::testdb(),
    cwd : Importer::cwd() };


  println!("-- Testing repeated complex import (successful and no-op)");
  assert!( repeat_importer.process().is_ok());

  let files_removed_ok = assert_files(vec![
    "tests/data/9107/hep-lat9107001/hep-lat9107001.zip",
    "tests/data/9107/hep-lat9107002/hep-lat9107002.zip",
    ]);
  assert!(files_removed_ok.is_ok());
  let dirs_removed_ok = assert_dirs(vec![
    "tests/data/9107/hep-lat9107001",
    "tests/data/9107/hep-lat9107002",
    "tests/data/9107"
  ]);
  assert!(dirs_removed_ok.is_ok());

}