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

fn env_usize(key: &str, default: usize) -> usize {
  std::env::var(key)
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(default)
}

/// Content-based type detection **delegated to the `infer` crate** (owner preference over a
/// hand-rolled magic-byte table — compression/detection is error-prone, so use a maintained crate).
/// Returns the detected archive extension, a reject label for known non-source content (e.g. a
/// `.gz` that is really a PDF), or the raw/text fallback (`infer` returns `None` for headerless
/// TeX).
fn detect_format(b: &[u8]) -> String {
  match infer::get(b) {
    Some(t) if matches!(t.extension(), "gz" | "tar" | "zip" | "bz2" | "xz" | "zst") => {
      t.extension().to_string()
    },
    Some(t) if t.extension() == "pdf" => "pdf — REJECT (not a source archive)".into(),
    Some(t) => format!("{} — REJECT (unexpected type)", t.extension()),
    None => "raw/text — single-file fallback".into(),
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

  // --- PER-TASK HOT PATH: open a result .zip and extract cortex.log (scanned for message lines) --
  // The real per-task op (helpers.rs): every returned result .zip is opened to read cortex.log. A
  // result holds a large converted output + a small log; the log is what we want. ZIP has a central
  // directory, so the `zip` crate can seek straight to cortex.log; libarchive streams sequentially
  // and must read past the large entry. cortex.log is placed LAST (the costly case for a scanner).
  let html_bytes = env_usize("HTML_MB", 4) * 1024 * 1024;
  let html = {
    let mut h = Vec::with_capacity(html_bytes);
    let mut s = 0x55u64;
    while h.len() < html_bytes {
      h.extend_from_slice(b"<p>converted output paragraph with some text</p>");
      for _ in 0..16 {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
        h.push((s >> 33) as u8);
      }
    }
    h
  };
  let log = "Info\tcortex\tlog line with a message and some detail text\n".repeat(160); // ~8 KB
  let result_zip = dir.path().join("result.zip");
  {
    let mut zw = zip::ZipWriter::new(std::fs::File::create(&result_zip).unwrap());
    let opts: zip::write::FileOptions<()> =
      zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zw.start_file("html/index.html", opts).unwrap();
    zw.write_all(&html).unwrap();
    zw.start_file("cortex.log", opts).unwrap(); // last entry
    zw.write_all(log.as_bytes()).unwrap();
    zw.finish().unwrap();
  }
  let scan_iters = 500;

  // zip crate — random-access by_name (skips the 4 MB html via the central directory).
  let start = Instant::now();
  for _ in 0..scan_iters {
    let mut za = zip::ZipArchive::new(std::fs::File::open(&result_zip).unwrap()).unwrap();
    let mut f = za.by_name("cortex.log").unwrap();
    let mut s = String::new();
    f.read_to_string(&mut s).unwrap();
    std::hint::black_box(s.lines().count());
  }
  let zip_secs = start.elapsed().as_secs_f64();

  // libarchive — sequential scan to cortex.log (reads through the html entry).
  let start = Instant::now();
  for _ in 0..scan_iters {
    let reader = Reader::new()
      .unwrap()
      .support_filter_all()
      .support_format_all()
      .open_filename(result_zip.to_str().unwrap(), BUFFER_SIZE)
      .unwrap();
    while let Ok(e) = reader.next_header() {
      if e.pathname() == "cortex.log" {
        let mut content = Vec::new();
        while let Ok(chunk) = reader.read_data(BUFFER_SIZE) {
          if chunk.is_empty() {
            break;
          }
          content.extend_from_slice(&chunk);
        }
        std::hint::black_box(content.len());
        break;
      }
    }
  }
  let la_secs = start.elapsed().as_secs_f64();

  println!(
    "  PER-TASK hot path — open result.zip + extract cortex.log (output {} MB, log last):",
    html_bytes / 1_048_576
  );
  println!(
    "    zip crate (by_name random access): {:.0} opens/s ({:.0} µs/op)",
    scan_iters as f64 / zip_secs,
    zip_secs / scan_iters as f64 * 1e6
  );
  println!(
    "    libarchive (sequential scan):      {:.0} opens/s ({:.0} µs/op)",
    scan_iters as f64 / la_secs,
    la_secs / scan_iters as f64 * 1e6
  );
  println!(
    "    → zip crate is {:.1}x libarchive on this op",
    la_secs / zip_secs.max(1e-9)
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
