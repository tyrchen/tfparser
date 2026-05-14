variable "name" {
  description = "Identifier prefix for the DB instance"
  type        = string
}

variable "engine_version" {
  description = "PostgreSQL engine version"
  type        = string
  default     = "16.3"
}

variable "instance_class" {
  description = "DB instance class"
  type        = string
  default     = "db.t3.micro"
}

variable "storage_gb" {
  description = "Allocated storage in GB"
  type        = number
  default     = 20
}

variable "subnet_ids" {
  description = "Subnet IDs the DB subnet group will use"
  type        = list(string)
}

variable "vpc_security_group_ids" {
  description = "Security groups attached to the DB instance"
  type        = list(string)
  default     = []
}

variable "multi_az" {
  description = "Whether to enable multi-AZ"
  type        = bool
  default     = false
}

variable "tags" {
  description = "Tags applied to all resources in the module"
  type        = map(string)
  default     = {}
}
