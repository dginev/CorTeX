#!/usr/bin/env bash
# Headless screenshot of a URL — for visual UI verification without a GUI or extra installs.
#
# Prefers Chromium (runs fine alongside a desktop browser); falls back to headless Firefox.
# NOTE: the snap Firefox allows only ONE instance, so the Firefox path needs the desktop Firefox
# closed (or a CI box with no GUI session). Append `?theme=midnight` / `?theme=paper` to the URL to
# force a theme for the shot.
#
# Usage: scripts/screenshot.sh <url> <out.png> [width] [height]
set -euo pipefail
url="${1:?usage: screenshot.sh <url> <out.png> [width] [height]}"
out="${2:?usage: screenshot.sh <url> <out.png> [width] [height]}"
w="${3:-1440}"
h="${4:-1200}"
mkdir -p "$(dirname "$out")"
# Firefox writes the screenshot relative to its CWD (and snap-confined), so always pass an ABSOLUTE
# path — a relative one silently produces nothing.
out="$(cd "$(dirname "$out")" && pwd)/$(basename "$out")"
here="$(cd "$(dirname "$0")/.." && pwd)"

# Preferred route: Playwright (repo venv) driving system Google Chrome — runs alongside a desktop
# browser, retina-scaled, waits for network idle. See scripts/screenshot.py.
if [ -x "$here/.venv/bin/python" ] && "$here/.venv/bin/python" -c "import playwright" 2>/dev/null; then
  if "$here/.venv/bin/python" "$here/scripts/screenshot.py" "$url" "$out" "$w" "$h" >/dev/null 2>&1; then
    [ -s "$out" ] && { echo "$out"; exit 0; }
  fi
fi

for chrome in chromium chromium-browser google-chrome google-chrome-stable; do
  if command -v "$chrome" >/dev/null 2>&1; then
    "$chrome" --headless=new --disable-gpu --hide-scrollbars \
      --window-size="${w},${h}" --screenshot="$out" "$url" >/dev/null 2>&1 || true
    [ -s "$out" ] && { echo "$out"; exit 0; }
  fi
done

if command -v firefox >/dev/null 2>&1; then
  # `--screenshot <abs>` BEFORE `--window-size` is the form snap Firefox honors.
  MOZ_NO_REMOTE=1 firefox --headless --screenshot "$out" --window-size="${w},${h}" "$url" \
    >/dev/null 2>&1 || true
  [ -s "$out" ] && { echo "$out"; exit 0; }
  echo "firefox produced no image — close the desktop Firefox (snap allows one instance) or use chromium" >&2
fi

echo "no usable headless browser (chromium/firefox) found" >&2
exit 1
