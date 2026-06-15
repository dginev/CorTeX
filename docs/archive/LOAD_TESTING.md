# Live-backup seeding, migration verification & load testing

> **PARTLY SUPERSEDED (2026-06-15).** The "seed → load-test → empty + reseed" lifecycle is overtaken
> by events: the seeded `cortex_load` DB (real arXiv data, ~5.87M tasks) became the **persistent
> public showcase database** behind `corpora.latexml.rs` (served by `cortex-frontend.service`) — it is
> **not** reset. The still-evergreen part of this doc is the **migration-fidelity check**
> (`scripts/verify_migrations.sh`, kept), which diffs a from-scratch `migrations/` schema against a
> restored backup. See `docs/DEPLOYMENT.md` for the live deployment.

Owner request (2026-06-13): seed a dedicated database from the **live CorTeX backup**, use it to
(a) verify our embedded migrations reproduce the live schema (find gaps), and (b) run real-world
load testing at production data volume. Then empty + reseed for a clean state.

This is the first time the productized installer + the resilience work (pooling, bounded metadata
writer D-1, backpressure D-6, the rollup matview) meet **real data volume and a real schema
history** — exactly where latent gaps surface. Plan below; **blocked on the backup file + a few
decisions** (see "Inputs needed").

## Guardrails

- **Restore into a dedicated database** (proposed `cortex_load`), never into `cortex` (prod) or
  `cortex_tester` (the suite truncates/reseeds those). Keep the two working DBs intact.
- **NVMe, never `/data`** (CLAUDE.md): the restore target's data dir must be on local NVMe.
- Runtime config now means **no recompile** to repoint a binary — set
  `CORTEX_DATABASE__URL=postgres://…/cortex_load` (or `DATABASE_URL`, which still wins) for the
  dispatcher/frontend/CLI. (This is itself a thing to confirm works end-to-end.)
- Sandbox notes from the Arm-14 bench: an in-process `pg_stat_activity` sampler trips seccomp
  (SIGSTKFLT) and the background-run wrapper signal-kills these jobs — **run load tests in the
  foreground**, time-boxed; chunk inserts under PG's 65535 bind-param cap.

## Phase 1 — Restore

1. `createdb cortex_load` (owned by the `cortex` role).
2. Restore the backup: `pg_restore -d cortex_load <dump>` (custom/`-Fc`) **or**
   `psql cortex_load -f <dump.sql>` (plain). Format TBD (see Inputs).
3. Sanity: row counts on `tasks`, the five `log_*` tables, `corpora`, `services`,
   `worker_metadata`, `historical_runs`/`historical_tasks`.

## Phase 2 — Migration verification (the "where are we missing migrations" question)

The live DB schema may sit **behind** our `migrations/` (it predates the recent matview /
unique-constraint / varchar-widen work) and may carry **manual drift** the migrations don't capture.

1. Read the restored `__diesel_schema_migrations` ledger; diff against `migrations/` → the **pending
   set**.
2. Apply pending migrations the way the *installer* will (the embedded path — `cortex init` /
   `migrations::run`), **on real data**. This is where the value is:
   - `worker_metadata` `UNIQUE(name, service_id)` migration's **one-time dedupe** must survive real
     duplicate rows.
   - `tasks.entry` widen to `varchar(4096)` on a large table (catalog-only — confirm no rewrite).
   - the `report_summary` matview build time over millions of `log_*` rows.
3. **Fidelity check — automated:** run **`scripts/verify_migrations.sh
   postgres://…/cortex_load`**. It rebuilds a reference DB by running `migrations/` on an empty DB,
   then diffs the two `--schema-only` dumps (locale-stable, sorted-set comparison robust to
   pg_dump's object-ordering). Output sections: *"In SOURCE but NOT reproduced by migrations/"* =
   schema the live DB has that we must author a migration for (the productization-blocking finding);
   *"… MISSING from SOURCE"* = the live DB is simply behind. Exit 0 = clean, 1 = drift. **Validated
   against the live-equivalent `cortex` DB (reports OK) and a synthetic drift (correctly flagged).**

## Phase 2 — RESULT (2026-06-14, dump `cortex_20260614_023225.dump`, 5.8 GB `-Fc`)

Restored **schema-only** into `cortex_load` and ran `scripts/verify_migrations.sh`. **Our embedded
migrations structurally reproduce the live schema** — the live DB is at migration `20250625185146`
and simply *behind* our 2026-06-13 set (`jobs` table, `report_summary` matview, `worker_metadata`
UNIQUE, `tasks.entry` widen → all correctly "missing from source", applied by `cortex init`). The
**only genuine "source has, migrations don't reproduce"** is:
1. the **manual autovacuum tuning** (`WITH (autovacuum_enabled='true', autovacuum_vacuum_scale_factor
   ='0.0002', …)`) — *intentionally* operator-applied per §8, not a structural migration; **decision
   for the owner:** bake it into a migration vs keep it operator-documented;
2. `tasks.entry varchar(200)` — just the live DB being behind the widen (becomes 4096 on migrate).

So: **no missing tables/columns/constraints.** Next: full data restore → apply our migrations on
real data (verify the `worker_metadata` UNIQUE dedupe survives real dup rows, the `entry` widen on a
big `tasks`, and the `report_summary` matview build time) → load test.

## Phase 3 — Load testing (real volume)

Point the stack at `cortex_load` and measure under the documented target (~2 admins, ~20 users,
~200 ZMQ workers, ~100 tasks/s):

- **Dispatch pipeline:** reset a real service's tasks to TODO (or a slice), run the dispatcher +
  echo workers (`examples/bench_pipeline.rs` A/B harness, or a real worker fleet). Measure
  throughput, pool saturation, that **D-6 backpressure** engages (or correctly never does), and the
  **finalize + rollup-refresh** time at volume.
- **Frontend read latency on real data:** report pages over millions of `log_*` rows — confirm the
  `report_summary` rollup fast-path holds the category/`what` reports well under the ~500ms the
  Arm-14 spike saw on the live aggregation; check the relocated+pooled report routes under
  concurrency.
- Record results in `RESOURCE_RATIONALIZATION.md` (the existing perf-evidence ledger).

## Phase 4 — Reset

`TRUNCATE`/`dropdb cortex_load` + restore again for a clean state, or drop entirely once findings
are captured. (No effect on `cortex`/`cortex_tester`.)

## Inputs needed (from the owner)

1. **Backup file**: path on the box + format (`pg_dump -Fc`, plain SQL, or `pg_dumpall`) + rough
   size (drives restore time + disk headroom).
2. **Target DB name** — OK to use `cortex_load`, or prefer another?
3. **When**: the schema-diff tooling is **built and validated** (`scripts/verify_migrations.sh`); it
   runs the moment a restored `cortex_load` exists. Remaining staging: a thin `restore_and_verify.sh`
   wrapper (restore + invoke the verifier) once the backup format is known.
4. **Load-test shape**: dispatch-throughput, frontend-read-latency, or both? Echo workers (safe,
   measures the framework) or real `latexml` workers (measures conversion too)?
