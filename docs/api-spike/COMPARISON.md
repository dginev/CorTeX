# API-docs auto-generation: `rocket_okapi` vs `utoipa` — head-to-head spike

The plan (Arm 9) was to **compare the actual generated outcomes, not pick on advice**. This is that
spike. Both frameworks annotate the **same slice** — the corpora read endpoints (`GET /api/corpora`
→ `Vec<CorpusDto>`, `GET /api/corpora/{name}` → `CorpusDetailDto` or `404`) — and emit an OpenAPI
spec. Reproduce with:

```bash
cargo run --example api_doc_spike_okapi    # -> docs/api-spike/okapi-openapi.json
# (the utoipa spike example was pruned once rocket_okapi was chosen — see the note at the bottom)
```

Generated specs are committed alongside this file. Source: `examples/api_doc_spike_okapi.rs` (the
utoipa spike's `utoipa-openapi.json` remains here as a frozen comparison artifact).

> **DECIDED + LANDED (2026-06-14):** `rocket_okapi` was chosen and is now **wired into the live app** —
> the generated OpenAPI 3 spec is served at `GET /api/openapi.json` with a RapiDoc page at `GET /api/docs`,
> built from the `#[openapi]`-annotated routes (`src/frontend/apidoc.rs`). The whole agent surface (26
> endpoints, reads + writes) is documented, with the `Actor` token guard as a `CortexToken` ApiKey
> security scheme. The utoipa runner-up (dev-dep + spike example) has been pruned.

## 1. Annotation ergonomics (the per-endpoint cost)

**`rocket_okapi`** — one attribute on the **real route**; method/path/param/body inferred from the
signature:

```rust
#[derive(Serialize, JsonSchema)]                // schemars
struct CorpusDto { name: String, /* … */ }

#[openapi]                                        // <- the only addition
#[get("/api/corpora/<name>")]
fn api_corpus(name: &str) -> Result<Json<CorpusDetailDto>, Status> { … }
```

**`utoipa`** — the operation is **restated** in a macro on a *separate* item (a dummy fn here; the
handler in real use), independent of the Rocket route:

```rust
#[derive(Serialize, ToSchema)]                    // utoipa's own
struct CorpusDto { name: String, /* … */ }

#[utoipa::path(get, path = "/api/corpora/{name}",
  params(("name" = String, Path, description = "Corpus name")),
  responses((status = 200, body = CorpusDetailDto), (status = 404, description = "Unknown corpus")))]
fn api_corpus() {}                                // path/method/params restated by hand

#[derive(OpenApi)]
#[openapi(paths(api_corpora, api_corpus), components(schemas(CorpusDto, CorpusDetailDto, ServiceStatusDto)))]
struct ApiDoc;                                     // + a registry listing every path & schema
```

- **okapi: ~1 line/endpoint, zero duplication** — the spec follows the route, so it can't drift out
  of sync. Fits our symmetry contract (one controller, one DTO, one source of truth) directly.
- **utoipa: the method/path/params/responses are a second source of truth** per endpoint, plus a
  central `ApiDoc` registry to maintain, plus a dummy fn (→ `dead_code`). More to write, more to
  drift, but total control.

## 2. Generated-spec quality (out of the box, same slice)

| | `rocket_okapi` (3.0.0) | `utoipa` (3.0.3) |
|---|---|---|
| Schemas (`CorpusDto` etc.) | ✅ full, field docs from doc-comments | ✅ full, field docs from doc-comments |
| `Vec<CorpusDto>` body | ✅ `array`/`$ref` | ✅ `array`/`$ref` |
| Path-param **description** | ❌ none (no place to put it without more annotation) | ✅ `"Corpus name (external handle)"` |
| `200` response **description** | ❌ empty `""` | ✅ `"Corpus detail"` |
| Error response | ⚠️ generic `"default"`, empty desc (inferred from `Result<_, Status>`) | ✅ explicit `404` `"Unknown corpus"` |
| Operation summary / tags | ❌ (description only) | ✅ summary + tag |

Net: **identical schema quality**; **utoipa's path/response docs are richer by default** precisely
because you hand-write them. okapi is **thinner by default** but can be enriched with extra
annotations (custom responses, `OpenApiResponderInner`) when a given endpoint warrants it.

## 3. Integration & maintenance

- **Rocket-version coupling (decisive for okapi):** `rocket_okapi` is pinned tight to Rocket —
  `rocket_okapi 0.8.0` requires `rocket = "=0.5.0"` and **fails to resolve against our 0.5.1**; only
  `rocket_okapi 0.9.0` works with 0.5.1. So a future Rocket bump is gated on a matching okapi
  release. **`utoipa` is framework-agnostic** — no Rocket dependency at all, upgrades independently.
- **Request guards:** okapi understands Rocket request guards (our `Actor` token guard would need an
  `OpenApiFromRequest` impl to document the security scheme); utoipa has no Rocket awareness, so
  guards/security are described entirely by hand in the macros.
- **Rendered docs page:** okapi ships rocket-native `make_swagger_ui` / `make_rapidoc` (mount and
  go). utoipa has `utoipa-swagger-ui` / `utoipa-rapidoc` / `utoipa-redoc` / `utoipa-scalar` (the
  swagger-ui crate has a `rocket` feature). Both can serve a static page to GH Pages.

## 4. Recommendation

**Lean `rocket_okapi`**, for fit with this codebase:

- The sprint's North Star is *one controller, one DTO, no duplication*. okapi's `#[openapi]` reads
  the real route, so **the spec is generated from the single source of truth** and cannot silently
  drift — exactly the symmetry contract, extended to the docs. With 16+ endpoints, the ~1-line cost
  and zero-duplication property compound.
- The thinner-by-default output is a *known, bounded* cost: add explicit response/param annotations
  only on the endpoints where doc precision matters (e.g. the `404`/`401` on guarded writes).
- Accept the trade: **Rocket-version coupling**. We're on 0.5 (current stable) and `rocket_okapi
  0.9` supports it; a future Rocket major would wait on okapi — a manageable, visible constraint.

**Choose `utoipa` instead if** we expect to swap web frameworks, want the richest spec with no extra
per-endpoint thought, or want to decouple the docs from Rocket's release cadence — at the cost of a
second source of truth per endpoint (drift risk) and hand-documented security.

*Once chosen, prune the loser's dev-dependency + example (both are dev-only today).*
