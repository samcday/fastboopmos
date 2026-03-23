variable "github_owner" {
  description = "GitHub organization or user that owns the repository"
  type        = string

  default = "samcday"
}

variable "github_repository" {
  description = "GitHub repository name"
  type        = string
  default = "fastboopmos"
}

variable "b2_application_key_id" {
  type        = string
  description = "Backblaze B2 primary application key ID used by the b2 provider."
  sensitive   = true
}

variable "b2_application_key" {
  type        = string
  description = "Backblaze B2 primary application key used by the b2 provider."
  sensitive   = true
}

variable "b2_bucket_name" {
  type        = string
  description = "Backblaze B2 bucket name for published artifacts."
}
