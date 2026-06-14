# Archive-library rationalization — pure-Rust, streaming, auto-detecting

Owner directive (2026-06-14): *"rationalize our use of the libarchive crate. There may be better
maintained alternatives in the Rust ecosystem. We have .tar.gz, .gz and .zip inputs/outputs, so we want
flexible generality in archive handling. And extremely high efficiency, this is a hot path."* Plus:
*"auto-detection of which archive format is in use — we do not always have a filename that accurately
tells us that (and some files are corrupted or wrong, e.g. raw PDF content)."*

## Current state

CorTeX depends on **`libarchive-sys`** — a **self-maintained C-FFI fork** (`git =
"https://github.com/dginev/libarchive-sys.git"`), wrapping the system `libarchive` C library (a
`libarchive-dev` build dependency). It is used in exactly **two files**:

- **`src/importer.rs`** — reads `.tar` (top-level arXiv dumps) and `.gz` (monthly sub-archives,
  including the *plain-text-mislabeled-as-gz* "surprise"), and **writes `.zip`** (one per entry).
- **`src/helpers.rs`** — reads `.zip` result archives entry-by-entry to parse `cortex.log` (already
  scope-localized to `.drop` the reader ASAP).

The dispatcher sink does **not** use libarchive — it writes the worker's result bytes straight to a
`.zip` on `/data` (a raw byte write). So the codec surface is small and import-side.

**Formats we actually need:** `.gz`, `.tar`, `.tar.gz`, `.zip` (read + write). libarchive's universality
(dozens of formats) is unused; its one feature we *do* lean on is **content-based auto-detection**
(`support_filter_all` + `support_format_all`).

## The pure-Rust stack

| Format | Crate | Notes |
| --- | --- | --- |
| `.gz` (gzip filter) | **`flate2`** | de-facto standard; default backend **miniz_oxide = pure Rust, zero C**; optional `zlib-ng` backend for C-speed |
| `.tar` | **`tar`** | first-party (alexcrichton); streaming `Archive`/`Builder` over `Read`/`Write` |
| `.tar.gz` | `flate2::GzDecoder` → `tar::Archive` | compose the two — idiomatic, streaming end-to-end |
| `.zip` (read + write) | **`zip`** | actively maintained; deflate/stored/bzip2/zstd; `ZipArchive`/`ZipWriter` |
| async streaming (optional) | **`async-compression`** | `tokio` `AsyncRead`/`AsyncWrite` gzip/zstd — fits the async dispatcher core if archive codec ever moves onto it |

All are **streaming-native** (`Read`/`Write` traits): process a fixed buffer at a time, never a whole
archive resident — which is exactly what the [memory-discipline audit](DISPATCHER_RATIONALIZATION.md)
requires.

## Evaluation

| Axis | libarchive-sys (today) | flate2 + tar + zip |
| --- | --- | --- |
| **Maintenance** | a **personal fork** of a C binding — owner-maintained, bus-factor 1 | first-party / widely-used Rust crates, active |
| **C dependency** | yes (`libarchive` + the fork's FFI) | **none** by default (miniz_oxide pure-Rust); opt-in `zlib-ng` if wanted |
| **Formats we need** | all (via universality) | all four — explicitly |
| **Streaming / bounded memory** | yes (`read_data` loop) | yes (`Read`/`Write`) — and integrates with the chunked-streaming design |
| **Async** | no | `async-compression` (tokio) available |
| **Auto-detection** | built-in (`support_*_all`) | magic-byte sniff (below) — small, explicit, controllable |
| **Efficiency (gzip decompress, hot op)** | 1467 MB/s | **1314 MB/s (0.90×)** — near parity (see below) |

**Key point:** the owner's two concerns — *better maintained* and *escape the C dependency* (the same
motivation as the libzmq → `zeromq` move) — both point at the pure-Rust stack, which also covers our
exact formats and streams natively.

## Efficiency (empirical, `examples/archive_bench.rs`)

8 MB representative arXiv source (TeX + incompressible figure bytes ⇒ ~2.9× compression), release build:

| Operation | Throughput |
| --- | --- |
| gzip **decompress** — flate2 (pure-Rust) | **1314 MB/s** |
| gzip **decompress** — libarchive (C FFI) | 1467 MB/s |
| → ratio | **flate2 = 0.90× libarchive** (≈ parity) |
| gzip **compress** — flate2 (default level) | 62 MB/s |
| **zip build** — zip crate (deflate) | 63 MB/s |

On realistic mixed data, pure-Rust gzip decompress is **within 10 % of libarchive** — and both are
~1.3–1.5 GB/s, **far above the `/data` disk read** that actually bounds bulk import, so the codec is
*not* the import bottleneck. (On hyper-compressible pure text the gap widens to ~0.56× — an artifact of
miniz_oxide's inflate on highly-redundant input; irrelevant for real sources.) **If maximal codec
throughput is ever wanted, flate2's `zlib-ng` backend reaches C-zlib speed** — a perf/purity knob, not
a redesign. Compress at 62 MB/s is the default level 6; a lower level trades ratio for speed if import
write-back ever needs it.

## Content-based format auto-detection (filenames lie)

arXiv filenames are unreliable (a `.gz` may be plain TeX, or raw PDF) — so detection must be by
**content (magic bytes)**, replicating libarchive's `support_filter_all`/`support_format_all`. A
~10-line sniffer covers everything we touch, and crucially **rejects wrong/corrupt content** instead of
feeding it to a decompressor:

| Magic (prefix / offset) | Format |
| --- | --- |
| `1f 8b` | gzip |
| `50 4b 03 04` (`PK..`) | zip |
| `ustar` at offset 257 | tar |
| `42 5a 68` (`BZh`) | bzip2 |
| `fd 37 7a 58 5a 00` | xz |
| `28 b5 2f fd` | zstd |
| `25 50 44 46` (`%PDF`) | **reject — not a source archive** |
| *(no match)* | **raw/text — single-file fallback** (the arXiv "surprise") |

Validated in the spike — real gz/zip/tar classify correctly, a `.gz`-that-is-really-a-PDF is rejected,
and a `.gz`-that-is-really-plain-TeX falls back to raw. (The **`infer`** crate provides the same
off-the-shelf if we'd rather not hand-roll + maintain the table; for ~6 formats hand-rolling is
arguably leaner.) This also closes part of **I-1** (importer fault-tolerance): detect-then-dispatch
means a corrupt/mislabeled entry is logged + skipped, never a panic or garbage decode.

## Recommendation

**Adopt `flate2` + `tar` + `zip` (+ a magic-byte sniffer), retiring `libarchive-sys`.** It is better
maintained, removes a C dependency (complementing the libzmq → `zeromq` move), covers our exact
formats, streams natively (serving the memory-discipline design), auto-detects by content (handling
mislabeled/corrupt input), and is within 10 % of libarchive on the hot decompress path (with a
`zlib-ng` escape hatch for C-speed). The migration surface is **just `importer.rs` + `helpers.rs`** and
can be done behind the existing importer tests (`tests/importer_test.rs`) plus a new detection test.

**Sequencing:** independent of the dispatcher transport work — can land before, after, or in parallel.
It pairs naturally with hardening the importer unpack path (I-1: replace the `.unwrap()`s on the
libarchive calls with detect → stream → log-and-skip-on-error), since both touch the same code.

## Open questions

1. **Backend: pure-Rust `miniz_oxide` (zero C, ~0.90× libarchive) or `zlib-ng` (C, ~parity-or-faster)?**
   Recommend starting pure-Rust (the codec isn't the bottleneck — disk is) and switching the feature
   flag only if a measured import-throughput need appears.
2. **Detection: hand-rolled magic-byte table (≈6 formats, no dep) or the `infer` crate?** Recommend
   hand-rolled — smaller, explicit, and we control the reject/fallback policy.
3. **Combine with the I-1 importer hardening** (unpack `.unwrap()`s → detect/stream/skip) in one pass?
   Recommend yes — same files, same review.
