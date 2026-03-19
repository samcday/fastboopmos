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
`https://images.postmarketos.org/bpo/index.json` and writes manifests under `bootprofiles/`.

## Local workflow

Generate canonical manifests:

```bash
python scripts/sync_bootprofiles.py
```

Build a compiled channel (concatenated `.bootpro` stream):

```bash
python scripts/build_channel.py --fastboop /path/to/fastboop
```

## Automation

- `.github/workflows/nightly-sync.yml`
  - runs nightly
  - refreshes `bootprofiles/`
  - opens one PR per changed `bootprofiles/<device>/` subtree
- `.github/workflows/publish-channel.yml`
  - runs on pull requests and on `main`
  - compiles all manifests into `edge.channel`
  - uploads workflow artifacts for CI/debugging
  - on `main`, creates `edge-YYYYMMDDhhmmss` release tags and attaches channel assets
