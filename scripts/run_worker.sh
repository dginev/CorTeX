#!/usr/bin/env bash
#
# run_worker.sh — the ONE reproducible launcher for the latexml-oxide cortex_worker fleet.
#
# Why this exists: the harness's own `--workers` default is CPU-derived and sizes to LOGICAL cores
# (a 64-physical / 128-logical box derives ~124), but the empirically battle-hardened sweet spot is
# PHYSICAL cores + 1/8 (×1.125) — 72 on this host. Launching without `--workers` silently over-commits
# (the 124-vs-72 accident). So every parameter is pinned in a versioned env file and this script
# REFUSES to start if any required one is missing — a wrong/absent param must fail loud, not default.
#
# Config: /etc/cortex/worker.env (per host; template: deploy/systemd/worker.env.example).
# Run standalone for debugging, or via the cortex-worker.service unit (its ExecStart calls this).
#
# Deliberately does NOT set the memory/timeout guards (--child-mem-limit-mb, --mem-pressure-floor-mb,
# --timeout, --max-rss-mb): the binary's COMPILED defaults are the validated values (child-mem-limit
# 5632, recycle 1408, auto governor floor, timeout 180s — see docs/CORTEX_WORKER_HARNESS.md and the
# `--help` history). Pinning a copy here only lets it rot — the stale doc's `--child-mem-limit-mb 8192`
# is exactly what once spiked a 72-worker sweep to 207 GB and tripped the kernel OOM-killer.
set -euo pipefail

ENV_FILE="${WORKER_ENV_FILE:-/etc/cortex/worker.env}"
[ -r "$ENV_FILE" ] || { echo "run_worker: missing/unreadable $ENV_FILE" >&2; exit 1; }
set -a; . "$ENV_FILE"; set +a

# Required parameters — refuse to launch on any missing/empty one (no silent default).
for v in WORKERS SERVICE SOURCE_ADDRESS SINK_ADDRESS PROFILE WORKER_BIN; do
  [ -n "${!v:-}" ] || { echo "run_worker: required parameter $v not set in $ENV_FILE" >&2; exit 2; }
done
[ -x "$WORKER_BIN" ] || { echo "run_worker: WORKER_BIN not executable: $WORKER_BIN" >&2; exit 3; }

# Ensure the disk-backed scratch dir exists (TMPDIR from the env file; the worker stages unpack/convert
# there via tempfile/env::temp_dir). Must NOT be a ramdisk — see worker.env.example / KNOWN_ISSUES D-18.
[ -n "${TMPDIR:-}" ] && mkdir -p "$TMPDIR"

echo "run_worker: cortex_worker --harness --workers $WORKERS --service $SERVICE" \
     "$SOURCE_ADDRESS -> $SINK_ADDRESS --profile $PROFILE (dumps: ${LATEXML_DUMP_DIR:-binary default})"

# `exec` so the harness becomes the unit's MainPID (systemd SIGTERMs it directly for a clean fleet
# teardown). Memory/timeout guards intentionally omitted — left at the binary's validated defaults.
exec "$WORKER_BIN" --harness \
  --workers        "$WORKERS" \
  --service        "$SERVICE" \
  --source-address "$SOURCE_ADDRESS" \
  --sink-address   "$SINK_ADDRESS" \
  --profile        "$PROFILE"
