# hax0rz

## Device templates

Each supported pmOS device is represented by a top-level Jinja2 template:

- `oneplus-enchilada.yaml`
- `oneplus-fajita.yaml`

Templates render a full BootProfile manifest. The script injects runtime values from
`https://images.postmarketos.org/bpo/index.json`:

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

The S3/B2 bucket is a memoization cache for compiled hint-bearing `.bootpro` artifacts.

- Key format: `<prefix>/<release>/bootpro/<artifact_sha512>-<scope_hash>.bootpro`
- `scope_hash` is derived from rendered manifest content + fastboop version
- If key exists: CI downloads and reuses it
- If key is missing: CI compiles from source artifact, uploads once, then reuses

No generated manifests or `.bootpro` files are committed to git.

## Local run (same logic as CI)

```bash
python scripts/build_channel.py \
  --fastboop /path/to/fastboop \
  --cache-bucket your-bucket \
  --cache-endpoint-url https://s3.eu-central-003.backblazeb2.com \
  --cache-prefix fastboopmos \
  --output dist/edge.channel
```

Optional targeted device run:

```bash
python scripts/build_channel.py \
  --fastboop /path/to/fastboop \
  --cache-bucket your-bucket \
  --cache-endpoint-url https://s3.eu-central-003.backblazeb2.com \
  --only-device oneplus-fajita \
  --output dist/edge.channel
```

## Automation

- `.github/workflows/channel-build.yml`
  - runs on PRs, pushes to `main`, nightly, and manual dispatch
  - supports optional `device` input for targeted runs
  - ensures bootpro cache keys exist for selected artifacts
  - assembles `dist/edge.channel` from cached bootpros
  - uploads workflow artifact every run
  - on `push` to `main` and nightly schedule, updates `infra/k8s/fastboopmos/latest.txt` to the new artifact id
