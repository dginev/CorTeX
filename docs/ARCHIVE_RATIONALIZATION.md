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

## Candidate crates (docs.rs `libarchive`, by downloads)

Surveyed per the owner's pointer. The **2018 crates are dead** (`libarchive` 0.1.1, `libarchive-sys`
0.0.2, `libarchive3-sys` 0.1.2 — all Jun 2018; our fork descends from these). The **live** options:

| Crate | Kind | Read | Write | Auto-detect | Streaming | C dep | Freshness |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **`compress-tools` 0.16.1** | high-level libarchive wrapper | ✅ | ❌ (extract/list only) | ✅ built-in | ✅ `ArchiveIterator` | libarchive ≥3.2 | Apr 2026, widely used |
| **`libarchive2` (+ `-sys`) 0.2** | safe libarchive bindings (v3.8.1) | ✅ | ✅ | via libarchive | ✅ | libarchive | days old, 1 maintainer |
| `akv` / `libarchive_src` 0.1 | safe bindings, bundles libarchive src | ✅ | ? | via libarchive | ? | bundled libarchive | new (May 2026) |
| `simple-archive` 0.4, `archive-reader` 0.4 | slim libarchive read wrappers | ✅ | ❌ | ✅ | ✅ | libarchive | 2026, niche |
| **`flate2` + `tar` + `zip`** | pure-Rust per-format | ✅ | ✅ | hand-rolled sniff | ✅ `Read`/`Write` | **none** (miniz_oxide) | first-party, active |

This yields **two real paths** (both retire the personal fork — the owner's core complaint):

- **Path A — pure-Rust** (`flate2` + `tar` + `zip` + a magic-byte sniffer): drops the C dependency
  entirely (consistent with the libzmq → `zeromq` move).
- **Path B — a *maintained* libarchive wrapper** (`compress-tools` for read/detect + `zip` for write):
  keeps libarchive's universality, full speed, and **built-in** content auto-detection, with the
  smallest migration — but keeps the libarchive C build dep. (`compress-tools` is read-only, so the
  importer's `.zip` *output* still uses the pure-Rust `zip` crate; `libarchive2` could do both sides in
  one binding but is days-old and single-maintainer — riskier than `compress-tools` + `zip`.)

## Path A — the pure-Rust stack

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

| Axis | libarchive-sys fork (today) | **A:** flate2 + tar + zip | **B:** compress-tools + zip |
| --- | --- | --- | --- |
| **Maintenance** | personal fork, bus-factor 1 | first-party Rust crates, active | `compress-tools` widely used + active; `zip` active |
| **C dependency** | yes | **none** (miniz_oxide; opt-in `zlib-ng`) | yes (libarchive ≥3.2 — read side) |
| **Formats** | all (universality) | the four we need — explicitly | all (universality) for read; `zip` writes |
| **Streaming / bounded memory** | yes | yes (`Read`/`Write`) | yes (`ArchiveIterator`) |
| **Auto-detection** | built-in | magic-byte sniff (below) — ~10 lines | **built-in** (libarchive) |
| **Write archives** | yes | `zip` crate (pure Rust) | `zip` crate (compress-tools is read-only) |
| **Efficiency (gzip decompress)** | 1467 MB/s | 1314 MB/s (0.90×) | full libarchive (≈ 1467 MB/s) |
| **Migration effort** | — | rewrite unpack with 3 crates + sniffer | swap reads to a high-level API; keep `zip` write |
| **Retires the personal fork** | — | ✅ | ✅ |

**The decision lever:** *both* paths retire the personal fork (the owner's core complaint). They differ
on **one axis — keep the libarchive C engine or not:**

- **Path B (`compress-tools` + `zip`)** is the most *directly responsive* to this ask's stated wants —
  *better-maintained* (✓ via a popular crate), *flexible generality* (✓ libarchive), *high efficiency*
  (✓ full C speed), *built-in auto-detection* (✓) — at the **least migration risk** (a high-level
  read/extract API with detection already inside). Cost: keeps the libarchive C build dep.
- **Path A (pure-Rust)** additionally **drops the C dependency** (consistent with the libzmq →
  `zeromq` move) and covers our exact formats at ≈ parity speed — at the cost of a hand-rolled sniffer
  and rewriting the unpack logic across three crates.

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

*(This is **Path A**'s detection; **Path B** inherits the same auto-detection from libarchive for
free.)* arXiv filenames are unreliable (a `.gz` may be plain TeX, or raw PDF) — so detection must be by
**content (magic bytes)**. A ~10-line sniffer covers everything we touch, and crucially **rejects
wrong/corrupt content** instead of feeding it to a decompressor:

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

**Retire the personal `libarchive-sys` fork — both paths do that; pick the lever.** My default, given
*this* ask's stated wants (better-maintained + generality + efficiency + auto-detection, all libarchive
strengths) and the lowest migration risk:

> **Default: Path B — `compress-tools` (read + built-in auto-detection + streaming) + `zip` (write).**
> It replaces the fork with a popular, maintained crate, keeps libarchive's universality + full speed +
> content detection for free, and only the `.zip` *output* moves to the pure-Rust `zip` crate. Smallest,
> safest change.

> **Choose Path A — `flate2` + `tar` + `zip` + a magic-byte sniffer — if dropping the libarchive C
> dependency is itself a goal** (consistency with the libzmq → `zeromq` removal). It's ≈ parity on the
> hot decompress path, covers our exact formats, and streams natively — at the cost of a hand-rolled
> sniffer and rewriting the unpack logic across three crates.

The decisive question is therefore **just: do we want to be free of the libarchive C dependency?** If
yes → A. If "a maintained crate is enough, keep the proven engine" → B. Either way the surface is **only
`importer.rs` + `helpers.rs`**, behind `tests/importer_test.rs` + a new detection test, and **pairs with
the I-1 importer hardening** (replace the unpack `.unwrap()`s with detect → stream → log-and-skip).
Independent of the dispatcher transport work — can land before, after, or in parallel.

## Open questions

1. **The lever — keep the libarchive C engine (Path B, `compress-tools` + `zip`) or go fully pure-Rust
   (Path A, `flate2` + `tar` + `zip`)?** This is the one real decision; everything else follows from it.
   My lean: **B** for least risk unless C-dependency removal is a stated goal (then **A**).
2. *(Path A only)* **Backend `miniz_oxide` (zero C) vs `zlib-ng` (C, faster)?** Start pure-Rust — the
   codec isn't the bottleneck (disk is); flip the flag only if a measured need appears.
3. *(Path A only)* **Detection: hand-rolled magic-byte table (no dep, we own the policy) vs the `infer`
   crate?** Lean hand-rolled. *(Path B gets detection from libarchive for free.)*
4. **Combine with the I-1 importer hardening** in one pass (same files, same review)? Recommend yes.
