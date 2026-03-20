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

## Local workflow

Generate canonical manifests:

```bash
python scripts/sync_bootprofiles.py
```

Generate manifests and compiled optimized `.bootpro` artifacts:

```bash
python scripts/sync_bootprofiles.py --compile-bootpro --fastboop /path/to/fastboop
```

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
