#!/usr/bin/env python3
"""Render the API-docs spike specs as self-contained, browser-openable HTML.

Wraps each committed OpenAPI JSON (docs/api-spike/*-openapi.json) in a RapiDoc page with the spec
*embedded inline*, so the files open directly from file:// — no server, no CORS. The SAME renderer
is used for both, so what differs on screen is the generated spec, not the UI. Also writes an
index.html that shows both side by side.

Run from the repo root:  python3 scripts/render_api_spike.py
"""
import json
import pathlib

HERE = pathlib.Path("docs/api-spike")

PAGE = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <script type="module" src="https://unpkg.com/rapidoc@9.3.4/dist/rapidoc-min.js"></script>
</head>
<body style="margin:0">
  <rapi-doc id="doc" render-style="read" theme="light" show-header="false"
            schema-style="table" allow-try="false" regular-font="-apple-system,Segoe UI,Roboto,sans-serif">
  </rapi-doc>
  <script>
    const spec = __SPEC__;
    customElements.whenDefined('rapi-doc').then(() => document.getElementById('doc').loadSpec(spec));
  </script>
</body>
</html>
"""

INDEX = """<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>API-docs spike — rocket_okapi vs utoipa (side by side)</title>
  <style>
    body {{ margin:0; font-family:-apple-system,Segoe UI,Roboto,sans-serif; }}
    .bar {{ display:flex; background:#222; color:#fff; }}
    .bar div {{ flex:1; padding:8px 14px; font-weight:600; }}
    .bar div:first-child {{ border-right:1px solid #555; }}
    .panes {{ display:flex; height:calc(100vh - 38px); }}
    .panes iframe {{ flex:1; border:0; border-right:1px solid #ccc; height:100%; }}
  </style>
</head>
<body>
  <div class="bar"><div>rocket_okapi (OpenAPI 3.0.0)</div><div>utoipa (OpenAPI 3.0.3)</div></div>
  <div class="panes">
    <iframe src="okapi-docs.html"></iframe>
    <iframe src="utoipa-docs.html"></iframe>
  </div>
</body>
</html>
"""


def render(stem: str, title: str) -> None:
  spec = (HERE / f"{stem}-openapi.json").read_text()
  # Embed the JSON as a JS object literal (valid JS); placeholder avoids brace clashes.
  html = PAGE.format(title=title).replace("__SPEC__", spec)
  (HERE / f"{stem}-docs.html").write_text(html)
  print(f"wrote docs/api-spike/{stem}-docs.html")


render("okapi", "rocket_okapi — CorTeX corpora API")
render("utoipa", "utoipa — CorTeX corpora API")
(HERE / "index.html").write_text(INDEX)
print("wrote docs/api-spike/index.html (side-by-side)")
