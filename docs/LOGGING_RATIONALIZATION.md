# Logging rationalization — plan-of-record (issue #30)

> Status: **IN PROGRESS 2026-06-19.** Phase 0 (the original ask) is **done**; Phase 1 (CLI parity)
> **lands with this doc**; Phases 2–3 (structured correlation fields, per-consumer projections) are
> scoped here and rolled out incrementally. This is the live home for [#30](https://github.com/dginev/CorTeX/issues/30)
> and the detail behind **Arm 8a** (tracing/metrics rails) in
> [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md); the **Arm 8b** Observatory screens consume what this lays down.

## What #30 originally asked, and why it's mostly already solved

The 2018 issue — *"adopt the [`log` crate] and adapt all prints into a consistent logging syntax;
it is a staring challenge to keep track of the dispatcher log during large-scale processing"* — has
been **substantially delivered, by a better choice than `log`**: the project adopted **`tracing`**
(`tracing` + `tracing-subscriber`, `Cargo.toml`), with:

- one `init_tracing()` bootstrap (`src/observability.rs`), `RUST_LOG`-filtered, idempotent
  (`try_init`), `rocket=warn` by default so the frontend's poll loop can't drown app events;
- the dispatcher hot path fully on leveled `tracing` — `ventilator.rs` (16 sites), `sink.rs` (13),
  `manager.rs` (9), `finalize.rs` (4), `server.rs` (4) — with per-task narration at `trace`/`debug`,
  **filtered out at the default `info`** so a high task rate no longer pays a synchronous locked-stderr
  write per event (this is the fix for **KNOWN_ISSUES D-11**, the per-event-print throughput-DoS);
- the frontend entirely on `tracing` (the only residual raw prints are the importer's ~28
  *intentional* operator-facing progress/error banners in `src/importer.rs`, plus a few error
  diagnostics).

So the literal "staring challenge" is gone, and the `log`-crate proposal in the issue body is
**obsolete** — `tracing` is strictly better for what came next (below). #30's *title* is still right;
its *body* should be retired.

## Why `tracing` (not `log`) was the right call — the three consumers prove it

CorTeX now serves **three** consumers (the Arm 15 symmetry contract): the **web UI**, the **`cortex`
CLI**, and the **agentic RESTful API**. They make the architecture clear in a way the flat 2018 ask
could not: **logging is not one stderr stream — it is one structured event substrate, rendered three
ways.** Each consumer wants a *different projection of the same events*:

| Consumer | What it needs from logging | State today |
|---|---|---|
| **Dispatcher / operator** (stderr during big runs) | human text, level-filtered, low overhead | ✅ done (`tracing` + `RUST_LOG`) |
| **`cortex` CLI** (`bin/cortex.rs`) | a subscriber at all + a verbosity knob; diagnostics on **stderr** so `--json` (stdout) stays clean | ⛏️ **fixed in Phase 1** (was: CLI never called `init_tracing()`) |
| **Agentic REST API** | *structured, correlatable, machine-readable* — not stderr text | 🟡 status/jobs/runs DTOs + `/metrics` exist; log **events mostly lack run/task/service ids** |

That table is the clarified design decision: the **agent plane** dictates the remaining work, and it
wants **structured fields**, not prettier strings. `tracing`'s layers/subscribers + structured fields
+ spans are exactly the fan-out mechanism — one event substrate → human `fmt` layer (stderr), a JSON
layer (machine), `/metrics` (Prometheus), and a future UI live-feed layer — which the bare `log` crate
cannot express. The three-consumer design **vindicates the `tracing` choice**.

### Scope boundary (do not conflate)

The per-task **`cortex.log`** (LaTeXML's per-conversion output, parsed by `helpers.rs::parse_log`
out of each result `.zip`, stored as `log_{info,warning,error,fatal,invalid}` rows and surfaced as
`DocumentReportDto`) is **data**, not process observability. #30 governs the **framework's own
process logging** (the `tracing` substrate), not the per-task message corpus. They meet only at the
sink, which *parses* `cortex.log` into the DB.

## Owner decision (2026-06-19)

Accepted the recommendation **"A + B now"**: finish the CLI parity (A) and lay the structured-field
substrate (B) now; defer the agent log-streaming surface and the UI live-feed (C2) into Arm 8b.

## Phases

### Phase 0 — `tracing` adoption + D-11 — ✅ DONE (pre-existing)
As above. The original #30 letter is satisfied here.

### Phase 1 — CLI logging parity — ⛏️ LANDS WITH THIS DOC
Closes the one concrete gap that still matches #30's title precisely: the **`cortex` CLI never
initialized a subscriber**, so `RUST_LOG` was dead in the CLI and there was no verbosity flag.

- `src/observability.rs`: **fix the latent writer bug** — the `fmt()` default writer is
  `fn() -> io::Stdout` (verified in `tracing-subscriber-0.3` `fmt/mod.rs`), so `init_tracing()` was
  writing to **stdout** despite its doc-comment claiming stderr. Pin it to **stderr** explicitly
  (correct for a log stream, and it matches the documented intent). This also unblocks the CLI, whose
  `--json` subcommands write machine output to stdout — logs must not interleave there.
- `src/observability.rs`: add `init_cli_tracing(verbose: u8, quiet: bool)` — same env-filter logic,
  always **stderr**, but when `RUST_LOG` is unset the level is driven by repeated `-v` flags:
  `-q`→`error`, default→`warn`, `-v`→`info`, `-vv`→`debug`, `-vvv`→`trace`. An explicit `RUST_LOG`
  still wins. The CLI default is `warn` (not `info`) because the CLI's *normal* output is the
  command's own stdout/`--json`; `tracing` events are diagnostics the user opts into.
- `bin/cortex.rs`: add global `-v/--verbose` (counted) and `-q/--quiet` flags to `Cli`, and call
  `init_cli_tracing(verbose, quiet)` at the top of `main()`.

### Phase 2 — structured correlation fields (the keystone; Arm 8a) — ⛏️ CODEBASE-WIDE SWEEP
Arm 8's explicit requirement: *"consistent run/task ids in every log line so an agent can
correlate."* Owner directive (2026-06-19): **be consistent in the entire codebase.** `jobs.rs`
(`kind`/`actor`/`job`/`elapsed_ms`/`error`), `finalize.rs:110,172` (`corpus_id`/`service_id`), and
`ventilator.rs:223` were already in the target style — they define it.

#### The canonical convention (the codebase-wide standard)

A `tracing` event records **identifiers and quantities as structured fields**, never interpolated
into the message string; the message is a short static `component: description`.

- Move every interpolated variable into a field — preserve all information, drop nothing.
- Field syntax: bare `name` when it equals the variable; `name = expr`; `name = %expr` for `Display`
  (strings, uuids, errors-with-Display); `name = ?expr` for `Debug` (paths, structs, `Option`, enums).
  A `{x}` → `%x`; a `{x:?}` → `?x`.
- Canonical field names (use when in scope): `actor`, `job` (uuid), `corpus`/`corpus_id`,
  `service`/`service_id`, `task_id`, `entry`, `worker`, `error`, plus event-specific quantities
  (`count`, `attempt`, `bytes`, `elapsed_ms`/`took_ms`/`recv_ms`, `discarded`, …).
- Keep the level unchanged. Leave already-structured events, and events with no variables, alone.
- Fields-on-events, **not** per-dispatch spans — matches the existing convention and avoids per-event
  span overhead on the hot path (the D-11 lesson).

#### Status
- ✅ **Dispatcher + backend done:** `ventilator.rs`, `sink.rs`, `finalize.rs`, `server.rs`,
  `backend.rs`. (`manager.rs`'s events are thread-lifecycle messages with no identifiers in scope —
  already clean static strings, nothing to field-ify.)
- ⛏️ **Frontend (admin actions):** `concerns.rs`, `webauthn.rs`, `corpora.rs`, `management.rs`,
  `audit.rs`, `retention.rs`, `reports.rs` — converting the interpolating events to the convention
  (the already-structured ones in `concerns.rs` are left as-is).
- Done one file at a time, verified centrally (build + `clippy --all-targets` + `fmt --check`); the
  dispatcher's race-prone code (D-4/D-5/D-6 history) gets no sweeping single-pass rewrite.

### Phase 3 — per-consumer projections — DEFERRED (with Arm 8b)
- **CLI:** `--log-format=auto|text|json` (needs the `tracing-subscriber/json` feature — not yet
  enabled).
- **Agent API surface — decided C1 (cheapest):** agents correlate via the Phase-2 structured ids +
  the existing `/api/status`, `/api/jobs`, `/api/runs` DTOs + `/metrics`. **No** raw-log endpoint in
  the API contract for now.
- **Live UI feed (C2):** a bounded in-memory ring-buffer `tracing` Layer exposed over SSE, feeding the
  Observatory live console — belongs to **Arm 8b**, not here.
- (C3, a `tracing-appender` JSON log file served by the API, is rejected for now — adds a dep + a file
  lifecycle for little gain over C1.)

## Tests / verification

- Build + `cargo clippy --all-targets -- -D warnings` clean (the pre-push gate).
- Manual: `cortex status --json | jq .` stays valid JSON with `-vv` set (logs go to stderr, not
  stdout) — proves the writer fix.
- `RUST_LOG` override still wins over `-v` (env-filter precedence).

## Relationship to existing work

- **Arm 8 (Observability)** in [`PRODUCTIZING_PLAN.md`](PRODUCTIZING_PLAN.md): this *is* Arm 8a's
  "tracing rails" detail; the Observatory (8b) consumes the Phase-2 fields + the C2 feed.
- **Arm 15 (Experience rationalization)** — the three-consumer symmetry contract that frames the whole
  rationalization above.
- **KNOWN_ISSUES D-11** — the per-event-print throughput-DoS, resolved in Phase 0; Phase 2 must not
  reintroduce a per-event synchronous cost (hence fields-on-events, not hot-path spans).
