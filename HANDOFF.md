# HANDOFF — cortex-worker (latexml-oxide) fleet performance optimization

**Status:** OPEN. **Gates cortex v0.6.0** (owner call, 2026-06-20): do not cut v0.6.0 until the
pipeline-throughput improvements discovered below are landed (or the hard floor is proven + documented).

## TL;DR

A controlled **Rust-vs-Perl experiment** — both engines dockerized, **72 workers each**, on the **same
10k shuffled arXiv sandbox** (`sandbox-arxiv-10k-shuffle`), the **same dispatcher** — measured
latexml-oxide at **~6.5× faster end-to-end** than the legacy Perl LaTeXML. But latexml-oxide is known
to be **20–30× faster as a converter**. The gap means the **Rust fleet is pipeline-bound, not
engine-bound**: the converter finishes fast, then waits on the shared CorTeX path
(ventilator → sink → finalize → Postgres). **Goal: find + fix the easy-win bottlenecks so fleet
throughput approaches the engine's real speed.**

## The numbers (PRELIMINARY — finalize at Perl #192 completion)

| metric (72 workers) | Rust (latexml-oxide) #190 | Perl (LaTeXML) #192 |
|---|---|---|
| throughput | **13.4 papers/s** | ~2.1 papers/s |
| per paper / worker | **5.4 s** | ~35 s |
| 10k wall-clock | ~12 min | ~80 min (projected) |
| `log_*` rows / run | ~1.12 M | ~0.9 M |
| `loaded_file` rows / run | ~440 k | ~810 k |
| robustness | clean (pathological tail only) | `rq=0 dl=0` (with lease=2760) |

End-to-end speedup **6.5×** vs an expected converter speedup of **20–30×** → roughly **3–5× of headroom
is being lost in the pipeline**, not the engine.

## Evidence it's pipeline-bound (not the converter)

1. **Sub-linear worker scaling.** 72 Rust workers → ~13.4/s; **124 workers (bare-metal, earlier) → only
   ~16/s peak.** Nearly doubling workers barely moved throughput ⇒ a shared ceiling, not per-worker compute.
2. **Per-paper math.** 5.4 s/paper/worker at 72w. If the engine converts a typical arXiv paper in well
   under a second standalone, the remaining ~4+ s/paper is pipeline wait (source stream-in, result
   stream-out, lease, parse, finalize).
3. **Write amplification.** ~1.12 M `log_*` rows per 10k run (440 k of them `loaded_file`), inserted into
   `log_infos` already at **62 GB**. The finalize path batch-INSERTs all of these on the hot path.
4. **Prior expectation.** Project notes already flag that "latexml-oxide flips the bottleneck to I/O+DB."
   This experiment is the first hard measurement of it.

## Reproduce

Both images are built on **ubuntu:24.04** (TL2023 parity, so a Rust-vs-Perl diff is the engine, not the texmf tree):

```bash
# Rust fleet (latexml-oxide), 72 workers:
docker run -d --name cw-fleet  --network host --shm-size=32g --hostname=$(hostname) \
  -e WORKERS=72        cortex-worker:latest 127.0.0.1
# Perl fleet (legacy LaTeXML), 72 workers (override baked into the image):
docker run -d --name perl-fleet --network host --shm-size=32g --hostname=$(hostname) \
  -e CORTEX_WORKERS=72 latexml-plugin-cortex:3.0 latexml_harness 127.0.0.1 51695 51696 tex_to_html
# Rerun a service on the sandbox to feed a fleet (clean-slate, R-13 verified):
./target/release/cortex rerun sandbox-arxiv-10k-shuffle <oxidized-tex-to-html|tex_to_html> --yes
```

Run them **sequentially** (not both at once) to avoid CPU/RAM contention skewing the numbers.
Dispatcher config in use: `cortex.toml [dispatcher] lease_timeout_seconds = 2760` (raised from 240 to fit
the Perl worker's 45-min budget — see D-17; the Rust worker wants 240).

## Investigation plan

1. **Establish the engine's TRUE rate** (isolate the converter from the pipeline): run
   `cortex_worker --standalone --input <zip> --output <zip>` over a representative sample and measure
   papers/s. `(standalone_rate × 72) ÷ 13.4` = the pipeline tax. This tells us how much headroom exists.
2. **Profile the pipeline under Rust load** — where does a paper's wall-clock go between *leased* and
   *finalized*? Trace: ventilator source-send → sink recv + archive-write → `generate_report` parse →
   finalize batch INSERT → status UPDATE.
3. **Worker-count sweep on a clean box** — plot fleet throughput vs worker count (32/48/64/72/96/124); the
   knee localizes the ceiling.
4. **DB-side** — insert rate, checkpoint frequency (`max_wal_size` now 16 GB), lock/HOT contention on the
   `log_*` tables, and the `report_summary` rollup refresh cost on run completion.

## Candidate bottlenecks (ranked suspects — confirm before fixing)

- **Finalize / DB writes (most likely).** `src/backend/mark.rs::mark_done` batch-INSERTs ~1.12 M `log_*`
  rows/run + status UPDATEs, one transaction per drain, chunked at 16 k rows; `finalize_batch_size=1024`.
  Suspects: batch sizing, per-row overhead, index maintenance on the 62 GB `log_*` tables (D-8 write
  amplification), a single serialized finalize thread.
- **`loaded_file` volume.** 440 k highly-repetitive rows/run (every paper logs `TeX.pool`/`latexml.sty`/
  `article.cls`…). Consider aggregating per-(corpus,service) instead of one row/task — a large write
  reduction with no information loss (cf. P-2 territory).
- **Sink archive-writing.** `src/dispatcher/sink.rs` — result ZIP streamed in and written to `/data` by the
  archive-writer pool (`SINK_WRITERS`). Is the pool the limiter? Is `/data` (QLC RAID6) the I/O floor?
- **`generate_report` parsing.** `src/helpers.rs::generate_report` parses each `cortex.log` → message
  structs; a 32 KB log with thousands of `(Loading …)` lines — is the regex/parse hot?
- **Connection model.** `WorkerMetadata` spawns a new thread+`PgConnection` per ZMQ transaction; finalize
  DB access. Pooling (Arm 3) interactions.
- **Ventilator source-streaming.** `src/dispatcher/ventilator.rs` — reading the source archive from `/data`
  + streaming it out; `fetch_tasks` batch + lease marking.

## Constraints

- **Do not regress Perl compatibility** — the same dispatcher must keep serving the Perl worker (D-17:
  lease tuning is per-engine; the proper fix is a per-service `lease_timeout` in the DB).
- **Maximum-robustness mandate** (`docs/DESIGN_PRINCIPLES.md`): no unbounded per-event resource
  acquisition, no silent loss, idempotent crash-consistent writes. Any finalize/sink change must preserve
  the loss-free + crash-consistent guarantees (`mark_done`'s single-transaction batch, the lease reaper).
- **Keep the integrity this experiment verified**: all 5 severities parsed, every `no_problem` → valid
  HTML, failed → cortex.log-only, no archive corruption through ZMQ → sink → `/data`.

## Definition of done

- The throughput gap toward the ~20–30× engine speedup is **materially closed** (top 1–2 bottlenecks
  fixed), **or** the hard floor is identified and documented with evidence.
- No robustness regression; both Rust and Perl still run clean on the 10k sandbox.
- **Then** cut cortex v0.6.0.

## Related / loose ends

- **D-17** (`docs/POSSIBLE_UPGRADES.md`): per-service `lease_timeout` — the Rust(180s)-vs-Perl(2700s) lease
  mismatch surfaced by this experiment; interim mitigation is the global `lease_timeout_seconds = 2760`.
- **`loaded_file` fix** (latexml-oxide logger: capture `(Loading …)` notes into `cortex.log`): landed;
  adds ~440 k rows/run — a direct input to the write-amplification suspect above.
- **5 oxidized #190 stragglers** left in TODO (pathological tail, recoverable, not a bug) — close at the
  v0.6.0 wrap.
- The cortex commits staged for v0.6.0 (R-13, the worker launcher, D-16, D-17, the docker work) are on
  `master` but **the version bump + tag wait on this perf work**.
