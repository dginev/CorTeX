extern crate glob;
extern crate Archive;

use glob::glob;
// use regex::Regex;
use Archive::*;
use std::path::Path;
use std::path::PathBuf;
use std::fs;
use std::io::Error;
// use std::fs::File;
use backend::{Task, Corpus, Backend};

// Only initialize auxiliary resources once and keep them in a Importer struct
pub struct Importer {
  pub corpus : Corpus,
  pub backend : Backend
}
impl Default for Importer {
  fn default() -> Importer {
    let default_backend = Backend::default();
    Importer {
      corpus : default_backend.add_corpus(
        Corpus {
          id: None,
          path : ".".to_string(),
          name : "default".to_string(),
          complex : false }).unwrap(),
      backend : default_backend
    }
  }
}

impl Importer {
  pub fn unpack(&self) -> Result<(),()> {
    try!(self.unpack_arxiv_top());
    try!(self.unpack_arxiv_months());
    Ok(())
  }
  pub fn unpack_arxiv_top(&self) -> Result<(),()> {
    // println!("Greetings from unpack_arxiv_top");
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
                    e.extract_to(&full_extract_path, Vec::new()).unwrap();
                  }
                }
              },
              Err(_) => { break }
            }
          }
        },
        Err(e) => {println!("Failed tar glob: {:?}", e)}
      }
    }
    Ok(())
  }
  pub fn unpack_arxiv_months(&self) -> Result<(),()> {
    // println!("Greetings from unpack_arxiv_months");
    let path_str = self.corpus.path.clone();
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
          let full_extract_path = entry_cp_dir.to_string() + "/" + base_name + ".zip";
          let mut archive_writer_new = Writer::new().unwrap()
            //.add_filter(ArchiveFilter::Lzip)
            .set_compression(ArchiveFilter::None)
            .set_format(ArchiveFormat::Zip);
          archive_writer_new.open_filename(&full_extract_path.clone()).unwrap();

          match archive_reader_new {
            Err(_) => {
              let raw_reader_new = Reader::new().unwrap()
                .support_filter_all()
                .support_format_raw()
                .open_filename(entry_path, 10240);
              match raw_reader_new {
                Ok(raw_reader) => {
                  println!("Simple TeX file: {:?}", entry_path);

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
                          Err(_) => {break}
                        };
                      }
                      match archive_writer_new.write_header_new(&tex_target,raw_data.len() as i64) {
                        Ok(_) => {},
                        Err(e) => {
                          println!("Couldn't write header: {:?}", e);
                          break;
                        }
                      }
                      archive_writer_new.write_data(raw_data).unwrap();
                    },
                    Err(_) => println!("No content in archive: {:?}", entry_path)
                  }
                },
                Err(_) => println!("Unrecognizeable archive: {:?}", entry_path)
              }
            },
            Ok(archive_reader) => {
              println!("Paper directirory: {:?}", entry_path);
              loop {
                match archive_reader.next_header() {
                  Ok(e) => {
                    archive_writer_new.write_header(e).unwrap();
                    loop {
                      let entry_data = archive_reader.read_data(10240);
                      match entry_data {
                        Ok(chunk) => { archive_writer_new.write_data(chunk).unwrap(); },
                        Err(_) => { break; }
                      };
                    }
                  },
                  Err(_) => { break; }
                }
              }
            }
          }
          // Done with this .gz , remove it:
          match fs::remove_file(path.clone()) {
            Ok(_) => {},
            Err(e) => println!("Can't remove source .gz: {:?}", e)
          };
        },
        Err(e) => println!("Failed gz glob: {:?}", e)
      }
    }
    Ok(())
  }

  pub fn walk_import<'walk>(&self) -> Result<(),Error> {
    let import_extension = if self.corpus.complex { "zip" } else { "tex" };
    let mut walk_q : Vec<PathBuf> = vec![Path::new(&self.corpus.path).to_owned()];
    let mut import_q : Vec<Task> = Vec::new();
    let mut import_counter = 0;
    while walk_q.len() > 0 { 
      let current_path = walk_q.pop().unwrap();
      let current_metadata = try!(fs::metadata(current_path.clone()));
      if current_metadata.is_dir() { // Ignore files
        // First, test if we just found an entry:
        let current_local_dir = current_path.file_name().unwrap();
        let current_entry = current_local_dir.to_str().unwrap().to_string() + "." + import_extension;
        let current_entry_path = current_path.to_str().unwrap().to_string() + "/" + &current_entry;
        match fs::metadata(current_entry_path.clone()) {
          Ok(_) => {
            // Found the expected file, import this entry:
            println!("Found entry: {:?}", current_entry_path);
            import_counter += 1;
            import_q.push(self.new_task(current_entry_path));
            if import_q.len() >= 1000 {
              // Flush the import queue to backend:
              self.backend.mark_imported(&import_q);
              import_q.clear();
            }
          },
          Err(_) => {
            //  No such entry found, traversing into the directory:
            for subentry in try!(fs::read_dir(current_path.clone())) {
              let subentry = try!(subentry);
              walk_q.push(subentry.path());
            }
          }
        }
      }
    }
    if !import_q.is_empty() {
      self.backend.mark_imported(&import_q); }
    println!("--- Imported {:?} entries.", import_counter);
    Ok(())
  }

  pub fn new_task(&self, entry : String) -> Task {
    Task {id: None, entry : entry, status : -5, corpusid : self.corpus.id.unwrap(), serviceid: 1}
  }

  pub fn process(&self) -> Result<(),()> {
    // println!("Greetings from the import processor");
    if self.corpus.complex { // Complex setup has an unpack step:
      self.unpack().unwrap(); }
    // Walk the directory tree and import the files in the TaskDB:
    self.walk_import().unwrap();

    Ok(())
  }
}