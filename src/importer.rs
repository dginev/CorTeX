// Copyright 2015-2025 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Import a new corpus into the framework
use crate::backend::Backend;
use crate::helpers::TaskStatus;
use crate::models::{Corpus, NewTask};
use glob::glob;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::path::PathBuf;

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
  pub active_prefixes: HashSet<String>,
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
        id: 0,
        path: ".".to_string(),
        complex: true,
        description: String::new(),
        parent_corpus_id: None,
        selection: None,
        // A nil sentinel: this mock corpus is never registered in the DB, so it carries no real
        // external handle (a registered corpus gets its UUIDv7 from the column default).
        public_id: uuid::Uuid::nil(),
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
    println!("-- Starting top-level unpack at {path_str}");
    // A corpus path containing glob metacharacters (`[`, `{`, …) makes `glob` return a
    // `PatternError`; propagate it as a clean import failure rather than `.unwrap()`-panicking.
    for entry in glob(&(path_str.clone() + "/*.tar"))? {
      match entry {
        Ok(path) => match unpack_top_tar(&path, &path_str, false) {
          Ok(prefixes) => self.active_prefixes.extend(prefixes),
          Err(reason) => eprintln!("-- import: skipping tar {path:?}: {reason}"),
        },
        Err(e) => println!("Failed tar glob: {e:?}"),
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
    println!("-- Starting top-level unpack-extend at {path_str}");
    for entry in glob(&(path_str.clone() + "/*.tar"))? {
      match entry {
        Ok(path) => match unpack_top_tar(&path, &path_str, true) {
          Ok(prefixes) => self.active_prefixes.extend(prefixes),
          Err(reason) => eprintln!("-- import: skipping tar {path:?}: {reason}"),
        },
        Err(e) => println!("Failed tar glob: {e:?}"),
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
      self
        .active_prefixes
        .iter()
        .map(|ap| format!("{path_str}/{ap}/*.gz"))
        .collect()
    };
    // Skip (with a log) any glob pattern that fails to compile rather than `.unwrap()`-panicking
    // the whole unpack; the valid patterns still contribute their entries.
    let globs_iter = gzs_paths
      .iter()
      .filter_map(|pattern| match glob(pattern) {
        Ok(paths) => Some(paths),
        Err(e) => {
          eprintln!("-- import: skipping invalid glob pattern {pattern:?}: {e}");
          None
        },
      })
      .flatten();
    for entry in globs_iter {
      match entry {
        Ok(path) => {
          if let Err(reason) = unpack_one_gz(&path) {
            eprintln!("-- import: skipping .gz {path:?}: {reason}");
          }
        },
        Err(e) => println!("Failed gz glob: {e:?}"),
      }
    }
    Ok(())
  }
  /// Given a CorTeX-topology corpus, walk the file system and import it into the Task store
  pub fn walk_import(&mut self) -> Result<usize, Box<dyn Error>> {
    let import_extension = if self.corpus.complex { "zip" } else { "tex" };
    let mut walk_q: Vec<PathBuf> = vec![Path::new(&self.corpus.path).to_owned()];
    println!("-- Starting import walk at {}", self.corpus.path);
    let mut import_q: Vec<NewTask> = Vec::new();
    let mut import_counter = 0;
    while let Some(current_path) = walk_q.pop() {
      // arXiv data is hostile: one unreadable path (a broken symlink, a vanished or
      // permission-denied entry) or a non-UTF-8 name must **skip**, not abort the whole import —
      // blast-radius isolation + transparent (logged) failure (docs/DESIGN_PRINCIPLES.md). Only a
      // backend write error is fatal (the DB is essential), and it now propagates as a `Result`
      // rather than `.unwrap()`-panicking.
      let current_metadata = match fs::metadata(&current_path) {
        Ok(meta) => meta,
        Err(e) => {
          eprintln!("-- import: skipping unreadable path {current_path:?}: {e}");
          continue;
        },
      };
      if !current_metadata.is_dir() {
        continue;
      }
      let current_path_str = match current_path.to_str() {
        Some(path_str) => path_str.to_string(),
        None => {
          eprintln!("-- import: skipping non-UTF-8 path {current_path:?}");
          continue;
        },
      };
      let rel_path = current_path_str.replace(&self.corpus.path, "");
      let mut slash_iter = rel_path.split('/');
      if rel_path.starts_with('/') {
        // drop the corpus root piece.
        slash_iter.next();
      }
      if let Some(base) = slash_iter.next() {
        // if we have an "active_prefixes" filter, comply with it
        if !base.is_empty()
          && !self.active_prefixes.is_empty()
          && !self.active_prefixes.contains(base)
        {
          continue;
        }
      }
      // First, test if we just found an entry:
      let current_entry = match current_path.file_name().and_then(|name| name.to_str()) {
        Some(local_dir) => local_dir.to_string() + "." + import_extension,
        None => {
          eprintln!("-- import: skipping path with no usable file name {current_path:?}");
          continue;
        },
      };
      let current_entry_path = current_path_str + "/" + &current_entry;
      match fs::metadata(&current_entry_path) {
        Ok(_) => {
          // Found the expected file, import this entry:
          println!("Found entry: {current_entry_path:?}");
          import_counter += 1;
          import_q.push(self.new_task(&current_entry_path));
          if import_q.len() >= 1000 {
            // Flush the import queue to backend:
            println!("Checkpoint backend writer: job {import_counter:?}");
            self.backend.mark_imported(&import_q)?;
            import_q.clear();
          }
        },
        Err(_) => {
          // No such entry found, traverse into the directory — skipping (not aborting) an
          // unreadable directory or directory entry.
          match fs::read_dir(&current_path) {
            Ok(entries) => {
              for subentry in entries {
                match subentry {
                  Ok(subentry) => walk_q.push(subentry.path()),
                  Err(e) => {
                    eprintln!("-- import: skipping unreadable entry under {current_path:?}: {e}")
                  },
                }
              }
            },
            Err(e) => eprintln!("-- import: skipping unreadable directory {current_path:?}: {e}"),
          }
        },
      }
    }
    if !import_q.is_empty() {
      println!("Checkpoint backend writer: job {:?}", import_q.len());
      self.backend.mark_imported(&import_q)?;
    }
    println!("--- Imported {import_counter:?} entries.");
    Ok(import_counter)
  }

  /// Create a new TODO task for the "import" service and the Importer-specified corpus
  /// (should get marked as "NoProblem" once the extension is completed)
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
      status: TaskStatus::TODO.raw(),
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
      .select_services(&mut self.backend.connection)
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
/// Extracts the entries of one top-level arXiv `.tar` to disk (skipping `.pdf`s, and entries
/// already unpacked), returning the set of top-level prefixes seen. Hostile-data tolerant (I-1): a
/// bad entry is logged + skipped, never a panic. `extend` additionally skips an entry whose
/// unpacked directory (the entry name without its `.gz` suffix) already exists.
fn unpack_top_tar(
  tar_path: &Path,
  path_str: &str,
  extend: bool,
) -> Result<HashSet<String>, String> {
  let file = File::open(tar_path).map_err(|e| format!("open: {e}"))?;
  let mut archive = tar::Archive::new(file);
  let mut prefixes = HashSet::new();
  for entry in archive.entries().map_err(|e| format!("tar entries: {e}"))? {
    let mut entry = match entry {
      Ok(entry) => entry,
      Err(e) => {
        eprintln!("-- import: skipping unreadable tar entry in {tar_path:?}: {e}");
        continue;
      },
    };
    let entry_pathname = match entry.path() {
      Ok(path) => path.to_string_lossy().into_owned(),
      Err(_) => continue,
    };
    if entry_pathname.ends_with(".pdf") {
      continue;
    }
    if let Some(base) = entry_pathname.split('/').next() {
      prefixes.insert(base.to_owned());
    }
    let full_extract_path = format!("{path_str}{entry_pathname}");
    if fs::metadata(&full_extract_path).is_ok() {
      continue; // already unpacked
    }
    if extend {
      let dir_extract_path = full_extract_path
        .strip_suffix(".gz")
        .unwrap_or(&full_extract_path);
      if dir_extract_path != full_extract_path && fs::metadata(dir_extract_path).is_ok() {
        continue;
      }
    }
    if let Some(parent) = Path::new(&full_extract_path).parent() {
      let _ = fs::create_dir_all(parent);
    }
    if let Err(e) = entry.unpack(&full_extract_path) {
      eprintln!("-- import: failed to extract {full_extract_path:?}: {e}");
    }
  }
  Ok(prefixes)
}

/// Repacks one arXiv per-paper `.gz` source into a `<base>.zip` under `<dir>/<base>/`, then removes
/// the `.gz`. The decompressed content is **content-detected** (filenames lie): a `tar` becomes its
/// file entries; headerless content (the arXiv "surprise" — a plain gzipped `.tex`) becomes
/// `<base>.tex`; any other detected type (e.g. a raw PDF) is **rejected** (the `.gz` kept for
/// inspection). Hostile-data tolerant (I-1): returns `Err` rather than panicking, so the caller
/// logs
/// + skips this one and continues the import.
fn unpack_one_gz(path: &Path) -> Result<(), String> {
  let entry_path = path
    .to_str()
    .ok_or_else(|| format!("non-UTF8 path {path:?}"))?;
  let entry_dir = path
    .parent()
    .and_then(|p| p.to_str())
    .ok_or_else(|| format!("no parent dir for {entry_path}"))?;
  let base_name = path
    .file_stem()
    .and_then(|s| s.to_str())
    .ok_or_else(|| format!("no file stem for {entry_path}"))?;
  let entry_cp_dir = format!("{entry_dir}/{base_name}");
  if let Err(reason) = fs::create_dir_all(&entry_cp_dir) {
    println!("Failed to mkdir -p {entry_cp_dir:?}: {:?}", reason.kind());
  }
  let full_extract_path = format!("{entry_cp_dir}/{base_name}.zip");

  // Decompress fully (arXiv per-paper source archives are small).
  let decompressed = {
    let file = File::open(entry_path).map_err(|e| format!("open {entry_path}: {e}"))?;
    let mut decoder = flate2::read::GzDecoder::new(file);
    let mut buf = Vec::new();
    decoder
      .read_to_end(&mut buf)
      .map_err(|e| format!("gunzip {entry_path}: {e}"))?;
    buf
  };

  // Reject mislabeled non-source content *before* writing anything (no stray empty zip left
  // behind).
  let detected = infer::get(&decompressed).map(|t| t.extension());
  if let Some(other) = detected {
    if other != "tar" {
      return Err(format!(
        "decompressed content is `{other}`, not a TeX source — rejected"
      ));
    }
  }

  let out =
    File::create(&full_extract_path).map_err(|e| format!("create {full_extract_path}: {e}"))?;
  let mut zw = zip::ZipWriter::new(out);
  let opts: zip::write::FileOptions<()> =
    zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
  if detected == Some("tar") {
    let mut tar = tar::Archive::new(std::io::Cursor::new(decompressed.as_slice()));
    for tar_entry in tar.entries().map_err(|e| format!("tar entries: {e}"))? {
      let mut tar_entry = match tar_entry {
        Ok(entry) => entry,
        Err(e) => {
          eprintln!("-- import: skipping unreadable tar entry in {entry_path}: {e}");
          continue;
        },
      };
      if !tar_entry.header().entry_type().is_file() {
        continue;
      }
      let name = match tar_entry.path() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => continue,
      };
      let mut data = Vec::new();
      if let Err(e) = tar_entry.read_to_end(&mut data) {
        eprintln!("-- import: reading tar entry {name} failed: {e}");
        continue;
      }
      zw.start_file(&name, opts)
        .map_err(|e| format!("zip start_file {name}: {e}"))?;
      zw.write_all(&data)
        .map_err(|e| format!("zip write {name}: {e}"))?;
    }
  } else {
    // Headerless = the arXiv "surprise": a plain gzipped TeX file.
    let tex_target = format!("{base_name}.tex");
    zw.start_file(&tex_target, opts)
      .map_err(|e| format!("zip start_file {tex_target}: {e}"))?;
    zw.write_all(&decompressed)
      .map_err(|e| format!("zip write {tex_target}: {e}"))?;
  }
  zw.finish()
    .map_err(|e| format!("finalize zip {full_extract_path}: {e}"))?;

  if let Err(e) = fs::remove_file(path) {
    println!("Can't remove source .gz: {e:?}");
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn write_gz(path: &Path, content: &[u8]) {
    let mut enc =
      flate2::write::GzEncoder::new(File::create(path).unwrap(), flate2::Compression::default());
    enc.write_all(content).unwrap();
    enc.finish().unwrap();
  }

  fn zip_names(zip_path: &Path) -> Vec<String> {
    let mut archive = zip::ZipArchive::new(File::open(zip_path).unwrap()).unwrap();
    (0..archive.len())
      .map(|i| archive.by_index(i).unwrap().name().to_string())
      .collect()
  }

  #[test]
  fn unpack_one_gz_handles_targz_plaintex_and_rejects_wrong_content() {
    let prefix_dir =
      std::env::temp_dir().join(format!("cortex_importer_test_{}/0001", std::process::id()));
    fs::create_dir_all(&prefix_dir).unwrap();

    // (a) a tar.gz with two source files -> a .zip carrying both.
    let tar_bytes = {
      let mut builder = tar::Builder::new(Vec::new());
      for (name, body) in [
        ("paper.tex", b"\\documentclass{article}".as_ref()),
        ("fig.eps", b"%!PS-Adobe".as_ref()),
      ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, name, body).unwrap();
      }
      builder.into_inner().unwrap()
    };
    let targz = prefix_dir.join("paperA.gz");
    write_gz(&targz, &tar_bytes);
    unpack_one_gz(&targz).expect("a tar.gz unpacks");
    let names = zip_names(&prefix_dir.join("paperA/paperA.zip"));
    assert!(
      names.contains(&"paper.tex".to_string()) && names.contains(&"fig.eps".to_string()),
      "tar.gz -> zip with both files, got {names:?}"
    );
    assert!(!targz.exists(), "the source .gz is removed");

    // (b) a plain gzipped .tex (the arXiv "surprise") -> a .zip carrying <base>.tex.
    let plaintex = prefix_dir.join("paperB.gz");
    write_gz(
      &plaintex,
      b"\\documentclass{article}\\begin{document}hi\\end{document}",
    );
    unpack_one_gz(&plaintex).expect("a plain gzipped tex unpacks");
    assert_eq!(
      zip_names(&prefix_dir.join("paperB/paperB.zip")),
      vec!["paperB.tex".to_string()],
      "a plain gz -> <base>.tex"
    );

    // (c) a gzipped PDF (wrong content) is rejected; the .gz is kept for inspection.
    let pdfgz = prefix_dir.join("paperC.gz");
    write_gz(&pdfgz, b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\nrest of a pdf");
    assert!(unpack_one_gz(&pdfgz).is_err(), "a gzipped PDF is rejected");
    assert!(pdfgz.exists(), "the rejected .gz is kept");

    fs::remove_dir_all(
      std::env::temp_dir().join(format!("cortex_importer_test_{}", std::process::id())),
    )
    .ok();
  }
}
