provider "github" {
  owner = var.github_owner
}

data "b2_account_info" "b2" {

}

data "b2_bucket" "rokkitpokkit" {
  bucket_name = var.b2_bucket_name
}

resource "github_actions_secret" "mirror_bucket" {
  repository      = var.github_repository
  secret_name     = "FASTBOOPMOS_MIRROR_BUCKET"
  plaintext_value = var.b2_bucket_name
}

resource "github_actions_secret" "mirror_access_key_id" {
  repository      = var.github_repository
  secret_name     = "FASTBOOPMOS_MIRROR_ACCESS_KEY_ID"
  plaintext_value = var.b2_application_key_id
}

resource "github_actions_secret" "mirror_secret_access_key" {
  repository      = var.github_repository
  secret_name     = "FASTBOOPMOS_MIRROR_SECRET_ACCESS_KEY"
  plaintext_value = var.b2_application_key
}

resource "github_actions_variable" "mirror_endpoint_url" {
  repository    = var.github_repository
  variable_name = "FASTBOOPMOS_MIRROR_ENDPOINT_URL"
  value         = data.b2_account_info.b2.s3_api_url
}

resource "github_actions_variable" "mirror_public_base_url" {
  repository    = var.github_repository
  variable_name = "FASTBOOPMOS_MIRROR_PUBLIC_BASE_URL"
  value         = data.b2_account_info.b2.download_url
}
