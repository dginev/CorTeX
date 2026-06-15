# Archive-library rationalization — pure-Rust, streaming, auto-detecting

> **STATUS: DONE (2026-06-14) — Path A shipped.** Owner chose pure-Rust ("the pure rust recommendation
> wins, go"). `libarchive-sys` (the personal C-FFI fork) is **removed**; archive handling is now
> `flate2` + `tar` + `zip` + `infer`, all pure-Rust + streaming. Migrated: `helpers::generate_report`
> (per-task result parse → `zip` `by_name`), `importer.rs` unpack (`unpack_top_tar` + `unpack_one_gz`,
> with I-1 hardening + `infer` detection), and the `record_loading_info` / `sandbox_arxiv` examples.
> The `libarchive-dev` system package is no longer a build dependency. Green on `echo_roundtrip`,
> `importer_test`, and the new unit tests. The evaluation below is retained as the rationale.

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
archive resident — which is exactly what the [memory-discipline audit](../DISPATCHER_RATIONALIZATION.md)
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

On realistic mixed data, pure-Rust gzip decompress is **within 10–15 % of libarchive** — and both are
~1.2–1.5 GB/s, **far above the `/data` disk read** that actually bounds bulk import, so the codec is
*not* the import bottleneck. (On hyper-compressible pure text the gap widens to ~0.56× — an artifact of
miniz_oxide's inflate on highly-redundant input; irrelevant for real sources.) **If maximal codec
throughput is ever wanted, flate2's `zlib-ng` backend reaches C-zlib speed** — a perf/purity knob, not
a redesign.

### The *critical* hot path: opening every result `.zip` for `cortex.log`

(Owner: *"every returned ZIP is opened as we scan cortex.log for message lines — this needs to be
maximally performant."*) This is the **per-task steady-state** op (`helpers.rs`), unlike the import (a
one-off): ~100–200×/s, every result archive opened to read its (small) `cortex.log` out of a (large)
converted output. ZIP carries a **central directory + per-entry sizes**, so the right primitive is
**random access by name**, not a sequential scan:

| Open result `.zip` + extract `cortex.log` (output 4 / 32 / 128 MB, log last) | per op |
| --- | --- |
| **`zip` crate — `by_name("cortex.log")`** (random access) | **~8 µs** |
| libarchive — sequential `next_header` scan | ~11 µs |

The `zip` crate is a **steady ~1.4×** faster, and *both* are **flat in the output size** (4 → 128 MB
identical) — ZIP's size headers let either library skip the output without decompressing it, so this is
a constant-factor win, not a scaling one. At ~8–11 µs/op and ≤200 tasks/s it is **~0.2 % of one core
either way** — *not* a throughput bottleneck — but the `zip` crate is faster **and** pure-Rust **and**
maintained, and `by_name` is the cleanest expression of "grab one file." **Note:** `compress-tools` /
libarchive are *streaming* (no random-access `by_name`), so for *this* path the **`zip` crate is the
right tool regardless of the import-side decision.**

## Content-based format auto-detection (filenames lie)

arXiv filenames are unreliable — a `.gz` may be plain TeX, or even a raw PDF (the importer already notes
the "surprise") — so detection must be by **content**, not extension. **Per owner preference, delegate
this to the `infer` crate** (don't hand-roll a magic-byte table — compression/detection is error-prone;
use a maintained crate). `infer::get(&bytes)` returns the type from its magic, or `None` for headerless
content (= the raw/text single-file fallback). Validated in the spike:

| Input (bytes only) | `infer` result → action |
| --- | --- |
| real `.gz` / `.zip` / `.tar` | `gz` / `zip` / `tar` → decode |
| a `.gz` that is really a PDF | `pdf` → **reject** (not a source archive) |
| a `.gz` that is really plain TeX | `None` → **raw/text** single-file fallback (the arXiv "surprise") |

This rejects wrong/corrupt content instead of feeding it to a decompressor — closing part of **I-1**
(detect-then-dispatch ⇒ a mislabeled/corrupt entry is logged + skipped, never a panic). **Path B**
(libarchive/`compress-tools`) gets equivalent detection built-in; **Path A** uses `infer`. Either way
**no custom detection logic is maintained** — which is the owner's ask.

## Recommendation

**Retire the personal `libarchive-sys` fork.** Both paths do that; two later requirements — *delegate
detection to a crate* (`infer`) and *the per-task `cortex.log` scan must be maximally performant* — have
shifted the lean to **Path A**:

> **Lean: Path A — `flate2` (gz) + `tar` (tar) + `zip` (zip read/write, incl. `by_name` for the per-task
> hot path) + `infer` (detection).** Because:
> 1. The **per-task hot path already needs the `zip` crate** — `by_name("cortex.log")` is the
>    performant random-access primitive (1.4× libarchive; `compress-tools`/libarchive are *sequential*,
>    no `by_name`). Since the `zip` crate is in regardless, `flate2` + `tar` complete **one consistent
>    pure-Rust stack**, vs. Path B's *mix* of `compress-tools` (import read) + `zip` (everything else).
> 2. **`infer`** supplies detection as a maintained crate (your preference), so Path A no longer carries
>    a hand-rolled sniffer — removing Path B's last real advantage (built-in detection).
> 3. It **drops the libarchive C dependency** (consistent with libzmq → `zeromq`), at ≈ parity on the
>    import decompress (which isn't the bottleneck anyway).
>
> All compression is delegated to maintained crates (`flate2`/`tar`/`zip`) and all detection to
> `infer` — *no custom codec or detection logic*, exactly the "delegate fully to crates" ask.

> **Path B (`compress-tools` + `zip` + `infer`) stays valid if keeping the proven libarchive engine is
> preferred over removing the C dependency** — but it adds one more archive crate and is sequential on
> the per-task path.

Surface: only `importer.rs` + `helpers.rs`, behind `tests/importer_test.rs` + a new detection test, and
it **pairs with the I-1 unpack hardening** (detect → stream → log-and-skip). Independent of the
dispatcher transport work.

## Open questions

1. **The lever — fully pure-Rust (Path A, leaned) or keep the libarchive C engine (Path B)?** The one
   real decision. My lean is now **A** (consistent single pure-Rust stack, the `zip` crate is needed for
   the hot path regardless, `infer` covers detection, drops the C dep) unless you'd rather keep
   libarchive as the engine.
2. *(Path A)* **gzip backend `miniz_oxide` (zero C) vs `zlib-ng` (C, faster)?** Start pure-Rust — the
   codec isn't the bottleneck (disk is); flip the flag only if a measured need appears.
3. **Detection — settled: the `infer` crate** (your preference; no hand-rolled table either path).
4. **Combine with the I-1 importer hardening** in one pass (same files, same review)? Recommend yes.
5. **The `helpers.rs` per-task `cortex.log` scan uses the `zip` crate's `by_name` regardless of the
   lever** (fastest + random-access) — agreed?
