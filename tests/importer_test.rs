extern crate cortex;
use cortex::importer::*;
use std::vec::Vec;
use std::fs;
use std::io::{Error};

fn assert_files(files:Vec<&str>) -> Result<(),std::io::Error> {
  for file in files.iter() {
    let meta = fs::metadata(file.clone());
    assert!(meta.is_ok());
    assert!(meta.unwrap().is_file());
    // They're also temporary, so delete them
    // try!(fs::remove_file(file.clone())); 
  }
  Ok(()) }

fn assert_dirs(dirs : Vec<&str>) -> Result<(),std::io::Error> {
  for dir in dirs.iter() {
    let meta = fs::metadata(dir.clone());
    assert!(meta.is_ok());
    assert!(meta.unwrap().is_dir());
    // They're also temporary, so delete them
    // try!(fs::remove_dir(dir.clone())); 
  }
  Ok(()) }

#[test]
fn can_import_simple() {
  let importer = Importer {
    complex: false,
    path: "tests/data/"};
  
  println!("-- Testing simple import");
  assert_eq!( importer.process(), Ok(()) );
}

#[test]
fn can_import_complex() {
  let importer = Importer {
    complex: true,
    path: "tests/data/"};
  
  println!("-- Testing complex import");
  assert_eq!( importer.process(), Ok(()) );
  let files_removed_ok = assert_files(vec![
    // "tests/data/9107/hep-lat9107001/9107001.tex",
    // "tests/data/9107/hep-lat9107001/fig1.eps",
    // "tests/data/9107/hep-lat9107002/hep-lat9107002.tex"
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