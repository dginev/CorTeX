#!/usr/bin/env bash
# Restart cortex-frontend if it is active but /healthz is unreachable or not "ok"
# (catches a hung/deadlocked process that Restart=always won't, since it never exits).
# No-op if the service isn't the active manager (won't fight a manual frontend).
set -u
URL="http://127.0.0.1:8000/healthz"
systemctl is-active --quiet cortex-frontend.service || exit 0
if body="$(curl -fsS --max-time 5 "$URL" 2>/dev/null)"; then
  case "$body" in
    *'"status":"ok"'*) exit 0 ;;
    *) logger -t cortex-health "healthz reachable but not ok -> restarting cortex-frontend"; systemctl restart cortex-frontend.service ;;
  esac
else
  logger -t cortex-health "healthz unreachable -> restarting cortex-frontend"
  systemctl restart cortex-frontend.service
fi
