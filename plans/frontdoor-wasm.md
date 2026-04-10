# Frontdoor: composable wasm-wasip2 refactor

## Context

The fastboopmos frontdoor (`infra/frontdoor/src/main.rs`, ~1036 lines) is a monolithic Axum binary. The sister project fastboop has a composable architecture: `frontdoor-core` (no_std, shared logic) + `frontdoor-edge` (wstd/WASI component). We want to move in the same direction — split the frontdoor into portable core logic and target-specific HTTP frontends, with wasm-wasip2 as the primary deployment target (via wasmCloud).

## Current frontdoor architecture

### Routes

| Route | Method | Purpose |
|-------|--------|---------|
| `/healthz` | GET | Health check, returns "ok" |
| `/gha/{run_id}` | GET, HEAD, OPTIONS | GitHub Actions artifact proxy with caching |
| `/edge.channel` | GET, HEAD, OPTIONS | Serve edge channel release with range requests |
| `/edge.channel.sha256` | GET, HEAD, OPTIONS | SHA256 checksum of edge channel |
| `/__fastboopmos/live` | GET, OPTIONS | Current artifact ID |

### Logic breakdown: core vs. axum-specific

**Portable core logic** (no HTTP framework dependency):
- `parse_single_byte_range(header, size) -> (start, end)` — range request parsing
- `freespace_pct(path) -> f64` — disk free space via libc statvfs
- Cache eviction — LRU by mtime, scan .blob files, delete oldest until free space threshold met
- Cache key computation — `sha256("{owner}/{repo}:{run_id}")`
- ZIP extraction — single file from ZIP archive, SHA256 hash during extraction
- Config struct — env var parsing
- GHA API types — artifact list response, pagination
- CORS origin validation — hardcoded allowed origins list

**Axum-specific** (HTTP framework glue):
- Router setup with path extractors
- `State(Arc<AppState>)` injection
- `Body::from_stream()` + `ReaderStream` for streaming responses
- `Response::builder()` construction
- Tokio async filesystem operations
- Per-run_id lock management via `Mutex<HashMap<String, Arc<Mutex<()>>>>`

### Cache structure

```
{cache_dir}/
  gha/
    {sha256(owner/repo:run_id)}.blob   # artifact binary
    {sha256(owner/repo:run_id)}.json   # metadata: {size, content_type, etag}
  release/
    {artifact_id}.blob                  # channel file
    {artifact_id}.json                  # metadata
```

Eviction: scan both dirs for `.blob` files, sort by mtime, delete oldest until `MIN_FREESPACE_PCT` satisfied.

## Proposed architecture

### Crate layout

```
infra/frontdoor/
  Cargo.toml              # workspace: members = ["crates/*"]
  rust-toolchain.toml     # pin Rust version + wasm32-wasip2 target
  crates/
    frontdoor-core/       # no_std + alloc, portable logic
      Cargo.toml
      src/
        lib.rs
        range.rs          # parse_single_byte_range()
        cache_key.rs      # SHA256 cache key computation
        config.rs         # Config struct + env parsing
        gha.rs            # GHA API types (serde structs)
        cors.rs           # Origin validation
    frontdoor-edge/       # wstd/WASI component (primary target)
      Cargo.toml          # crate-type = ["cdylib"], depends on frontdoor-core
      wit/
        world.wit         # WIT component interface
      src/
        lib.rs            # wstd HTTP handler, routes, caching, streaming
```

### What goes in frontdoor-core

Following the fastboop pattern, core should be `#![no_std]` with `extern crate alloc`:

| Module | Contents | Dependencies |
|--------|----------|-------------|
| `range.rs` | `parse_single_byte_range()`, `RangeError` enum | none |
| `cache_key.rs` | `cache_key()` -> `[u8; 32]`, `hex_encode()` | `sha2` (no_std) |
| `config.rs` | `Config` struct, field types | none (no env parsing in no_std) |
| `gha.rs` | `ArtifactListResponse`, `Artifact` serde types | `serde` (no_std) |
| `cors.rs` | `is_allowed_origin()`, origin list | none |

Things that do NOT go in core:
- Filesystem operations (cache eviction, file I/O) — these use platform-specific APIs
- HTTP client calls — framework-specific
- ZIP extraction — depends on I/O traits
- Lock management — runtime-specific

### frontdoor-edge (wstd) implementation

The WASI component would:
- Use `wstd::http_server` macro for the HTTP handler
- Use `wstd::http::Client` for outbound HTTP (GHA API, artifact downloads)
- Use std sync filesystem for cache I/O (WASI provides filesystem)
- Import `frontdoor-core` for range parsing, cache keys, config types, CORS

Key adaptation challenges vs. current axum version:
1. **No tokio**: All I/O is synchronous in WASI. The current code uses `tokio::fs`, `tokio::sync::Mutex`, `ReaderStream`, etc. These all need to be replaced with sync equivalents.
2. **No reqwest**: HTTP client is `wstd::http::Client`. The GHA API calls need to be rewritten.
3. **No libc statvfs**: Disk free space detection needs a different approach in WASI. Could track cache size manually or use a simpler eviction strategy (max total size instead of free space percentage).
4. **Streaming responses**: `wstd::http::Body` handles this differently from axum's `Body::from_stream()`.

### Optional: axum native target

Could also maintain an axum-based crate for native deployment:

```
crates/
  frontdoor-native/     # axum binary (optional, for testing/native deploy)
    Cargo.toml
    src/main.rs
```

This would use frontdoor-core for shared logic but keep axum for HTTP handling. Useful for local dev (current `cargo xtask frontdoor-dev` pattern) and as a fallback if wasmCloud isn't available.

### Deployment changes

Current: Docker container (debian:bookworm-slim) -> K8s Deployment

Target: wasmCloud WorkloadDeployment (like fastboop's frontdoor-edge)

```yaml
apiVersion: core.oam.dev/v1beta1
kind: Application
metadata:
  name: frontdoor-edge
spec:
  components:
    - name: frontdoor
      type: component
      properties:
        image: ghcr.io/samcday/fastboopmos-frontdoor-edge:latest
      traits:
        - type: spreadscaler
          properties:
            instances: 2
        - type: link
          properties:
            target: httpserver
            ...
```

CI would change from Docker build to:
1. `cargo build --release --target wasm32-wasip2 -p frontdoor-edge`
2. `wash push ghcr.io/samcday/fastboopmos-frontdoor-edge:$TAG frontdoor_edge.wasm`

### xtask frontdoor-dev update

Once refactored, `cargo xtask frontdoor-dev` changes from running native binary to:

```rust
// Build wasm component
cargo build --target wasm32-wasip2 -p frontdoor-edge

// Run via wasmtime
wasmtime serve --wasi cli --wasi http --addr 127.0.0.1:38080 \
  --env EDGE_CHANNEL_ARTIFACT_ID=... \
  --dir $CACHE_DIR::/cache \
  frontdoor_edge.wasm
```

Same pattern as fastboop's xtask.

### Migration strategy

1. Create the `infra/frontdoor/` workspace with `frontdoor-core` crate
2. Extract portable logic from `main.rs` into core (range, cache_key, types)
3. Create `frontdoor-edge` crate targeting wasm32-wasip2
4. Port route handlers one at a time: `/healthz` first (trivial), then `/__fastboopmos/live`, then `/edge.channel`, then `/gha/{run_id}`
5. Test with `wasmtime serve` locally
6. Set up wasmCloud deployment alongside existing K8s deployment
7. Switch DNS/routing to wasmCloud
8. Remove Docker-based deployment
9. Update xtask to use wasm target

### Open questions

- Do we need the axum native target, or is wasm-only fine? (fastboop is wasm-only for frontdoor-edge)
- How to handle disk free space detection in WASI? Options: manual size tracking, fixed max cache size, or WASI filesystem capacity API if available
- Should the GHA proxy route stay in the frontdoor, or should it be a separate wasmCloud component?
- wasmCloud networking: does the WASI component need explicit network access grants for github.com / objects.githubusercontent.com?
