#!/usr/bin/env bash
# run_dispatcher.sh — launch the CorTeX dispatcher from the prebuilt release binary against the
# production pipeline DB. Replaces the ad-hoc `cargo run --release --bin dispatcher`, which
# recompiles on every launch and silently falls back to the localhost/cortex default when
# DATABASE_URL isn't exported (→ "password authentication failed", a crash loop).
#
# It (1) builds the release binary once, then execs it — fast startup, and always the latest code
# (e.g. the `Warn:`→`Warning:` log-parser fix); (2) resolves the DB explicitly and prints the target
# (credentials redacted) so you can confirm it's production before the dispatcher starts managing the
# task queue; (3) passes any extra args through to the binary.
#
# DB resolution (first hit wins):
#   $DATABASE_URL  →  /etc/cortex/dispatcher.env   (the canonical managed-env location)
# If neither is set (or dispatcher.env still holds the scaffold `USER:PASSWORD` placeholder), it
# errors and points you at the frontend's DB.
#
# For a supervised, reboot-surviving dispatcher, prefer the systemd unit (deploy/README.md
# "Dispatcher cutover"): configure /etc/cortex/dispatcher.env, then
#   sudo systemctl enable --now cortex-dispatcher
#
# Usage:
#   scripts/run_dispatcher.sh                              # foreground, perpetual
#   DATABASE_URL=postgres://… scripts/run_dispatcher.sh    # explicit DB for this launch
#   nohup scripts/run_dispatcher.sh >dispatcher.log 2>&1 & # detached
set -euo pipefail
cd "$(dirname "$0")/.."

# Resolve DATABASE_URL; echo where it came from, or return non-zero if none is usable.
resolve_db() {
  if [[ -n "${DATABASE_URL:-}" ]]; then echo "env"; return 0; fi
  local f=/etc/cortex/dispatcher.env url
  url="$({ sudo -n grep -hE '^DATABASE_URL=' "$f" 2>/dev/null || grep -hE '^DATABASE_URL=' "$f" 2>/dev/null; } | head -1 | cut -d= -f2- | tr -d '"')"
  # Skip the unconfigured scaffold template (username `USER` / `USER:PASSWORD`).
  if [[ -n "$url" && "$url" != *"://USER"* && "$url" != *"USER:PASS"* ]]; then
    export DATABASE_URL="$url"
    echo "$f"
    return 0
  fi
  return 1
}

if ! src="$(resolve_db)"; then
  cat >&2 <<'MSG'
error: no usable DATABASE_URL for the dispatcher.
  • set it for this launch:        DATABASE_URL=postgres://… scripts/run_dispatcher.sh
  • or configure the managed env:  sudoedit /etc/cortex/dispatcher.env   (then re-run)
The production pipeline DB is usually the one the frontend reads — see /etc/cortex/frontend.env.
MSG
  exit 2
fi

redacted="$(printf '%s' "$DATABASE_URL" | sed -E 's#(://)[^@]*@#\1<redacted>@#')"
echo "dispatcher DB (${src}): ${redacted}"
export RUST_LOG="${RUST_LOG:-cortex=info}"

echo "building release dispatcher…"
cargo build --release --bin dispatcher
echo "starting dispatcher (Ctrl-C to stop) — binds :51695 (ventilator) + :51696 (sink)"
exec target/release/dispatcher "$@"
