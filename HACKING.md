# hax0rz

Aight so here's how we gon' do it.

## Device allow-list

`devices.yaml` maps postmarketOS device names to one or more fastboop device profile IDs.

```yaml
oneplus-fajita:
  device_profiles:
    - oneplus-fajita
```

The nightly sync job resolves all available UI variants per allow-listed device from
`https://images.postmarketos.org/bpo/index.json`, writes manifests under `bootprofiles/`, and
compiles optimized `.bootpro` binaries beside each YAML.

When mirror mode is enabled, `index.json` is treated as the complete source of truth for the
target release. The sync will:

- mirror every rootfs artifact for each allow-listed device (excluding `-boot` and `-bootpart`)
- compile optimized hint-bearing `.bootpro` objects for every mirrored rootfs artifact
- purge mirrored rootfs and `.bootpro` objects that are not present in the current desired state

## Local workflow

Generate canonical manifests:

```bash
python scripts/sync_bootprofiles.py
```

Generate manifests and compiled optimized `.bootpro` artifacts:

```bash
python scripts/sync_bootprofiles.py --compile-bootpro --fastboop /path/to/fastboop
```

Run release reconciliation with bucket mirroring + purge:

```bash
python scripts/sync_bootprofiles.py \
  --compile-bootpro \
  --fastboop /path/to/fastboop \
  --mirror-bucket your-bucket \
  --mirror-endpoint-url https://s3.us-west-000.backblazeb2.com \
  --mirror-region us-west-000 \
  --mirror-prefix fastboopmos \
  --mirror-public-base-url https://cdn.example.com
```

Mirror mode uses an S3-compatible API (including Backblaze B2's S3 endpoint), so ensure
`aws` CLI credentials are available in the environment (`AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`).

Build a channel from committed `.bootpro` files (concatenated stream):

```bash
python scripts/build_channel.py
```

## Automation

- `.github/workflows/nightly-sync.yml`
  - runs nightly
  - refreshes `bootprofiles/*.yaml`
  - compiles optimized `bootprofiles/*.bootpro`
  - opens one PR per changed `bootprofiles/<device>/` subtree
- `.github/workflows/publish-channel.yml`
  - runs on pull requests and on `main`
  - concatenates committed `.bootpro` artifacts into `edge.channel`
  - uploads workflow artifacts for CI/debugging
  - on `main`, creates `edge-YYYYMMDDhhmmss` release tags and attaches channel assets
