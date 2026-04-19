# Root Crate: fastboopmos channel builder

## Context

`scripts/build_channel.py` (545 lines) orchestrates building fastboopmos edge channels. It fetches the postmarketOS image index, renders Jinja2 device templates into bootprofile manifests, shells out to the `fastboop` CLI binary to compile them into `.bootpro` files, caches results in B2/S3, and assembles the final `edge.channel` artifact. CI downloads a pre-built `fastboop` binary from GitHub releases.

This plan replaces that with a proper Rust binary crate in the workspace that consumes `fastboop-core` and `fastboop-schema` as library dependencies from crates.io.

## What the Python script does today

### Pipeline stages

1. **Index fetch**: HTTP GET `https://images.postmarketos.org/bpo/index.json` -> JSON with releases, devices, interfaces, images (url, sha512, size, timestamp)
2. **Template discovery**: Glob `*.yaml` from `--templates-dir`, extract `pmos_device` from frontmatter
3. **Rootfs selection**: For each template's `pmos_device`, find matching images in index. Select latest per (ui_name, variant) pair
4. **Manifest rendering**: Jinja2 expands each template with `{release_name, pmos_device, ui_name, variant, target_name, image_name, image_url, image_sha512, image_size, timestamp}` -> YAML bootprofile manifest
5. **Compilation**: `fastboop bootprofile create <manifest> -o <output> --optimize --local-artifact <.img.xz>` per manifest -> `.bootpro` binary
6. **Caching**: Check local cache -> S3 cache -> compile if miss. Upload compiled result to S3. Cache key: `{prefix}/{release}/bootpro/{image_sha512}-{scope_hash}.bootpro` where `scope_hash = sha256(fastboop_version + manifest_content)[:24]`
7. **Assembly**: Concatenate all `.bootpro` files into `dist/edge.channel`

### Device templates

`oneplus-enchilada.yaml` / `oneplus-fajita.yaml` define bootprofiles with three artifact roles from the same pmOS image:
- **rootfs**: ext4 GPT index 1, android_sparseimg format, xz-compressed from HTTP
- **kernel**: fat GPT index 0, extracts `/vmlinuz`
- **dtbs**: fat GPT index 0, extracts `/dtbs`

Plus device-specific stage0 config (kernel cmdline, dtb path, dt overlays).

### CI invocation

`.github/workflows/channel-build.yml`:
- Downloads `fastboop` v0.0.1-rc.15 binary from GitHub releases
- Runs: `python scripts/build_channel.py --fastboop ./bin/fastboop --cache-bucket $BUCKET ...`
- Uploads `dist/edge.channel` + `dist/edge.channel.sha256` as workflow artifacts
- Commits updated artifact ID to `infra/k8s/fastboopmos/latest.txt`

## Proposed Rust rewrite

### Crate structure

Add a root binary crate to the workspace:

```
Cargo.toml  (workspace: members = ["xtask", "fastboopmos"])
fastboopmos/
  Cargo.toml
  src/
    main.rs          # CLI entry (clap)
    index.rs         # pmOS index fetching + parsing
    template.rs      # Template rendering
    compile.rs       # Bootprofile compilation (via fastboop-core)
    cache.rs         # S3/B2 caching
    channel.rs       # Channel assembly
```

### Key dependencies

| Python | Rust replacement |
|--------|-----------------|
| `urllib.request` (index fetch, artifact download) | `reqwest` (already in workspace via frontdoor) |
| `jinja2` (template rendering) | `minijinja` — lightweight, no_std-friendly, Jinja2-compatible |
| `fastboop` CLI binary (bootprofile create) | `fastboop-core` + `fastboop-schema` from crates.io (direct library calls) |
| `subprocess` (aws s3 cp/ls) | `rusty-s3` or `aws-sdk-s3` for B2-compatible S3 |
| `hashlib` (sha256, sha512) | `sha2` (already a dep) |
| `json` | `serde_json` |
| `argparse` | `clap` |

### Consuming fastboop as a library

`fastboop-core` (v0.0.1-rc.15) is published to crates.io and exports:
- `BootProfileManifest` — deserialized from YAML manifest
- `encode_boot_profile()` — compiles manifest to binary `.bootpro`
- `validate_boot_profile()` — validates before encoding
- `encode_channel_pipeline_hints_record()` — pipeline hints sidecar
- `collect_profile_pipeline_hints()` — optimization pass

This means we can call the compilation directly instead of shelling out to `fastboop bootprofile create`. The `--optimize` flag maps to `collect_profile_pipeline_hints()`.

The only thing that might still need a subprocess is DTC compilation (device tree overlays) if any templates use `dt_overlays`. Current templates do reference DT overlays, so we'd either:
- Keep a `dtc` subprocess call for that one step
- Or pull in a Rust DTC library if one exists

### S3/B2 caching

The current script uses `aws` CLI subprocess calls for S3 operations. Options:
- `rusty-s3` — lightweight, minimal deps, S3-compatible API, good for B2
- `aws-sdk-s3` — full AWS SDK, heavier but more complete

`rusty-s3` is probably the right call for this use case (just head/get/put on a B2 bucket).

### Template rendering

`minijinja` is the cleanest Rust Jinja2 implementation. The templates use simple variable substitution (`{{ image_url }}`, `{{ release_name }}`, etc.) — no filters, macros, or inheritance. Could even get away with simple string replacement, but minijinja keeps compatibility with the existing template syntax.

### CI changes

```yaml
# Before:
- run: curl -L ... -o bin/fastboop  # download binary
- run: python scripts/build_channel.py --fastboop ./bin/fastboop ...

# After:
- run: cargo build --release -p fastboopmos
- run: ./target/release/fastboopmos build-channel ...
```

No more Python, no more downloading fastboop binary. The Rust binary has everything baked in.

### Migration strategy

1. Implement the Rust crate with identical CLI interface to the Python script's arguments
2. Run both side-by-side in CI, diff the outputs
3. Once outputs match, remove Python script and update CI workflow
4. Delete `scripts/build_channel.py`

### Open questions

- Does `fastboop-core` export everything needed for the optimize pass, or are some bits CLI-only? Need to check the public API surface.
- DTC compilation: is there a Rust crate for device tree compilation, or do we keep the `dtc` subprocess?
- Should the cache key scheme change, or maintain backwards compatibility with existing S3 cache?
