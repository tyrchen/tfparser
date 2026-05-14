terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
  backend "s3" {
    bucket = "my-tf-state"
    key    = "multi-provider/terraform.tfstate"
    region = "us-east-1"
  }
}

provider "aws" {
  alias  = "main"
  region = "us-east-1"
}

provider "aws" {
  alias  = "backup"
  region = "us-west-2"
}

resource "aws_s3_bucket" "primary" {
  provider = aws.main
  bucket   = "northwind-primary-${random_id.suffix.hex}"
}

resource "aws_s3_bucket" "secondary" {
  provider = aws.backup
  bucket   = "northwind-secondary-${random_id.suffix.hex}"
}

resource "random_id" "suffix" {
  byte_length = 4
}
