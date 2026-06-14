// Copyright 2015-2025 Deyan Ginev. MIT license.
//
// Archive-library rationalization SPIKE (throwaway; docs/ARCHIVE_RATIONALIZATION.md). The owner
// wants to replace the self-maintained `libarchive-sys` C-FFI fork with a better-maintained Rust
// stack that covers our formats (.gz, .tar, .tar.gz, .zip) with flexible generality and **high
// efficiency on the hot path** (bulk arXiv import = decompress-heavy). This measures the pure-Rust
// stack vs. libarchive on the dominant operation — gzip decompress — plus pure-Rust compress +
// zip-build throughput, and confirms the Rust APIs stream (bounded memory) rather than buffering
// whole archives.
//
// Run:  cargo run --release --example archive_bench

use std::io::{Read, Write};
use std::time::Instant;

use Archive::*; // the current libarchive-sys binding

const BUFFER_SIZE: usize = 10_240;

/// ~8 MB representative of an arXiv source: compressible TeX text interleaved with incompressible
/// pseudo-random bytes (embedded figures/PDFs), tuned to a realistic ~3–4x overall compression.
fn sample_tex() -> Vec<u8> {
  let unit = b"\\documentclass{article}\\usepackage{amsmath}\\begin{document}\n\
    \\section{On the Asymptotics of $\\zeta(s)$}\nLet $x_n \\to \\infty$ as $n \\to \\infty$. Then \
    by Lemma~3.2 we have $\\sum_{k=1}^n \\frac{1}{k^2} = \\frac{\\pi^2}{6} - O(1/n)$, and the \
    remainder term is controlled by the integral $\\int_n^\\infty t^{-2}\\,dt$.\n\\end{document}\n";
  let mut data = Vec::with_capacity(8 * 1024 * 1024);
  let mut seed = 0x1234_5678_9abc_def0u64;
  while data.len() < 8 * 1024 * 1024 {
    data.extend_from_slice(unit); // compressible text
                                  // ~half as many incompressible bytes (a "figure"), so the overall ratio lands around 3–4x.
    for _ in 0..unit.len() / 2 {
      seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
      data.push((seed >> 33) as u8);
    }
  }
  data
}

fn mb_per_s(bytes: usize, iters: usize, secs: f64) -> f64 {
  (bytes * iters) as f64 / secs.max(1e-9) / 1_048_576.0
}

/// Content-based archive-format detection by **magic bytes** — replicating libarchive's
/// `support_filter_all`/`support_format_all`, because arXiv filenames lie: a `.gz` may be plain
/// TeX, or even a raw PDF. Returns the format, `raw/text` when no archive magic matches (the
/// single-file fallback the importer already relies on), or a reject label for known non-source
/// content. (The `infer` crate does exactly this off-the-shelf if we'd rather not hand-roll the
/// table.)
fn detect_format(b: &[u8]) -> &'static str {
  if b.len() >= 2 && b[0] == 0x1f && b[1] == 0x8b {
    "gzip"
  } else if b.len() >= 4 && &b[0..4] == b"PK\x03\x04" {
    "zip"
  } else if b.len() >= 3 && &b[0..3] == b"BZh" {
    "bzip2"
  } else if b.len() >= 6 && &b[0..6] == b"\xfd7zXZ\x00" {
    "xz"
  } else if b.len() >= 4 && b[0..4] == [0x28, 0xb5, 0x2f, 0xfd] {
    "zstd"
  } else if b.len() >= 262 && &b[257..262] == b"ustar" {
    "tar"
  } else if b.len() >= 4 && &b[0..4] == b"%PDF" {
    "pdf — REJECT (not a source archive)"
  } else {
    "raw/text — single-file fallback"
  }
}

fn main() {
  let data = sample_tex();
  let iters = 20;
  println!(
    "archive_bench: {:.1} MB sample, {iters} iters/op",
    data.len() as f64 / 1_048_576.0
  );

  // Prepare a real .gz file on disk (both readers open a path, the real usage pattern).
  let dir = tempfile::tempdir().expect("tempdir");
  let gz_path = dir.path().join("sample.gz");
  {
    let mut enc = flate2::write::GzEncoder::new(
      std::fs::File::create(&gz_path).unwrap(),
      flate2::Compression::default(),
    );
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
  }
  let gz_size = std::fs::metadata(&gz_path).unwrap().len();
  println!(
    "  gz on disk: {:.2} MB ({:.1}x compression)",
    gz_size as f64 / 1_048_576.0,
    data.len() as f64 / gz_size as f64
  );

  // --- gzip DECOMPRESS A/B (the import hot path) -----------------------------------------------
  // flate2 (pure-Rust miniz_oxide), streaming through a fixed buffer.
  let start = Instant::now();
  let mut flate_out = 0usize;
  for _ in 0..iters {
    let mut dec = flate2::read::GzDecoder::new(std::fs::File::open(&gz_path).unwrap());
    let mut buf = vec![0u8; BUFFER_SIZE];
    loop {
      let n = dec.read(&mut buf).unwrap();
      if n == 0 {
        break;
      }
      flate_out += n;
    }
  }
  let flate_secs = start.elapsed().as_secs_f64();

  // libarchive-sys (C FFI), raw single-stream gzip, streaming read_data.
  let start = Instant::now();
  let mut la_out = 0usize;
  for _ in 0..iters {
    let reader = Reader::new()
      .unwrap()
      .support_filter_all()
      .support_format_raw()
      .open_filename(gz_path.to_str().unwrap(), BUFFER_SIZE)
      .unwrap();
    while reader.next_header().is_ok() {
      while let Ok(chunk) = reader.read_data(BUFFER_SIZE) {
        if chunk.is_empty() {
          break;
        }
        la_out += chunk.len();
      }
    }
  }
  let la_secs = start.elapsed().as_secs_f64();

  println!("  gzip decompress (the import hot op):");
  println!(
    "    flate2 (pure-Rust): {:.0} MB/s   [{} MB out]",
    mb_per_s(data.len(), iters, flate_secs),
    flate_out / iters / 1_048_576
  );
  println!(
    "    libarchive  (C FFI): {:.0} MB/s   [{} MB out]",
    mb_per_s(data.len(), iters, la_secs),
    la_out / iters / 1_048_576
  );
  println!(
    "    → flate2 is {:.2}x libarchive",
    mb_per_s(data.len(), iters, flate_secs) / mb_per_s(data.len(), iters, la_secs).max(1e-9)
  );

  // --- pure-Rust gzip COMPRESS throughput ------------------------------------------------------
  let start = Instant::now();
  for _ in 0..iters {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&data).unwrap();
    enc.finish().unwrap();
  }
  println!(
    "  gzip compress (flate2, default level): {:.0} MB/s",
    mb_per_s(data.len(), iters, start.elapsed().as_secs_f64())
  );

  // --- pure-Rust ZIP build throughput (the importer's output format) ---------------------------
  let start = Instant::now();
  for _ in 0..iters {
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts: zip::write::FileOptions<()> =
      zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("paper.tex", opts).unwrap();
    zw.write_all(&data).unwrap();
    zw.finish().unwrap();
  }
  println!(
    "  zip build (zip crate, deflate): {:.0} MB/s",
    mb_per_s(data.len(), iters, start.elapsed().as_secs_f64())
  );

  // --- content-based format auto-detection (filenames lie; corrupt/wrong content happens) -------
  // Build real samples + mislabeled content, detect each from its bytes alone.
  let gz_prefix = {
    let mut f = std::fs::File::open(&gz_path).unwrap();
    let mut buf = vec![0u8; 512];
    let n = Read::read(&mut f, &mut buf).unwrap();
    buf.truncate(n);
    buf
  };
  let zip_bytes = {
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default();
    zw.start_file("a.tex", opts).unwrap();
    zw.write_all(b"hello").unwrap();
    zw.finish().unwrap().into_inner()
  };
  let tar_bytes = {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(5);
    h.set_cksum();
    b.append_data(&mut h, "a.tex", &b"hello"[..]).unwrap();
    b.into_inner().unwrap()
  };
  let pdf_bytes = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n1 0 obj".to_vec(); // a ".gz" that's really a PDF
  let text_bytes = b"\\documentclass{article}\\begin{document}plain TeX, not gzipped".to_vec();

  println!("  content-based format detection (magic bytes, not filename):");
  for (label, bytes) in [
    ("real .gz", &gz_prefix),
    ("real .zip", &zip_bytes),
    ("real .tar", &tar_bytes),
    (".gz that is really a PDF", &pdf_bytes),
    (".gz that is really plain TeX", &text_bytes),
  ] {
    println!("    {label:<28} → {}", detect_format(bytes));
  }

  println!(
    "  note: flate2/tar/zip are all streaming (Read/Write) — bounded {BUFFER_SIZE}-byte memory, never \
     a whole archive resident (cf. the memory-discipline audit)."
  );
}
