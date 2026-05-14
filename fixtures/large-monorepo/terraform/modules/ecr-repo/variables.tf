variable "repository_name" {
  description = "ECR repository name"
  type        = string
}

variable "image_tag_mutability" {
  description = "MUTABLE or IMMUTABLE"
  type        = string
  default     = "IMMUTABLE"
}

variable "scan_on_push" {
  description = "Whether ECR scans images on push"
  type        = bool
  default     = true
}

variable "tags" {
  description = "Tags applied to the repository"
  type        = map(string)
  default     = {}
}
