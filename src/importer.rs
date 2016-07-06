// Copyright 2015-2016 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Import a new corpus into the framework

extern crate glob;
extern crate Archive;

use glob::glob;
// use regex::Regex;
use Archive::*;
use std::path::Path;
use std::path::PathBuf;
use std::fs;
use std::env;
use std::io::Error;
use backend::Backend;
use data::{Task, TaskStatus, Corpus};

/// Struct for performing corpus imports into CorTeX
pub struct Importer {
  /// a `Corpus` to be imported, containing all relevant metadata
  pub corpus: Corpus,
  /// a `Backend` on which to persist the import into the Task store
  pub backend: Backend,
  /// the current working directory, to resolve relative paths
  pub cwd: PathBuf,
}
impl Default for Importer {
  fn default() -> Importer {
    let default_backend = Backend::default();
    Importer {
      corpus: default_backend.add(Corpus {
                               id: None,
                               path: ".".to_string(),
                               name: "default".to_string(),
                               complex: false,
                             })
                             .unwrap(),
      backend: default_backend,
      cwd: Importer::cwd(),
    }
  }
}

impl Importer {
  /// Convenience method for (recklessly?) obtaining the current working dir
  pub fn cwd() -> PathBuf {
    env::current_dir().unwrap()
  }
  /// Top-level method for unpacking an arxiv-toplogy corpus from its tar-ed form
  fn unpack(&self) -> Result<(), Error> {
    try!(self.unpack_arxiv_top());
    try!(self.unpack_arxiv_months());
    Ok(())
  }
  fn unpack_extend(&self) -> Result<(), Error> {
    try!(self.unpack_extend_arxiv_top());
    // We can reuse the monthly unpack, as it deletes all unpacked document archives
    // In other words, it always acts as a conservative extension
    try!(self.unpack_arxiv_months());
    Ok(())
  }

  /// Unpack the top-level tar files from an arxiv-topology corpus
  fn unpack_arxiv_top(&self) -> Result<(), Error> {
    println!("-- Starting top-level unpack process");
    let path_str = self.corpus.path.clone();
    let tars_path = path_str.to_string() + "/*.tar";
    for entry in glob(&tars_path).unwrap() {
      match entry {
        Ok(path) => {
          // let base_name = path.file_stem().unwrap().to_str().unwrap();
          // If we wanted fine-grained control, we could infer the dir name:
          // let arxiv_name_re = Regex::new(r"arXiv_src_(\d+)_").unwrap();
          // let captures = arxiv_name_re.captures(base_name).unwrap();
          // let unpack_dirname = match captures.at(1) {
          //   Some(month) => month,
          //   None => base_name
          // };
          // --- but not for now

          // Let's open the tar file and unpack it:
          let archive_reader = Reader::new()
                                 .unwrap()
                                 .support_filter_all()
                                 .support_format_all()
                                 .open_filename(path.to_str().unwrap(), 10240)
                                 .unwrap();
          loop {
            match archive_reader.next_header() {
              Ok(e) => {
                let full_extract_path = path_str.to_string() + &e.pathname();
                match fs::metadata(full_extract_path.clone()) {
                  Ok(_) => println!("File {:?} exists, won't unpack.", e.pathname()),
                  Err(_) => {
                    println!("To unpack: {:?}", full_extract_path);
                    match e.extract_to(&full_extract_path, Vec::new()) {
                      Ok(_) => {}
                      _ => {
                        println!("Failed to extract {:?}", full_extract_path);
                      }
                    }
                  }
                }
              }
              Err(_) => break,
            }
          }
        }
        Err(e) => println!("Failed tar glob: {:?}", e),
      }
    }
    Ok(())
  }
  /// Top-level extension unpacking for arxiv-topology corpora
  fn unpack_extend_arxiv_top(&self) -> Result<(), Error> {
    // What I am trying to figure out right now is how to avoid a monstrous amount of code duplication
    // I don't want to copy-paste all of unpack_arxiv_top in here ...
    println!("-- Starting top-level unpack-extend process");
    let path_str = self.corpus.path.clone();
    let tars_path = path_str.to_string() + "/*.tar";
    for entry in glob(&tars_path).unwrap() {
      match entry {
        Ok(path) => {
          // Let's open the tar file and unpack it:
          let archive_reader = Reader::new()
                                 .unwrap()
                                 .support_filter_all()
                                 .support_format_all()
                                 .open_filename(path.to_str().unwrap(), 10240)
                                 .unwrap();
          loop {
            match archive_reader.next_header() {
              Ok(e) => {
                let full_extract_path = path_str.to_string() + &e.pathname();
                match fs::metadata(full_extract_path.clone()) {
                  Ok(_) => {}//println!("File {:?} exists, won't unpack.", e.pathname()),
                  Err(_) => {
                    // Archive entries end in .gz, let's try that as well, to check if the directory is there
                    let dir_extract_path = &full_extract_path[0..full_extract_path.len() - 3];
                    match fs::metadata(dir_extract_path) {
                      Ok(_) => {}//println!("Directory for {:?} already exists, won't unpack.", e.pathname()),
                      Err(_) => {
                        println!("To unpack: {:?}", full_extract_path);
                        match e.extract_to(&full_extract_path, Vec::new()) {
                          Ok(_) => {}
                          _ => {
                            println!("Failed to extract {:?}", full_extract_path);
                          }
                        }
                      }
                    }
                  }
                }
              }
              Err(_) => break,
            }
          }
        }
        Err(e) => println!("Failed tar glob: {:?}", e),
      }
    }
    Ok(())
  }

  /// Unpack the monthly sub-archives of an arxiv-topology corpus, into the CorTeX organization
  fn unpack_arxiv_months(&self) -> Result<(), Error> {
    println!("-- Starting to unpack monthly .gz archives");
    let path_str = self.corpus.path.clone();
    let gzs_path = path_str.to_string() + "/*/*.gz";
    for entry in glob(&gzs_path).unwrap() {
      match entry {
        Ok(path) => {
          let entry_path = path.to_str().unwrap();
          let entry_dir = path.parent().unwrap().to_str().unwrap();
          let base_name = path.file_stem().unwrap().to_str().unwrap();
          let entry_cp_dir = entry_dir.to_string() + "/" + base_name;
          fs::create_dir_all(entry_cp_dir.clone()).unwrap_or_else(|reason| {
            println!("Failed to mkdir -p {:?} because: {:?}",
                     entry_cp_dir.clone(),
                     reason.kind());
          });
          // Careful here, some of arXiv's .gz files are really plain-text TeX files (surprise!!!)
          let archive_reader_new = Reader::new()
                                     .unwrap()
                                     .support_filter_all()
                                     .support_format_all()
                                     .open_filename(entry_path, 10240);
          // We'll write out a ZIP file for each entry
          let full_extract_path = entry_cp_dir.to_string() + "/" + base_name + ".zip";
          let mut archive_writer_new = Writer::new().unwrap()
            //.add_filter(ArchiveFilter::Lzip)
            .set_compression(ArchiveFilter::None)
            .set_format(ArchiveFormat::Zip);
          archive_writer_new.open_filename(&full_extract_path.clone()).unwrap();

          match archive_reader_new {
            Err(_) => {
              let raw_reader_new = Reader::new()
                                     .unwrap()
                                     .support_filter_all()
                                     .support_format_raw()
                                     .open_filename(entry_path, 10240);
              match raw_reader_new {
                Ok(raw_reader) => {
                  match raw_reader.next_header() {
                    Ok(_) => {
                      let tex_target = base_name.to_string() + ".tex";
                      // In a "raw" read, we don't know the data size in advance. So we bite the bullet and
                      // read the usually tiny tex file in memory, obtaining a size estimate
                      let mut raw_data = Vec::new();
                      loop {
                        let chunk_data = raw_reader.read_data(10240);
                        match chunk_data {
                          Ok(chunk) => raw_data.extend(chunk.into_iter()),
                          Err(_) => break,
                        };
                      }
                      match archive_writer_new.write_header_new(&tex_target, raw_data.len() as i64) {
                        Ok(_) => {}
                        Err(e) => {
                          println!("Couldn't write header: {:?}", e);
                          break;
                        }
                      }
                      match archive_writer_new.write_data(raw_data) {
                        Ok(_) => {}
                        Err(e) => {
                          println!("Failed to write data to {:?} because {:?}",
                                   tex_target.clone(),
                                   e)
                        }
                      };
                    }
                    Err(_) => println!("No content in archive: {:?}", entry_path),
                  }
                }
                Err(_) => println!("Unrecognizeable archive: {:?}", entry_path),
              }
            }
            Ok(archive_reader) => {
              loop {
                match archive_reader.next_header() {
                  Ok(e) => {
                    match archive_writer_new.write_header(e) {
                      Ok(_) => {}
                      _ => {} // TODO: If we need to print an error message, we can do so later.
                    };
                    loop {
                      let entry_data = archive_reader.read_data(10240);
                      match entry_data {
                        Ok(chunk) => {
                          archive_writer_new.write_data(chunk).unwrap();
                        }
                        Err(_) => {
                          break;
                        }
                      };
                    }
                  }
                  Err(_) => {
                    break;
                  }
                }
              }
            }
          }
          // Done with this .gz , remove it:
          match fs::remove_file(path.clone()) {
            Ok(_) => {}
            Err(e) => println!("Can't remove source .gz: {:?}", e),
          };
        }
        Err(e) => println!("Failed gz glob: {:?}", e),
      }
    }
    Ok(())
  }

  /// Given a CorTeX-topology corpus, walk the file system and import it into the Task store
  pub fn walk_import<'walk>(&self) -> Result<(), Error> {
    println!("-- Starting import walk");
    let import_extension = if self.corpus.complex {
      "zip"
    } else {
      "tex"
    };
    let mut walk_q: Vec<PathBuf> = vec![Path::new(&self.corpus.path).to_owned()];
    let mut import_q: Vec<Task> = Vec::new();
    let mut import_counter = 0;
    while walk_q.len() > 0 {
      let current_path = walk_q.pop().unwrap();
      // println!("-- current path {:?}", current_path);
      let current_metadata = try!(fs::metadata(current_path.clone()));
      if current_metadata.is_dir() {
        // Ignore files
        // First, test if we just found an entry:
        let current_local_dir = current_path.file_name().unwrap();
        let current_entry = current_local_dir.to_str().unwrap().to_string() + "." + import_extension;
        let current_entry_path = current_path.to_str().unwrap().to_string() + "/" + &current_entry;
        match fs::metadata(current_entry_path.clone()) {
          Ok(_) => {
            // Found the expected file, import this entry:
            // println!("Found entry: {:?}", current_entry_path);
            import_counter += 1;
            import_q.push(self.new_task(current_entry_path));
            if import_q.len() >= 1000 {
              // Flush the import queue to backend:
              println!("Checkpoint backend writer: job {:?}", import_counter);
              self.backend.mark_imported(&import_q).unwrap(); // TODO: Proper Error-handling
              import_q.clear();
            }
          }
          Err(_) => {
            // No such entry found, traversing into the directory:
            for subentry in try!(fs::read_dir(current_path.clone())) {
              let subentry = try!(subentry);
              walk_q.push(subentry.path());
            }
          }
        }
      }
    }
    if !import_q.is_empty() {
      println!("Checkpoint backend writer: job {:?}", import_q.len());
      self.backend.mark_imported(&import_q).unwrap();
    } // TODO: Proper Error-handling
    println!("--- Imported {:?} entries.", import_counter);
    Ok(())
  }

  /// Create a new NoProblem task for the "import" service and the Importer-specified corpus
  pub fn new_task(&self, entry: String) -> Task {
    let abs_entry: String = if Path::new(&entry).is_relative() {
      let mut new_abs = self.cwd.clone();
      new_abs.push(&entry);
      new_abs.to_str().unwrap().to_string()
    } else {
      entry.clone()
    };

    Task {
      id: None,
      entry: abs_entry,
      status: TaskStatus::NoProblem.raw(),
      corpusid: self.corpus.id.unwrap(),
      serviceid: 2,
    }
  }
  /// Top-level import driver, performs an optional unpack, and then an import into the Task store
  pub fn process(&self) -> Result<(), Error> {
    // println!("Greetings from the import processor");
    if self.corpus.complex {
      // Complex setup has an unpack step:
      try!(self.unpack());
    }
    // Walk the directory tree and import the files in the Task store:
    try!(self.walk_import());

    Ok(())
  }

  /// Top-level corpus extension, performs a check for newly added documents and extracts+adds them to the existing corpus tasks
  pub fn extend_corpus(&self) -> Result<(), Error> {
    if self.corpus.complex {
      // Complex setup has an unpack step:
      try!(self.unpack_extend());
    }
    // Use the regular walk_import, at the cost of more database work,
    // the "Backend::mark_imported" ORM method allows us to insert only if new
    try!(self.walk_import());
    Ok(())
  }
}
