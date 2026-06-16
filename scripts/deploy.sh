#!/usr/bin/env bash
#
# deploy.sh — the recurring CorTeX frontend deploy: build → migrate (online) → restart → verify.
#
# Run from the deploy host (the box with the systemd cortex-frontend.service serving from this repo).
# The Arm-3-style migrations are backward-compatible with the running binary (add-column / FK NOT
# VALID + VALIDATE), so migrating BEFORE the restart is zero-downtime: the old binary keeps serving
# while the schema evolves, and only the brief `systemctl restart` is downtime.
#
# This does NOT do the one-time DB promotion/rename (cortex_load → cortex) — that was a one-off.
#
# Usage:  scripts/deploy.sh        # build + migrate + restart + verify
#         scripts/deploy.sh --no-migrate   # skip the migration step (code/template/CSS-only deploy)
set -euo pipefail

cd "$(dirname "$0")/.."

SERVICE=cortex-frontend.service
ENV_FILE=/etc/cortex/frontend.env
PORT="$(sudo grep -hoE 'ROCKET_PORT=[0-9]+' "$ENV_FILE" 2>/dev/null | grep -oE '[0-9]+' || echo 8000)"
MIGRATE=1
[ "${1:-}" = "--no-migrate" ] && MIGRATE=0

echo "==> 1/4  build release (the old binary keeps serving during the build)"
cargo build --release

if [ "$MIGRATE" = 1 ]; then
  echo "==> 2/4  migrate the production DB online (idempotent; embedded migrations)"
  PROD_URL="$(sudo grep -hoE 'postgres://[^ ]+' "$ENV_FILE" | head -1)"
  # Explicit DATABASE_URL wins over the working-tree .env (dotenvy is non-overriding), so this
  # always targets production, not the dev DB.
  DATABASE_URL="$PROD_URL" ./target/release/cortex init
else
  echo "==> 2/4  skipping migration (--no-migrate)"
fi

echo "==> 3/4  restart $SERVICE"
sudo systemctl restart "$SERVICE"

echo "==> 4/4  verify"
sleep 2
state="$(systemctl is-active "$SERVICE" || true)"
code="$(curl -s -o /dev/null -w '%{http_code}' "http://localhost:${PORT}/healthz" || echo 000)"
echo "    service=$state  GET /healthz -> HTTP $code"
if [ "$state" != active ] || [ "$code" != 200 ]; then
  echo "==> DEPLOY FAILED — frontend is not healthy (check: journalctl -u $SERVICE -n 50)" >&2
  exit 1
fi
echo "==> deploy OK"
