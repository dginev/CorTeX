# CorTeX Known Issues — resilience & correctness ledger

> Running list of known problems, weighted toward **robustness/fault-tolerance** gaps (see
> [`DESIGN_PRINCIPLES.md`](DESIGN_PRINCIPLES.md)). Owner's direction: **record every known problem as
> we find it; we go back and solve them all at the end.** Do not silently fix-and-forget, and do not
> silently leave a discovered gap unrecorded.
>
> Status legend: 🔴 open · 🟢 resolved (kept for history). This ledger is **bugs only**; mitigated
> items with a documented upgrade path (the former 🟡 "partially mitigated" set) live in
> [`POSSIBLE_UPGRADES.md`](POSSIBLE_UPGRADES.md) — stretch goals, not defects.
> Severity: **S1** can crash/corrupt/destabilise the system · **S2** drops or hides work ·
> **S3** correctness/UX papercut · **S4** cleanup/polish.

> **2026-06-16 — clean slate.** Every bug tracked through the productize-2026 sprint reached 🟢
> (36 of 36); that all-resolved ledger is archived verbatim at
> [`archive/KNOWN_ISSUES_June16.md`](archive/KNOWN_ISSUES_June16.md). **No open issues.** Add new
> findings below the moment they're discovered, with a stable ID, severity, and a one-line fix
> direction; promote 🔴→🟢 (don't delete) when fixed, with the commit that did it.

**No open issues.**

## Resolved since the reset

| # | Sev | Status | Issue |
|---|---|---|---|
| I-2 | S4 | 🟢 | **`activate`/`extend` loaded a corpus's whole import-entry list into RAM — fixed (2026-06-16, recorded + resolved same day).** `services_aggregate.rs` (`Backend::activate`/`extend`, reached from the web `/services/<s>/activate` + the agent `POST /api/corpora/<c>/services/<s>` and `…/extend`) loaded **every** import `entry` of a corpus into a `Vec` (~1.5M strings ≈ 120 MB for arxmliv), then issued one `create`/`create_if_new` per entry — O(corpus) RAM **and** ~1.5M per-document round-trips. Both now do the task creation as **one server-side `INSERT … SELECT`** straight from the `import`-service rows (`extend` adds `ON CONFLICT (entry, service_id, corpus_id) DO NOTHING` — the bulk twin of the old per-entry `create_if_new`, preserving its only-new semantics; `activate` is guarded against an already-registered pair so needs no conflict clause). No entry list is materialized and there is no per-document round-trip — **bounded work regardless of corpus size**, matching the precedent in `save_historical_tasks`. Verified by `corpora_test` (`post_corpora_extend_adds_new_entries` pins extend's only-new behaviour; the registration tests cover activate) + `services_test`, both green. Turned out cleaner/lower-risk than the initial "deferred" assessment once the diesel `INSERT…SELECT` precedent + the `UNIQUE` constraint made it a contained swap. `src/backend/services_aggregate.rs`. |
| E-3 | S4 | 🟢 | **`cortex export-dataset` held the whole work-list in RAM — fixed (2026-06-16, graduated from POSSIBLE_UPGRADES).** `export_html_dataset` loaded one severity's `entry` paths into a `Vec` and bucketed a lightweight `{result_zip, paper, yymm}` per task into a `BTreeMap` before bundling — O(corpus) resident (≈ a few hundred MB for the largest ~1.5M-paper corpus). Rewritten to **stream**: an `ArchiveStreamer` holds **at most one output archive open** at a time and copies each paper's HTML straight in, fed by a **keyset-paginated** entry scan (`EXPORT_PAGE_SIZE = 10_000`, `entry > $cursor ORDER BY entry` — `entry` is unique within a `(corpus, service)`, so it's a gap-free cursor, no deep-`OFFSET`). Both modes feed same-key entries contiguously: month mode does one cross-severity `ORDER BY entry` pass (each `yymm` is a contiguous run, so a month archive still aggregates papers from several severities); severity mode processes one severity at a time. Footprint is now **O(one page + one open zip + one paper's HTML)** regardless of corpus size. Resume parity preserved (an existing `.zip` is skipped without opening a writer). Contract pinned by `tests/export_test.rs` — the original single-paper test plus a new `streams_multiple_months_and_severities` (multi-`yymm` rotation + cross-severity month aggregation + per-severity spanning months). `src/backend/export.rs`. |
