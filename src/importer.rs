// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Import a new corpus into the framework
use glob::glob;
// use regex::Regex;
use crate::backend::Backend;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, NewTask};
use std::env;
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use Archive::*;

const BUFFER_SIZE: usize = 10_240;

/// Struct for performing corpus imports into `CorTeX`
#[derive(Debug)]
pub struct Importer {
  /// a `Corpus` to be imported, containing all relevant metadata
  pub corpus: Corpus,
  /// a `Backend` on which to persist the import into the Task store
  pub backend: Backend,
  /// the current working directory, to resolve relative paths
  pub cwd: PathBuf,
  /// the known prefixes of top-level directories to import
  /// used to avoid re-examining existing directories.
  pub active_prefixes: HashSet<String>
}
impl Default for Importer {
  fn default() -> Importer {
    let default_backend = Backend::default();
    // We'll add a mock corpus to the Importer default but it is
    // *NOT* meant to be used in any real operations, as the Corpus isn't
    // actually registered in the DB.
    Importer {
      corpus: Corpus {
       name: "mock corpus".to_string(),
        id:0,
        path: ".".to_string(),
        complex: true,
        description: String::new(),
      },
      backend: default_backend,
      cwd: Importer::cwd(),
      active_prefixes: HashSet::new(),
    }
  }
}

impl Importer {
  /// Convenience method for (recklessly?) obtaining the current working dir
  pub fn cwd() -> PathBuf { env::current_dir().unwrap() }
  /// Top-level method for unpacking an arxiv-toplogy corpus from its tar-ed form
  fn unpack(&mut self) -> Result<(), Box<dyn Error>> {
    self.unpack_arxiv_top()?;
    self.unpack_arxiv_months()?;
    Ok(())
  }
  fn unpack_extend(&mut self) -> Result<(), Box<dyn Error>> {
    self.unpack_extend_arxiv_top()?;
    // We can reuse the monthly unpack, as it deletes all unpacked document archives
    // In other words, it always acts as a conservative extension
    self.unpack_arxiv_months()?;
    Ok(())
  }

  /// Unpack the top-level tar files from an arxiv-topology corpus
  fn unpack_arxiv_top(&mut self) -> Result<(), Box<dyn Error>> {
    let path_str = self.corpus.path.clone();
    println!("-- Starting top-level unpack at {}", path_str);
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
            .open_filename(path.to_str().unwrap(), BUFFER_SIZE)
            .unwrap();
          while let Ok(e) = archive_reader.next_header() {
            let entry_pathname = e.pathname();
            if entry_pathname.ends_with(".pdf") {
              continue;
            }
            if let Some(base) = entry_pathname.split('/').next() {
              self.active_prefixes.insert(base.to_owned());
            }
            let full_extract_path = path_str.to_string() + &entry_pathname;
            match fs::metadata(full_extract_path.clone()) {
              Ok(_) => println!("File {:?} exists, won't unpack.", entry_pathname),
              Err(_) => {
                println!("To unpack: {:?}", full_extract_path);
                match e.extract_to(&full_extract_path, Vec::new()) {
                  Ok(_) => {},
                  _ => {
                    println!("Failed to extract {:?}", full_extract_path);
                  },
                }
              },
            }
          }
        },
        Err(e) => println!("Failed tar glob: {:?}", e),
      }
    }
    Ok(())
  }
  /// Top-level extension unpacking for arxiv-topology corpora
  fn unpack_extend_arxiv_top(&mut self) -> Result<(), Box<dyn Error>> {
    let mut path_str = self.corpus.path.clone();
    if !path_str.ends_with('/') {
      path_str.push('/');
    }
    println!("-- Starting top-level unpack-extend at {}",path_str);
    let tars_path = path_str.to_string() + "/*.tar";
    for entry in glob(&tars_path).unwrap() {
      match entry {
        Ok(path) => {
          // Let's open the tar file and unpack it:
          let archive_reader = Reader::new()
            .unwrap()
            .support_filter_all()
            .support_format_all()
            .open_filename(path.to_str().unwrap(), BUFFER_SIZE)
            .unwrap();
          while let Ok(e) = archive_reader.next_header() {
            let entry_pathname = e.pathname();
            if entry_pathname.ends_with(".pdf") {
              continue;
            }
            if let Some(base) = entry_pathname.split('/').next() {
              self.active_prefixes.insert(base.to_owned());
            }
            let full_extract_path = path_str.to_string() + &entry_pathname;
            match fs::metadata(full_extract_path.clone()) {
              Ok(_) => {}, //println!("File {:?} exists, won't unpack.", e.pathname()),
              Err(_) => {
                // Archive entries end in .gz, let's try that as well, to check if the directory is
                // there
                let dir_extract_path = &full_extract_path[0..full_extract_path.len() - 3];
                match fs::metadata(dir_extract_path) {
                  Ok(_) => {}, /* println!("Directory for {:?} already exists, won't unpack.", */
                  // e.pathname()),
                  Err(_) => {
                    // println!("To unpack: {:?}", full_extract_path);
                    match e.extract_to(&full_extract_path, Vec::new()) {
                      Ok(_) => {},
                      _ => {
                        println!("Failed to extract {:?}", full_extract_path);
                      },
                    }
                  },
                }
              },
            }
          }
        },
        Err(e) => println!("Failed tar glob: {:?}", e),
      }
    }
    Ok(())
  }

  /// Unpack the monthly sub-archives of an arxiv-topology corpus, into the CorTeX organization
  fn unpack_arxiv_months(&self) -> Result<(), Box<dyn Error>> {
    println!("-- Starting to unpack monthly .gz archives");
    let path_str = self.corpus.path.clone();
    let gzs_paths = if self.active_prefixes.is_empty() {
      vec![path_str + "/*/*.gz"]
    } else {
      self.active_prefixes.iter().map(|ap| format!("{}/{}/*.gz", path_str, ap)).collect()
    };
    let globs_iter = gzs_paths.iter().flat_map(|path| glob(&path).unwrap());

    for entry in globs_iter {
      match entry {
        Ok(path) => {
          let entry_path = path.to_str().unwrap();
          let entry_dir = path.parent().unwrap().to_str().unwrap();
          let base_name = path.file_stem().unwrap().to_str().unwrap();
          let default_tex_target = base_name.to_string() + ".tex";
          let entry_cp_dir = entry_dir.to_string() + "/" + base_name;
          fs::create_dir_all(entry_cp_dir.clone()).unwrap_or_else(|reason| {
            println!(
              "Failed to mkdir -p {:?} because: {:?}",
              entry_cp_dir.clone(),
              reason.kind()
            );
          });
          // We'll write out a ZIP file for each entry
          let full_extract_path = entry_cp_dir.to_string() + "/" + base_name + ".zip";
          let mut archive_writer_new = Writer::new()
            .unwrap()
            //.add_filter(ArchiveFilter::Lzip)
            // .set_compression(ArchiveFilter::None)
            .set_format(ArchiveFormat::Zip);
          archive_writer_new
            .open_filename(&full_extract_path.clone())
            .unwrap();

          // Careful here, some of arXiv's .gz files are really plain-text TeX files (surprise!!!)
          let mut raw_read_needed = false;
          match Reader::new()
            .unwrap()
            .support_filter_all()
            .support_format_all()
            .open_filename(entry_path, BUFFER_SIZE)
          {
            Err(_) => raw_read_needed = true,
            Ok(archive_reader) => {
              let mut file_count = 0;
              while let Ok(e) = archive_reader.next_header() {
                file_count += 1;
                match archive_writer_new.write_header(e) {
                  Ok(_) => {}, // TODO: If we need to print an error message, we can do so later.
                  Err(e2) => println!("Header write failed: {:?}", e2),
                };
                while let Ok(chunk) = archive_reader.read_data(BUFFER_SIZE) {
                  archive_writer_new.write_data(chunk).unwrap();
                }
              }
              if file_count == 0 {
                // Special case (bug? in libarchive crate), single file in .gz
                raw_read_needed = true;
              }
            },
          }

          if raw_read_needed {
            let raw_reader_new = Reader::new()
              .unwrap()
              .support_filter_all()
              .support_format_raw()
              .open_filename(entry_path, BUFFER_SIZE);
            match raw_reader_new {
              Ok(raw_reader) => match raw_reader.next_header() {
                Ok(_) => {
                  single_file_transfer(&default_tex_target, &raw_reader, &mut archive_writer_new);
                },
                Err(_) => println!("No content in archive: {:?}", entry_path),
              },
              Err(_) => println!("Unrecognizeable archive: {:?}", entry_path),
            }
          }
          // Done with this .gz , remove it:
          match fs::remove_file(path.clone()) {
            Ok(_) => {},
            Err(e) => println!("Can't remove source .gz: {:?}", e),
          };
        },
        Err(e) => println!("Failed gz glob: {:?}", e),
      }
    }
    Ok(())
  }

  /// Given a CorTeX-topology corpus, walk the file system and import it into the Task store
  pub fn walk_import(&self) -> Result<usize, Box<dyn Error>> {
    println!("-- Starting import walk");
    let import_extension = if self.corpus.complex { "zip" } else { "tex" };
    let mut walk_q: Vec<PathBuf> = vec![Path::new(&self.corpus.path).to_owned()];
    let mut import_q: Vec<NewTask> = Vec::new();
    let mut import_counter = 0;
    while !walk_q.is_empty() {
      let current_path = walk_q.pop().unwrap();
      let current_metadata = fs::metadata(current_path.clone())?;
      if current_metadata.is_dir() {
        let current_path_str = current_path.to_str().unwrap().to_string();
        let rel_path = current_path_str.replace(&self.corpus.path,"");
        let mut slash_iter = rel_path.split('/');
        slash_iter.next(); // drop the corpus root piece.
        if let Some(base) = slash_iter.next() {
          if !self.active_prefixes.contains(base) {
            continue;
          }
        }
        // First, test if we just found an entry:
        let current_local_dir = current_path.file_name().unwrap();
        let current_entry =
          current_local_dir.to_str().unwrap().to_string() + "." + import_extension;
        let current_entry_path = current_path_str + "/" + &current_entry;
        match fs::metadata(&current_entry_path) {
          Ok(_) => {
            // Found the expected file, import this entry:
            println!("Found entry: {:?}", current_entry_path);
            import_counter += 1;
            import_q.push(self.new_task(&current_entry_path));
            if import_q.len() >= 1000 {
              // Flush the import queue to backend:
              println!("Checkpoint backend writer: job {:?}", import_counter);
              self.backend.mark_imported(&import_q).unwrap(); // TODO: Proper Error-handling
              import_q.clear();
            }
          },
          Err(_) => {
            // No such entry found, traversing into the directory:
            for subentry in fs::read_dir(current_path.clone())? {
              let subentry = subentry?;
              walk_q.push(subentry.path());
            }
          },
        }
      }
    }
    if !import_q.is_empty() {
      println!("Checkpoint backend writer: job {:?}", import_q.len());
      self.backend.mark_imported(&import_q).unwrap();
    } // TODO: Proper Error-handling
    println!("--- Imported {:?} entries.", import_counter);
    Ok(import_counter)
  }

  /// Create a new NoProblem task for the "import" service and the Importer-specified corpus
  pub fn new_task(&self, entry: &str) -> NewTask {
    let abs_entry: String = if Path::new(&entry).is_relative() {
      let mut new_abs = self.cwd.clone();
      new_abs.push(entry);
      new_abs.to_str().unwrap().to_string()
    } else {
      entry.to_string()
    };

    NewTask {
      entry: abs_entry,
      status: TaskStatus::NoProblem.raw(),
      corpus_id: self.corpus.id,
      service_id: 2,
    }
  }
  /// Top-level import driver, performs an optional unpack, and then an import into the Task store
  pub fn process(&mut self) -> Result<(), Box<dyn Error>> {
    // println!("Greetings from the import processor");
    if self.corpus.complex {
      // Complex setup has an unpack step:
      self.unpack()?;
    }
    // Walk the directory tree and import the files in the Task store:
    self.walk_import()?;

    Ok(())
  }

  /// Top-level corpus extension, performs a check for newly added documents and extracts+adds
  /// them to the existing corpus tasks
  pub fn extend_corpus(&mut self) -> Result<(), Box<dyn Error>> {
    if self.corpus.complex {
      // Complex setup has an unpack step:
      self.unpack_extend()?;
    }
    // Before we import, mark any current runs as completed.
    for service in self
      .corpus
      .select_services(&self.backend.connection)
      .unwrap_or_default()
      .iter()
    {
      self.backend.mark_new_run(
        &self.corpus,
        service,
        "cli-admin".to_string(), // command line interface only?
        "extending corpus with more entries".to_string(),
      )?;
    }
    // Use the regular walk_import, at the cost of more database work,
    // the "Backend::mark_imported" ORM method allows us to insert only if new
    self.walk_import()?;
    Ok(())
  }
}

/// Transfer the data contained within `Reader` to a `Writer`, assuming it was a single file
pub fn single_file_transfer(tex_target: &str, reader: &Reader, writer: &mut Writer) {
  // In a "raw" read, we don't know the data size in advance. So we bite the
  // bullet and read the usually tiny tex file in memory,
  // obtaining a size estimate
  let mut raw_data = Vec::new();
  while let Ok(chunk) = reader.read_data(BUFFER_SIZE) {
    raw_data.extend(chunk.into_iter());
  }
  let mut ok_header = false;
  match writer.write_header_new(tex_target, raw_data.len() as i64) {
    Ok(_) => {
      ok_header = true;
    },
    Err(e) => {
      println!("Couldn't write header: {:?}", e);
    },
  }
  if ok_header {
    match writer.write_data(raw_data) {
      Ok(_) => {},
      Err(e) => println!("Failed to write data to {:?} because {:?}", tex_target, e),
    };
  }
}
