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

_No open issues._
