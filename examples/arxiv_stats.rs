// Copyright 2015-2018 Deyan Ginev. See the LICENSE
// file at the top-level directory of this distribution.
//
// Licensed under the MIT license <LICENSE-MIT or http://opensource.org/licenses/MIT>.
// This file may not be copied, modified, or distributed
// except according to those terms.
extern crate gnuplot;
extern crate serde;
extern crate serde_json;

use serde::ser::Serialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::fs::File;
use std::hash::Hash;
use std::io::Write;
use std::path::Path;
// use std::io::Result;
use gnuplot::*;
use std::fs::DirEntry;
use std::u64;

// arXiv months in order
// TODO: Only current as of 1505, you'll have to extend manually next time this is run
static ORDERED_MONTHS: [&'static str; 304] = [
  "9107", "9108", "9109", "9110", "9111", "9112", "9201", "9202", "9203", "9204", "9205", "9206",
  "9207", "9208", "9209", "9210", "9211", "9212", "9301", "9302", "9303", "9304", "9305", "9306",
  "9307", "9308", "9309", "9310", "9311", "9312", "9401", "9402", "9403", "9404", "9405", "9406",
  "9407", "9408", "9409", "9410", "9411", "9412", "9501", "9502", "9503", "9504", "9505", "9506",
  "9507", "9508", "9509", "9510", "9511", "9512", "9601", "9602", "9603", "9604", "9605", "9606",
  "9607", "9608", "9609", "9610", "9611", "9612", "9701", "9702", "9703", "9704", "9705", "9706",
  "9707", "9708", "9709", "9710", "9711", "9712", "9801", "9802", "9803", "9804", "9805", "9806",
  "9807", "9808", "9809", "9810", "9811", "9812", "9901", "9902", "9903", "9904", "9905", "9906",
  "9907", "9908", "9909", "9910", "9911", "9912", "0001", "0002", "0003", "0004", "0005", "0006",
  "0007", "0008", "0009", "0010", "0011", "0012", "0101", "0102", "0103", "0104", "0105", "0106",
  "0107", "0108", "0109", "0110", "0111", "0112", "0201", "0202", "0203", "0204", "0205", "0206",
  "0207", "0208", "0209", "0210", "0211", "0212", "0301", "0302", "0303", "0304", "0305", "0306",
  "0307", "0308", "0309", "0310", "0311", "0312", "0401", "0402", "0403", "0404", "0405", "0406",
  "0407", "0408", "0409", "0410", "0411", "0412", "0501", "0502", "0503", "0504", "0505", "0506",
  "0507", "0508", "0509", "0510", "0511", "0512", "0601", "0602", "0603", "0604", "0605", "0606",
  "0607", "0608", "0609", "0610", "0611", "0612", "0701", "0702", "0703", "0704", "0705", "0706",
  "0707", "0708", "0709", "0710", "0711", "0712", "0801", "0802", "0803", "0804", "0805", "0806",
  "0807", "0808", "0809", "0810", "0811", "0812", "0901", "0902", "0903", "0904", "0905", "0906",
  "0907", "0908", "0909", "0910", "0911", "0912", "1001", "1002", "1003", "1004", "1005", "1006",
  "1007", "1008", "1009", "1010", "1011", "1012", "1101", "1102", "1103", "1104", "1105", "1106",
  "1107", "1108", "1109", "1110", "1111", "1112", "1201", "1202", "1203", "1204", "1205", "1206",
  "1207", "1208", "1209", "1210", "1211", "1212", "1301", "1302", "1303", "1304", "1305", "1306",
  "1307", "1308", "1309", "1310", "1311", "1312", "1401", "1402", "1403", "1404", "1405", "1406",
  "1407", "1408", "1409", "1410", "1411", "1412", "1501", "1502", "1503", "1504", "1505", "1506",
  "1507", "1508", "1509", "1510", "1511", "1512", "1601", "1602", "1603", "1604", "1605", "1606",
  "1607", "1608", "1609", "1610",
];

fn get_dir_size(direntry: &DirEntry) -> u64 {
  // Initialize with the size of the current directory
  let mut size = direntry.metadata().unwrap().len();
  // Sum up the current file children
  for subfile in &get_subfiles(direntry) {
    let subfile_size = subfile.metadata().unwrap().len();
    size += subfile_size;
  }
  // And recurse into subdirectories
  for subdir in &get_subdirs(direntry) {
    size += get_dir_size(subdir);
  }
  size
}

fn is_dir(direntry: &DirEntry) -> bool {
  let metadata = direntry.metadata();
  match metadata {
    Err(_) => false,
    Ok(metadata) => metadata.is_dir(),
  }
}

fn get_subdirs(direntry: &DirEntry) -> Vec<DirEntry> { get_path_subdirs(direntry.path().as_path()) }
fn get_subfiles(direntry: &DirEntry) -> Vec<DirEntry> {
  get_path_subfiles(direntry.path().as_path())
}

fn get_path_subdirs(path: &Path) -> Vec<DirEntry> {
  let children = fs::read_dir(path).unwrap();
  let children_entries = children.map(|c| c.unwrap()).collect::<Vec<_>>();
  children_entries
    .into_iter()
    .filter(|c| is_dir(c))
    .collect::<Vec<_>>()
}
fn get_path_subfiles(path: &Path) -> Vec<DirEntry> {
  let children = fs::read_dir(path).unwrap();
  let children_entries = children.map(|c| c.unwrap()).collect::<Vec<_>>();
  children_entries
    .into_iter()
    .filter(|c| !is_dir(c))
    .collect::<Vec<_>>()
}

fn write_stats<K: Hash + Eq + Serialize, V: Serialize>(
  name: &'static str,
  counts: &HashMap<K, V>,
) -> std::io::Result<()>
{
  let mut f = try!(File::create(name));
  try!(f.write_all(serde_json::to_string(&counts).unwrap().as_bytes()));
  Ok(())
}

fn main() {
  let args: Vec<_> = env::args().collect();
  let mut min_size = u64::MAX;
  let mut max_size = 0;
  let mut max_path: String = "".to_string();
  let mut min_path: String = "".to_string();

  let arxiv_root = &Path::new(&args[1]);
  let mut arxiv_size_frequencies: HashMap<u64, u64> = HashMap::new();
  let mut arxiv_monthly_sizes: HashMap<String, u64> = HashMap::new();
  let mut arxiv_counts: HashMap<String, u64> = HashMap::new();
  let arxiv_month_dirs = get_path_subdirs(arxiv_root);

  for month_dir in &arxiv_month_dirs {
    println!("-- Measuring {:?}", month_dir.file_name().to_str().unwrap());
    let mut monthly_size = 0;
    let month_papers = get_subdirs(month_dir);
    // Record how many papers were submitted in that month
    arxiv_counts.insert(
      month_dir.file_name().to_str().unwrap().to_string(),
      month_papers.len() as u64,
    );

    for paper_dir in &month_papers {
      // Record the size of each paper directory
      let paper_size = get_dir_size(paper_dir) / 1024; // In KB
      if paper_size > max_size {
        max_size = paper_size;
        max_path = paper_dir.path().to_str().unwrap().to_string();
      }
      if paper_size < min_size {
        min_size = paper_size;
        min_path = paper_dir.path().to_str().unwrap().to_string();
      }
      monthly_size += paper_size;

      let size_frequency = arxiv_size_frequencies.entry(paper_size).or_insert(0);
      *size_frequency += 1;
    }
    arxiv_monthly_sizes.insert(
      month_dir.file_name().to_str().unwrap().to_string(),
      monthly_size,
    );
  }

  // Print the recorded stats
  match write_stats("arxiv_submission_counts.json", &arxiv_counts) {
    Ok(_) => {},
    Err(e) => println!("{:?}", e),
  };

  match write_stats("arxiv_monthly_sizes.json", &arxiv_monthly_sizes) {
    Ok(_) => {},
    Err(e) => println!("{:?}", e),
  };

  match write_stats("arxiv_size_frequencies.json", &arxiv_size_frequencies) {
    Ok(_) => {},
    Err(e) => println!("{:?}", e),
  };

  let mut f4 = File::create("arxiv_stats.txt").unwrap();
  f4.write_all(b"Min size: \n").unwrap();
  f4.write_all(min_size.to_string().as_bytes()).unwrap();
  f4.write_all(b"\nMin path: \n").unwrap();
  f4.write_all(min_path.as_bytes()).unwrap();
  f4.write_all(b"\nMax size: \n").unwrap();
  f4.write_all(max_size.to_string().as_bytes()).unwrap();
  f4.write_all(b"\nMax path: \n").unwrap();
  f4.write_all(max_path.as_bytes()).unwrap();

  // Plot of paper counts by month:
  let zero = 0 as u64;
  let ordered_counts = ORDERED_MONTHS
    .iter()
    .map(|m| match arxiv_counts.get(&m.to_string()) {
      Some(counts) => counts,
      None => &zero,
    });
  let ordered_month_xcoords = 1..ORDERED_MONTHS.len() + 1;

  let mut fg = Figure::new();
  fg.axes2d()
    .points(
      ordered_month_xcoords.clone(),
      ordered_counts,
      &[PointSymbol('D'), Color("#ffaa77"), PointSize(0.5)],
    )
    .set_x_label("arXiv month", &[Rotate(45.0)])
    .set_y_label("Submitted papers", &[Rotate(90.0)])
    .set_title("arXiv TeX submission counts", &[]);

  fg.set_terminal("pngcairo", "arxiv_submission_counts.png");
  fg.show();

  // Plot of submission counts by month:
  let ordered_sizes = ORDERED_MONTHS.iter().map(|m| {
    match arxiv_monthly_sizes.get(&m.to_string()) {
      Some(&counts) => counts,
      None => zero,
    }
  });
  fg = Figure::new();
  fg.axes2d()
    .points(
      ordered_month_xcoords.clone(),
      ordered_sizes,
      &[PointSymbol('D'), Color("#ffaa77"), PointSize(0.5)],
    )
    .set_x_label("arXiv month", &[Rotate(45.0)])
    .set_y_label("Submission size in MB", &[Rotate(90.0)])
    .set_title("arXiv TeX submission sizes by month", &[]);

  fg.set_terminal("pngcairo", "arxiv_submission_sizes.png");
  fg.show();

  // Plot average paper size in KB
  // Plot of submission size by month:

  let freq_keys = arxiv_size_frequencies
    .clone()
    .into_iter()
    .map(|entry| entry.0);
  let freq_values = arxiv_size_frequencies
    .clone()
    .into_iter()
    .map(|entry| entry.1);

  fg = Figure::new();
  fg.axes2d()
    .points(
      freq_keys,
      freq_values,
      &[PointSymbol('D'), Color("#ffaa77"), PointSize(0.5)],
    )
    .set_x_label("Paper size in KB", &[Rotate(45.0)])
    .set_y_label("Paper count", &[Rotate(90.0)])
    .set_title("arXiv TeX paper sizes", &[]);

  fg.set_terminal("pngcairo", "arxiv_paper_sizes.png");
  fg.show();
}
