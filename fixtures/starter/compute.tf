data "aws_ami" "amazon_linux" {
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["al2023-ami-*-x86_64"]
  }
}

resource "aws_launch_template" "app" {
  name_prefix   = "${local.name_prefix}-app-"
  image_id      = data.aws_ami.amazon_linux.id
  instance_type = var.instance_type

  iam_instance_profile {
    name = aws_iam_instance_profile.app.name
  }

  vpc_security_group_ids = [aws_security_group.app.id]

  tag_specifications {
    resource_type = "instance"
    tags = {
      Name = "${local.name_prefix}-app"
    }
  }
}

resource "aws_autoscaling_group" "app" {
  name_prefix         = "${local.name_prefix}-app-"
  min_size            = var.instance_count
  max_size            = var.instance_count * 2
  desired_capacity    = var.instance_count
  vpc_zone_identifier = aws_subnet.private[*].id

  launch_template {
    id      = aws_launch_template.app.id
    version = "$Latest"
  }

  tag {
    key                 = "Name"
    value               = "${local.name_prefix}-app"
    propagate_at_launch = true
  }
}
