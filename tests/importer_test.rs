extern crate cortex;
use cortex::importer::*;

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
}