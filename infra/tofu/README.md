# OpenTofu: GitHub Actions mirror settings

This module manages the GitHub Actions secrets/variables required by
`.github/workflows/nightly-sync.yml`.

Managed GitHub Actions secrets:

- `FASTBOOPMOS_MIRROR_BUCKET`
- `FASTBOOPMOS_MIRROR_ACCESS_KEY_ID`
- `FASTBOOPMOS_MIRROR_SECRET_ACCESS_KEY`
- `FASTBOOPMOS_MIRROR_ENDPOINT_URL`
- `FASTBOOPMOS_MIRROR_PUBLIC_BASE_URL`

Required Terraform/OpenTofu input variables:

- `github_owner`
- `github_repository`
- `mirror_bucket`
- `mirror_access_key_id`
- `mirror_secret_access_key`
- `mirror_endpoint_url`
- `mirror_public_base_url`

## tf-controller inputs

The `Terraform` CR at `infra/k8s/fastboopmos/github-actions-secrets-terraform.yaml`
expects these refs to exist:

- `ConfigMap/fastboopmos-tofu-vars`
- `Secret/fastboopmos-tofu-vars-secret`
- `Secret/fastboopmos-tofu-env` (via `envFrom`)

Expected keys for `varsFrom` (as Terraform variable names):

- `github_owner`
- `github_repository`
- `mirror_bucket`
- `mirror_access_key_id`
- `mirror_secret_access_key`
- `mirror_endpoint_url`
- `mirror_public_base_url`
- `mirror_prefix` (optional)

Expected keys for `envFrom`:

- `GITHUB_TOKEN`
