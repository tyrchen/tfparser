variable "function_name" {
  description = "Lambda function name"
  type        = string
}

variable "handler" {
  description = "Lambda handler"
  type        = string
  default     = "main.handler"
}

variable "runtime" {
  description = "Lambda runtime"
  type        = string
  default     = "python3.12"
}

variable "memory_mb" {
  description = "Memory in MB"
  type        = number
  default     = 256
}

variable "timeout_seconds" {
  description = "Function timeout"
  type        = number
  default     = 30
}

variable "role_arn" {
  description = "IAM role the function assumes"
  type        = string
}

variable "image_uri" {
  description = "Container image URI (if using image package type)"
  type        = string
  default     = ""
}

variable "environment_variables" {
  description = "Environment variables for the function"
  type        = map(string)
  default     = {}
}

variable "tags" {
  description = "Tags applied to the function"
  type        = map(string)
  default     = {}
}
