variable "name" {
  description = "Name prefix for the VPC and its subnets"
  type        = string
}

variable "cidr" {
  description = "Primary IPv4 CIDR block for the VPC"
  type        = string
}

variable "az_count" {
  description = "Number of availability zones to span"
  type        = number
  default     = 2
}

variable "tags" {
  description = "Tags applied to all resources in the module"
  type        = map(string)
  default     = {}
}
