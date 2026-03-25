# OpenTofu: GitHub Actions cache settings

This module manages the GitHub Actions secrets/variables required by
`.github/workflows/channel-build.yml`.

Managed GitHub Actions secrets:

- `FASTBOOPMOS_CACHE_BUCKET`
- `FASTBOOPMOS_CACHE_ACCESS_KEY_ID`
- `FASTBOOPMOS_CACHE_SECRET_ACCESS_KEY`

Managed GitHub Actions variables:

- `FASTBOOPMOS_CACHE_ENDPOINT_URL`
- `FASTBOOPMOS_CACHE_PREFIX`

These values back the bootpro memoization cache used by
`.github/workflows/channel-build.yml`.

Required Terraform/OpenTofu input variables:

- `github_owner`
- `github_repository`
- `b2_application_key_id`
- `b2_application_key`
- `b2_bucket_name`
- `cache_prefix` (optional, default `fastboopmos`)

## tf-controller inputs

The `Terraform` CR at `infra/k8s/fastboopmos/tofu.yaml`
expects these refs to exist:

- `Secret/tofu-bucket-vars`
- `Secret/tofu-env` (via `envFrom`)

Expected keys for `varsFrom` (as Terraform variable names):

- `github_owner`
- `github_repository`
- `b2_application_key_id`
- `b2_application_key`
- `b2_bucket_name`
- `cache_prefix` (optional)

Expected keys for `envFrom`:

- `GITHUB_TOKEN`
