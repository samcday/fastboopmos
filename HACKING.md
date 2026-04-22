# hax0rz

## Device templates

Each supported pmOS device is represented by a top-level Jinja2 template:

- `oneplus-enchilada.yaml`
- `oneplus-fajita.yaml`

Templates render a full BootProfile manifest. `fastboopmos` injects runtime
values from `https://images.postmarketos.org/bpo/index.json`:

- `release_name`
- `pmos_device`
- `ui_name`
- `variant`
- `target_name`
- `image_name`
- `image_url`
- `image_sha512`
- `image_size`
- `timestamp`

The same template is rendered once per discovered UI variant for that device.

## Cache model

The allPublic B2 bucket (`samcday-fastboopmos`) is a memoization cache for
compiled hint-bearing `.bootpro` artifacts.

- Key format: `<prefix>/<release>/bootpro/<artifact_sha512>-<scope_hash>.bootpro`
- `scope_hash` is derived from rendered manifest content + fastboop version
- Reads: `fastboopmos` does a plain HTTP GET against the public endpoint; on
  200 the cache entry is reused, on 404 it compiles locally
- Writes: CI pushes newly-compiled entries back to B2 via `aws s3 sync
  --size-only` — credentials only needed for this side

No generated manifests or `.bootpro` files are committed to git.

## Local run

Read-only against the public cache (no AWS credentials needed):

```bash
cargo run -p fastboopmos --release -- \
  --fastboop /path/to/fastboop \
  --output dist/edge.channel
```

Targeted to a single device:

```bash
cargo run -p fastboopmos --release -- \
  --fastboop /path/to/fastboop \
  --only-device oneplus-fajita \
  --output dist/edge.channel
```

`--cache-url` defaults to the public bucket; pass `--cache-url ""` (or set
`FASTBOOPMOS_CACHE_URL=`) to force a cold compile of everything.

## Automation

- `.github/workflows/channel-build.yml`
  - runs on PRs, pushes to `main`, nightly, and manual dispatch
  - supports optional `device` input for targeted runs
  - runs `fastboopmos` to assemble `dist/edge.channel`, then `aws s3 sync`
    pushes any newly-compiled bootpros back to B2
  - uploads workflow artifact every run
  - on `push` to `main` and nightly schedule, updates
    `infra/k8s/fastboopmos/latest.txt` to the new artifact id
