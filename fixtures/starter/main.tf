data "aws_caller_identity" "current" {}

data "aws_availability_zones" "available" {
  state = "available"
}

resource "random_id" "suffix" {
  byte_length = 4
}
