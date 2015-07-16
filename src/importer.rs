extern crate glob;
extern crate Archive;

use glob::glob;
use regex::Regex;
use Archive::*;
use std::path::Path;
use std::fs;
use std::fs::File;

// Only initialize auxiliary resources once and keep them in a Importer struct
pub struct Importer <'a> {
  pub path : &'a str,
  pub complex : bool
}
impl <'a> Default for Importer <'a> {
  fn default() -> Importer <'a> {
    Importer {
      path : ".",
      complex : false
    }
  }
}

impl <'a> Importer <'a> {
  pub fn unpack(&self) -> Result<(),()> {
    try!(self.unpack_arxiv_top());
    try!(self.unpack_arxiv_months());
    Ok(())
  }
  pub fn unpack_arxiv_top(&self) -> Result<(),()> {
    println!("Greetings from unpack_arxiv_top");
    let path_str = self.path;
    let tars_path = path_str.to_string() + "/*.tar";
    for entry in glob(&tars_path).unwrap() {
      match entry {
        Ok(path) => {
          let base_name = path.file_stem().unwrap().to_str().unwrap();
          // If we wanted fine-grained control, we could infer the dir name:          
          // let arxiv_name_re = Regex::new(r"arXiv_src_(\d+)_").unwrap();
          // let captures = arxiv_name_re.captures(base_name).unwrap();            
          // let unpack_dirname = match captures.at(1) {
          //   Some(month) => month,
          //   None => base_name
          // };
          // --- but not for now

          // Let's open the tar file and unpack it:
          let archive_reader = Reader::new().unwrap()
            .support_filter_all()
            .support_format_all()
            .open_filename(path.to_str().unwrap(), 10240).unwrap();
          loop {
            match archive_reader.next_header() {
              Ok(e) => {
                let full_extract_path = path_str.to_string() + &e.pathname();
                match fs::metadata(full_extract_path.clone()) {
                  Ok(_) => println!("File {:?} exists, won't unpack.", e.pathname()),
                  Err(_) => {
                    println!("To unpack: {:?}", full_extract_path); 
                    let res_code = e.extract_to(&full_extract_path);
                  }
                }
              },
              Err(_) => { break }
            }
          }
        },
        Err(e) => println!("Failed tar glob: {:?}", e),
      }
    }
    Ok(())
  }
  pub fn unpack_arxiv_months(&self) -> Result<(),()> {
    println!("Greetings from unpack_arxiv_months");
    let path_str = self.path;
    let gzs_path = path_str.to_string() + "/*/*.gz";
    for entry in glob(&gzs_path).unwrap() {
      match entry {
        Ok(path) => {
          let entry_path = path.to_str().unwrap();
          let entry_dir = path.parent().unwrap().to_str().unwrap();
          let base_name = path.file_stem().unwrap().to_str().unwrap();
          let entry_cp_dir = entry_dir.to_string() + "/" + base_name;
          fs::create_dir_all(entry_cp_dir.clone()).unwrap_or_else( |reason| {
            println!("Failed to mkdir -p {:?} because: {:?}", entry_cp_dir.clone(), reason.kind());
          });
          // Careful here, some of arXiv's .gz files are really plain-text TeX files (surprise!!!)
          let archive_reader_new = Reader::new().unwrap()
            .support_filter_all()
            .support_format_all()
            .open_filename(entry_path, 10240);
          // We'll write out a ZIP file for each entry
          let archive_writer_new = Writer::new().unwrap();
          match archive_reader_new {
            Err(e) => {
              let raw_reader_new = Reader::new().unwrap()
                .support_filter_all()
                .support_format_raw()
                .open_filename(entry_path, 10240);
              match raw_reader_new {
                Ok(raw_reader) => {
                  println!("Simple TeX file: {:?}", entry_path);
                  match raw_reader.next_header() {
                    Ok(e) => {
                      let full_extract_path = entry_cp_dir.to_string() + "/" + base_name + ".tex";
                      e.extract_to(&full_extract_path);
                    },
                    Err(_) => println!("No content in archive: {:?}", entry_path)
                  }
                },
                Err(_) => println!("Unrecognizeable archive: {:?}", entry_path)
              }
            },
            Ok(archive_reader) => {
              loop {
                match archive_reader.next_header() {
                  Ok(e) => {
                    let full_extract_path = entry_cp_dir.to_string() + "/" + &e.pathname();
                    match fs::metadata(full_extract_path.clone()) {
                      Ok(m) => println!("File {:?} exists, won't unpack.", e.pathname()),
                      Err(_) => {
                        println!("To unpack: {:?}", full_extract_path); 
                        let res_code = e.extract_to(&full_extract_path);
                      }
                    }
                  },
                  Err(_) => { break }
                }
              }
            }
          }
          // Done with this .gz , remove it:
          fs::remove_file(path.clone());
        },
        Err(e) => println!("Failed gz glob: {:?}", e)
      }
    }
    Ok(())
  }

  pub fn process(&self) -> Result<(),()> {
    println!("Greetings from the import processor");
    if self.complex {
      let unpacked_status = self.unpack();
    }
    Ok(())
  }
}