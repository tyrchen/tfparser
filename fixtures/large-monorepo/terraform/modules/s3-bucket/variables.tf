variable "name" {
  description = "Bucket name (must be globally unique)"
  type        = string
}

variable "versioning" {
  description = "Enable object versioning"
  type        = bool
  default     = true
}

variable "lifecycle_days" {
  description = "Days after which non-current versions expire; 0 disables the rule"
  type        = number
  default     = 0
}

variable "kms_key_arn" {
  description = "KMS key ARN for SSE-KMS encryption; empty for SSE-S3"
  type        = string
  default     = ""
}

variable "tags" {
  description = "Tags applied to the bucket"
  type        = map(string)
  default     = {}
}
