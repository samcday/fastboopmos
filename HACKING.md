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

## CLI

`fastboopmos` exposes three subcommands:

- `list` — emit a JSON array of `{device, ui}` entries (drives the GHA
  matrix; also handy for piping through `jq` locally).
- `build --device <id> [--ui <name>]` — compile bootpros for one device into
  `--bootpro-cache-dir` (default `build/pmos-bootpros`). Hits the local cache
  first, then the HTTP cache, otherwise downloads the rootfs and compiles.
- `channel [--device <id>] [--ui <name>]` — assemble the indexed channel
  from cached bootpros. **Cache-only**: errors out if any bootpro is missing
  from local + HTTP cache instead of triggering a multi-GB rootfs download.

## Local run

Build all bootpros (read-only against the public cache, no AWS creds):

```bash
for d in *.yaml; do
  ./tools/cargo-local.sh run -p fastboopmos --release -- \
    build --device "${d%.yaml}"
done
./tools/cargo-local.sh run -p fastboopmos --release -- \
  channel --output dist/edge.channel
```

Targeted to a single device + UI:

```bash
./tools/cargo-local.sh run -p fastboopmos --release -- \
  build --device oneplus-fajita --ui phosh
./tools/cargo-local.sh run -p fastboopmos --release -- \
  channel --device oneplus-fajita --output dist/edge.channel
```

`./tools/cargo-local.sh` prefers crates from a local `./fastboop` checkout by
emitting a temporary `[patch.crates-io]` overlay for the fastboop crates used by
fastboopmos. If `./fastboop` is absent, it falls back to normal crates.io resolution.

`--cache-url` defaults to the public bucket; pass `--cache-url ""` (or set
`FASTBOOPMOS_CACHE_URL=`) to disable HTTP cache lookups (forces a cold
compile under `build`; makes `channel` succeed only against the local cache
dir).

## Automation

- `.github/workflows/channel-build.yml` runs on PRs, pushes to `main`,
  nightly, and manual dispatch with an optional `device` input.

  Three jobs:

  1. **`prepare`** — builds the `fastboopmos` release binary, runs
     `fastboopmos list` to compute the build matrix (filtered by the
     workflow_dispatch `device` input if present), and uploads the binary as
     a workflow artifact so downstream jobs share an identical build.
  2. **`bootpro`** — matrix fan-out: one job per `(device, ui)` pair, with
     `fail-fast: false` and `max-parallel: 4` to bound concurrent rootfs
     downloads. Each slot runs `fastboopmos build` then `aws s3 sync`s the
     freshly-compiled bootpros to B2. The upload runs under
     `if: always() && hashFiles(...)` so partial-progress failures still
     persist whatever they did compile.
  3. **`channel`** — needs `bootpro`. Runs `fastboopmos channel` (cache-only)
     to assemble `dist/edge.channel`, uploads the workflow artifact, and on
     `push`/`schedule` updates `infra/k8s/fastboopmos/latest.txt`.

  A failed `bootpro` slot blocks the `channel` job, but bootpros from the
  successful slots are now cached for the next workflow run — re-running
  picks up where the failure left off instead of re-downloading every
  rootfs.
