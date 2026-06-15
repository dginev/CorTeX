#!/usr/bin/env python3
"""Headless screenshot via Playwright + the system Google Chrome (channel="chrome").

Why channel="chrome": Playwright can't *download* a browser on very new/unsupported distros (e.g.
Ubuntu 26.04), but it can drive a system-installed Chrome. Install once:
    sudo apt-get install -y python3.14-venv
    python3 -m venv .venv && .venv/bin/pip install playwright
    # + a system Google Chrome (apt install ./google-chrome-stable_current_amd64.deb)

Run with the repo venv:
    .venv/bin/python scripts/screenshot.py <url> <out.png> [width] [height] [--full]

Tip: append ?theme=paper or ?theme=midnight to the URL to force a theme.
"""

import sys


def main() -> int:
    from playwright.sync_api import sync_playwright

    args = [a for a in sys.argv[1:] if a != "--full"]
    full = "--full" in sys.argv[1:]
    if len(args) < 2:
        print("usage: screenshot.py <url> <out.png> [width] [height] [--full]", file=sys.stderr)
        return 2
    url, out = args[0], args[1]
    width = int(args[2]) if len(args) > 2 else 1440
    height = int(args[3]) if len(args) > 3 else 1200

    with sync_playwright() as pw:
        browser = pw.chromium.launch(channel="chrome", chromium_sandbox=False, args=["--no-sandbox"])
        page = browser.new_page(
            viewport={"width": width, "height": height}, device_scale_factor=2
        )
        page.goto(url, wait_until="networkidle", timeout=45000)
        page.screenshot(path=out, full_page=full)
        browser.close()
    print(out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
