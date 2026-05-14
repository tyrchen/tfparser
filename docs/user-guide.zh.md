# 用户指南

> *设计文档见 [`./specs/`](../specs/)；面向贡献者的内容见*
> *[`dev-guide.zh.md`](./dev-guide.zh.md)。English version:*
> *[user-guide.en.md](./user-guide.en.md)。*

`tfparser` 在不运行 `terraform plan` 的前提下，把 Terraform / Terragrunt
源码仓库解析为四张规范化的 Parquet 表（`resources`、`dependencies`、
`components`、`modules`）以及一份 SHA-256 校验清单。

## 1. 安装

```sh
# 从源码安装
cargo install --path apps/cli --locked

# 发布后
cargo install tfparser-cli
```

确认安装成功：

```sh
tfparser --version    # tfparser 0.1.0
tfparser --help
```

## 2. 解析一个 workspace

```sh
tfparser parse ./my-tf-repo -o ./out
```

输出目录会按需创建；如果重新跑到同一目录，需要加上 `--overwrite`。

输出大致如下：

```text
✓ wrote 35715 rows (1483403 bytes) in 128 ms
  - ./out/resources.parquet
  - ./out/dependencies.parquet
  - ./out/components.parquet
  - ./out/modules.parquet
  - ./out/workspace.manifest.json
658 diagnostic(s)
```

末尾的 diagnostic 数量是提示信息；使用 `-v` / `-vv` / `-vvv` 提升
verbosity 可以查看每条具体内容，绝大多数是不致命的告警（无法解析的引用、
缺失的 profile-map 条目等）。

## 3. 查询 Parquet 表

任意支持 Arrow 的查询引擎都可以读取这些表，DuckDB 最为顺手。

```sql
-- 哪些 component 资源数最多？
SELECT component_path, resource_count
FROM read_parquet('./out/components.parquet')
ORDER BY resource_count DESC
LIMIT 10;

-- 引用某个安全组的所有规则
SELECT r1.address AS rule, r1.file, r1.line
FROM read_parquet('./out/dependencies.parquet') d
JOIN read_parquet('./out/resources.parquet') r1
  ON d.from_address = r1.address
JOIN read_parquet('./out/resources.parquet') r2
  ON d.to_address   = r2.address
WHERE r2.address = 'aws_security_group.web'
  AND r1.resource_type = 'aws_security_group_rule';
```

完整的列定义在
[`specs/10-data-model.md`](../specs/10-data-model.md)。

## 4. 固定环境与 Terragrunt cascade

很多 Terragrunt 仓库依据 `get_env("TF_VAR_environment")` 切换 cascade。
有两种方式喂给解析器：

```sh
# 方式一：固定 terraform.workspace 与默认 region
tfparser parse ./repo \
  --environment production \
  --region us-west-2 \
  -o ./out

# 方式二：绑定 repo 级别的 Terraform 变量
tfparser parse ./repo \
  --var environment=production \
  --var region=us-west-2 \
  -o ./out
```

默认情况下 env-var 沙箱是 **strict**：除非用 `--allow-env FOO` 显式放行，
否则 `get_env("FOO")` 返回 Unresolved。用 `--env-mode` 切换策略：

| 模式 | 行为 |
| ---- | ---- |
| `strict`（默认） | 未放行的 env 名返回 Unresolved。 |
| `passthrough` | 读取真实进程环境，适合 `TF_VAR_*` 的工作流；环境值可能写入 `attributes_json`，推荐优先使用 strict + 放行列表。 |
| `mock` | `get_env` 始终返回调用方默认值（或 `""`），完全可重现。 |

## 5. AWS profile / 账号解析

若代码里写了 `provider "aws" { profile = "..." }`，提供一份 profile map
就能让 resolver 在每个资源上补全 `account_id` / `region`：

```sh
# YAML profile map（规范 16 § 3.2）
tfparser parse ./repo --profile-map ./profiles.yaml -o ./out

# 或者直接读取 ~/.aws/config（规范 16 § 3.1）
tfparser parse ./repo --aws-config ~/.aws/config -o ./out
```

加上 `--strict-providers` 后，引用了未登记的 profile 会以硬错误（退出码
6）结束，而不是静默忽略。

## 6. 可复现构建

固定 `parsed_at` + 使用确定性压缩，可得到字节级稳定的输出：

```sh
tfparser parse ./repo -o ./out \
  --parsed-at 2026-01-01T00:00:00Z \
  --compression zstd --zstd-level 3
```

manifest 中的 `command_line` 字段会自动遮蔽匹配 `*token*` / `*secret*` /
`*password*` 的参数。

## 7. 校验历史结果

```sh
tfparser verify --dir ./out
# 或者
tfparser verify --manifest ./out/workspace.manifest.json
```

会重新计算每个文件的 SHA-256 并与 manifest 对比，发现差异即非零退出。

## 8. 退出码

参见 [`specs/50-cli.md § 4.3`](../specs/50-cli.md)：

| 退出码 | 类别 |
| ----- | ---- |
| 0 | 成功 |
| 2 | 输入校验错误（flag 值非法） |
| 3 | I/O |
| 4 | 资源限制 / 图构建 |
| 5 | Terragrunt resolver |
| 6 | provider resolver |
| 7 | 导出器 |
| 1 | 其他错误 |

## 9. 在 Rust 里调用

如果想从 Rust 代码里驱动整条 pipeline，详见
[开发者指南](./dev-guide.zh.md)，以及
[`crates/core/examples`](../crates/core/examples) 下可直接运行的示例。

```rust,no_run
let workspace = tfparser_core::parse("./my-tf-repo")?;
println!("{} components", workspace.components.len());
# Ok::<_, tfparser_core::Error>(())
```
