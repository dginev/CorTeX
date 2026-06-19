#!/usr/bin/env bash
#
# build-docs-site.sh — assemble the full CorTeX documentation site for GitHub Pages.
#
# Produces one static directory holding BOTH halves of the published docs:
#   • the Rust library reference  — `cargo doc --no-deps` (the long-standing gh-pages content)
#   • the agent API docs          — the generated OpenAPI 3 spec + a RapiDoc browser page, the
#                                    static twin of the running frontend's `/api/openapi.json`
#                                    and `/api/docs`. The spec is emitted by `cortex openapi`,
#                                    built straight from the `#[openapi]` routes, so it can never
#                                    drift from the served API.
#
# This replaces the dead Travis-CI `doc-upload.sh`; the live deploy is `.github/workflows/docs.yml`
# (force-pushes the output to the `gh-pages` branch on every push to master). Run it locally to
# preview the exact site the workflow publishes:
#
#   scripts/build-docs-site.sh                 # -> target/gh-pages-site
#   scripts/build-docs-site.sh /tmp/site       # custom output dir
#   python3 -m http.server -d target/gh-pages-site 8001   # then browse localhost:8001
#
# Layout of the output directory (served at https://dginev.github.io/CorTeX/):
#   index.html          landing page linking both halves
#   .nojekyll           tell Pages to serve rustdoc's `_`-prefixed assets verbatim
#   cortex/…            the rustdoc crate root (the README "Library docs" badge target)
#   api/index.html      the RapiDoc viewer (the "Agent API" badge target)
#   api/openapi.json    the OpenAPI 3 contract
set -euo pipefail

cd "$(dirname "$0")/.."

OUT="${1:-target/gh-pages-site}"
# RapiDoc is loaded from jsDelivr, major-version-pinned: `@9` always resolves to the latest 9.x
# (the attributes below are stable across 9.x), so the page can't break on a guessed patch version.
RAPIDOC_SRC="https://cdn.jsdelivr.net/npm/rapidoc@9/dist/rapidoc-min.js"

echo "==> 1/4  cargo doc --no-deps (Rust library reference)"
cargo doc --no-deps

echo "==> 2/4  cortex openapi (generated OpenAPI 3 spec — no server/DB needed)"
# Build first, then invoke the binary directly: `cargo run` would interleave Cargo's progress on
# stdout with the JSON. The `openapi` subcommand neither reads config nor connects to a database.
cargo build --bin cortex
SPEC="$(cargo metadata --no-deps --format-version 1 \
  | grep -oE '"target_directory":"[^"]*"' | head -1 | cut -d'"' -f4)/debug/cortex"
"$SPEC" openapi > /tmp/cortex-openapi.json
# Fail loudly rather than publish an empty/half spec (the static docs must match the live API).
python3 -c 'import json,sys; d=json.load(open("/tmp/cortex-openapi.json")); assert d.get("openapi","").startswith("3"), "not an OpenAPI 3 doc"; assert d.get("paths"), "no paths in spec"'

echo "==> 3/4  assemble site at $OUT"
rm -rf "$OUT"
mkdir -p "$OUT/api"
cp -R target/doc/. "$OUT/"
# GitHub Pages runs Jekyll by default, which drops files/dirs beginning with `_` — rustdoc ships
# `search-index*.js` and other assets it needs. `.nojekyll` serves the tree verbatim.
touch "$OUT/.nojekyll"
cp /tmp/cortex-openapi.json "$OUT/api/openapi.json"

# The agent-API browser page: RapiDoc rendering the generated spec. `allow-try="false"` because the
# static site has no live backend to call; this page is documentation, the running frontend's
# `/api/docs` is the interactive surface.
cat > "$OUT/api/index.html" <<HTML
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>CorTeX agent API</title>
  <link rel="icon" href="data:,">
  <script type="module" src="${RAPIDOC_SRC}"></script>
</head>
<body>
  <rapi-doc
    spec-url="openapi.json"
    render-style="read"
    theme="light"
    show-header="false"
    allow-try="false"
    allow-authentication="false"
    use-path-in-nav-bar="true"
    nav-item-spacing="compact"
    schema-style="table"
    primary-color="#2a5d84"
    regular-font="-apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif">
    <div slot="overview" style="font:14px sans-serif;margin:0 0 1rem">
      &larr; <a href="../">CorTeX documentation</a> &middot;
      <a href="../cortex/index.html">Library reference</a> &middot;
      <a href="openapi.json">openapi.json</a>
    </div>
  </rapi-doc>
</body>
</html>
HTML

# The landing page: the gh-pages root, linking both halves of the docs.
cat > "$OUT/index.html" <<'HTML'
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>CorTeX documentation</title>
  <style>
    body { font: 16px/1.5 -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
           max-width: 46rem; margin: 4rem auto; padding: 0 1.25rem; color: #1d2733; }
    h1 { font-size: 1.9rem; margin-bottom: .25rem; }
    p.lead { color: #56636f; margin-top: 0; }
    .cards { display: grid; gap: 1rem; grid-template-columns: 1fr 1fr; margin-top: 2rem; }
    @media (max-width: 34rem) { .cards { grid-template-columns: 1fr; } }
    a.card { display: block; padding: 1.25rem 1.4rem; border: 1px solid #d7dee6; border-radius: 10px;
             text-decoration: none; color: inherit; transition: border-color .15s, box-shadow .15s; }
    a.card:hover { border-color: #2a5d84; box-shadow: 0 2px 10px rgba(42,93,132,.12); }
    a.card h2 { font-size: 1.15rem; margin: 0 0 .35rem; color: #2a5d84; }
    a.card p { margin: 0; color: #56636f; font-size: .95rem; }
    footer { margin-top: 3rem; color: #8a96a3; font-size: .85rem; }
    footer a { color: #56636f; }
  </style>
</head>
<body>
  <h1>CorTeX documentation</h1>
  <p class="lead">A general-purpose processing framework for corpora of scientific documents.</p>
  <div class="cards">
    <a class="card" href="cortex/index.html">
      <h2>Library reference &rarr;</h2>
      <p>The Rust API docs (<code>rustdoc</code>) for the <code>cortex</code> crate &mdash; backend,
         dispatcher, frontend, and models.</p>
    </a>
    <a class="card" href="api/index.html">
      <h2>Agent API &rarr;</h2>
      <p>The OpenAPI&nbsp;3 contract for the HTTP agent API, rendered with RapiDoc &mdash; the machine
         twin of the admin screens.</p>
    </a>
  </div>
  <footer>
    Source: <a href="https://github.com/dginev/CorTeX">github.com/dginev/CorTeX</a>.
    Generated from the <code>master</code> branch.
  </footer>
</body>
</html>
HTML

echo "==> 4/4  done"
echo "    site:        $OUT"
echo "    library:     $OUT/cortex/index.html"
echo "    agent API:   $OUT/api/index.html  (spec: $OUT/api/openapi.json)"
echo "    preview:     python3 -m http.server -d $OUT 8001"
