#!/usr/bin/env bash
#
# deploy.sh — the graceful CorTeX upgrade: build BOTH binaries → migrate (online) → restart the
# frontend AND the dispatcher → verify both. "Rebuild both, then replace the running ones."
#
# Run from the deploy host (the box whose systemd cortex-frontend.service / cortex-dispatcher.service
# serve from this repo). The frontend and dispatcher are independent over the shared DB (no start
# ordering between them), but they are one DEPLOYMENT: a frontend with active tasks needs a live
# dispatcher to drain them, so an upgrade replaces both together.
#
# Why this ordering is safe:
#   • Build first — both binaries are produced before anything is touched, so a compile failure aborts
#     with the old binaries still serving.
#   • Migrate online — the Arm-3-style migrations are backward-compatible with the running binary
#     (add-column / FK NOT VALID + VALIDATE), so the old binary keeps serving while the schema evolves.
#   • Restart via systemd — `systemctl restart` STOPS the old (SIGTERM, up to TimeoutStopSec, then
#     SIGKILL) BEFORE starting the new, so the dispatcher's ZMQ ports (:51695/:51696) hand over with no
#     bind race. In-flight conversions during the dispatcher swap are re-leased by the lease reaper
#     (no loss). NOTE: the dispatcher's graceful shutdown can take up to its TimeoutStopSec (120s) when
#     idle (docs/KNOWN_ISSUES.md), so its restart leg may pause before the new binary binds — the
#     verify step polls for it.
#
# This does NOT do the one-time DB promotion/rename (cortex_load → cortex) — that was a one-off.
#
# Usage:
#   scripts/deploy.sh                  # build + migrate + restart frontend & dispatcher + verify both
#   scripts/deploy.sh --no-migrate     # code/template/CSS-only deploy
#   scripts/deploy.sh --frontend-only  # skip the dispatcher (UI-only change; no fleet to disrupt)
set -euo pipefail

cd "$(dirname "$0")/.."

FRONTEND=cortex-frontend.service
DISPATCHER=cortex-dispatcher.service
ENV_FILE=/etc/cortex/frontend.env
PORT="$(sudo grep -hoE 'ROCKET_PORT=[0-9]+' "$ENV_FILE" 2>/dev/null | grep -oE '[0-9]+' || echo 8000)"
MIGRATE=1
DO_DISPATCHER=1
for arg in "$@"; do
  case "$arg" in
    --no-migrate) MIGRATE=0 ;;
    --frontend-only) DO_DISPATCHER=0 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done
# Only touch the dispatcher if it is a managed (enabled) unit on this host.
dispatcher_managed() { [ "$DO_DISPATCHER" = 1 ] && systemctl is-enabled "$DISPATCHER" >/dev/null 2>&1; }

echo "==> 1/5  build release (both binaries; the running ones keep serving during the build)"
cargo build --release

if [ "$MIGRATE" = 1 ]; then
  echo "==> 2/5  migrate the production DB online (idempotent; embedded migrations)"
  PROD_URL="$(sudo grep -hoE 'postgres://[^ ]+' "$ENV_FILE" | head -1)"
  # Explicit DATABASE_URL wins over the working-tree .env (dotenvy is non-overriding), so this
  # always targets production, not the dev DB.
  DATABASE_URL="$PROD_URL" ./target/release/cortex init
else
  echo "==> 2/5  skipping migration (--no-migrate)"
fi

echo "==> 3/5  restart $FRONTEND"
sudo systemctl restart "$FRONTEND"

if dispatcher_managed; then
  echo "==> 4/5  restart $DISPATCHER (systemd stops the old before the new binds — clean port handover)"
  sudo systemctl restart "$DISPATCHER"
else
  echo "==> 4/5  skipping dispatcher ($([ "$DO_DISPATCHER" = 0 ] && echo '--frontend-only' || echo 'service not enabled on this host'))"
fi

echo "==> 5/5  verify"
ok=1
sleep 2
fstate="$(systemctl is-active "$FRONTEND" || true)"
code="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:${PORT}/healthz" || echo 000)"
echo "    frontend=$fstate  GET /healthz -> HTTP $code"
{ [ "$fstate" = active ] && [ "$code" = 200 ]; } || ok=0
if dispatcher_managed; then
  # Poll: the dispatcher leg can lag (slow idle shutdown of the old, then the new binds + connects).
  dstate=""; bound=0
  for _ in $(seq 1 30); do
    dstate="$(systemctl is-active "$DISPATCHER" || true)"
    bound="$(ss -ltn 2>/dev/null | grep -cE ':5169[56][^0-9]')"
    { [ "$dstate" = active ] && [ "$bound" = 2 ]; } && break
    [ "$dstate" = failed ] && break
    sleep 1
  done
  echo "    dispatcher=$dstate  ZMQ ports bound=$bound/2"
  { [ "$dstate" = active ] && [ "$bound" = 2 ]; } || ok=0
fi

if [ "$ok" != 1 ]; then
  echo "==> DEPLOY FAILED — not healthy (check: journalctl -u $FRONTEND -u $DISPATCHER -n 50)" >&2
  exit 1
fi
echo "==> deploy OK"
