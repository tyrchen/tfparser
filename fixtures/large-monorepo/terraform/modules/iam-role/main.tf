data "aws_iam_policy_document" "trust" {
  dynamic "statement" {
    for_each = var.trusted_service != "" ? [1] : []
    content {
      actions = ["sts:AssumeRole"]
      principals {
        type        = "Service"
        identifiers = [var.trusted_service]
      }
    }
  }

  dynamic "statement" {
    for_each = length(var.trusted_account_ids) > 0 ? [1] : []
    content {
      actions = ["sts:AssumeRole"]
      principals {
        type        = "AWS"
        identifiers = [for id in var.trusted_account_ids : "arn:aws:iam::${id}:root"]
      }
    }
  }
}

resource "aws_iam_role" "this" {
  name               = var.name
  assume_role_policy = data.aws_iam_policy_document.trust.json

  tags = merge(var.tags, { Name = var.name })
}

resource "aws_iam_role_policy_attachment" "managed" {
  for_each   = toset(var.managed_policy_arns)
  role       = aws_iam_role.this.name
  policy_arn = each.value
}

resource "aws_iam_role_policy" "inline" {
  for_each = var.inline_policies
  name     = each.key
  role     = aws_iam_role.this.id
  policy   = each.value
}
