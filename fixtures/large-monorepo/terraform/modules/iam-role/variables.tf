variable "name" {
  description = "Role name"
  type        = string
}

variable "trusted_service" {
  description = "AWS service principal allowed to assume the role (e.g. lambda.amazonaws.com)"
  type        = string
  default     = ""
}

variable "trusted_account_ids" {
  description = "Account IDs trusted to assume this role"
  type        = list(string)
  default     = []
}

variable "managed_policy_arns" {
  description = "Managed policies to attach"
  type        = list(string)
  default     = []
}

variable "inline_policies" {
  description = "Map of inline policy name to its JSON document"
  type        = map(string)
  default     = {}
}

variable "tags" {
  description = "Tags applied to the role"
  type        = map(string)
  default     = {}
}
