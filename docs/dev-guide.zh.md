# 开发者指南

> *设计文档见 [`./specs/`](../specs/)；最终用户文档见*
> *[`user-guide.zh.md`](./user-guide.zh.md)。English version:*
> *[dev-guide.en.md](./dev-guide.en.md)。*

## 1. 仓库结构

```text
.
├── apps/
│   └── cli/                tfparser — 围绕 tfparser-core 的轻量 CLI
├── crates/
│   └── core/               tfparser-core — 库
│       ├── examples/       端到端示例（可直接运行）
│       └── benches/        criterion 微基准
├── docs/                   指南 + 调研备忘（即当前目录）
├── fixtures/               测试用的合成 TF/Terragrunt workspace
├── specs/                  设计文档（PRD、组件设计等）
└── Makefile                CI 入口（build / test / lint / fuzz / bench）
```

Cargo workspace 成员在顶层 [`Cargo.toml`](../Cargo.toml) 中以 `crates/*`
和 `apps/*` 两个通配符登记。新增 crate 只需放进任一前缀即可，无需修改
工作区清单。

## 2. 库的 façade

`tfparser-core` 提供分层的 API。请优先使用能完成任务的最上层接口。

### 2.1 一行调用

```rust
let workspace = tfparser_core::parse("./my-tf-repo")?;
```

等价于：

```rust
tfparser_core::Parser::builder()
    .workspace_root("./my-tf-repo")
    .build()?
    .parse()?;
```

### 2.2 完整 Builder

```rust
use std::sync::Arc;
use std::path::Path;
use tfparser_core::{Parser, EnvVarMode, ExportOptions};

let parser = Parser::builder()
    .workspace_root("./my-tf-repo")
    .environment("production")
    .default_region("us-west-2")?
    .env_var_mode(EnvVarMode::Passthrough)
    .allow_env("TF_VAR_environment")
    .var("region", "us-east-1")
    .strict_providers(true)
    .max_walk_depth(32)
    .max_file_bytes(8 * 1024 * 1024)
    .build()?;

let export = ExportOptions::builder()
    .out_dir(Arc::<Path>::from(Path::new("./out")))
    .overwrite(true)
    .build();

let (workspace, report) = parser.parse_and_export(&export)?;
```

### 2.3 更底层的接口

如果想替换某个阶段或单独运行某一步，下面的 trait 和默认实现可以直接用：

| 用途                     | 类型 / 函数                                              |
| ------------------------ | -------------------------------------------------------- |
| 在测试里整体替换         | impl [`Pipeline`](../crates/core/src/pipeline.rs)        |
| 单跑文件发现             | [`FsDiscoverer`](../crates/core/src/discovery)           |
| 单跑 HCL loader          | [`HclEditLoader`](../crates/core/src/loader)             |
| 单跑求值器               | [`HclEvaluator`](../crates/core/src/eval)                |
| 单跑 Terragrunt resolver | [`FsTerragruntResolver`](../crates/core/src/terragrunt)  |
| 单跑 provider resolver   | [`DefaultProviderResolver`](../crates/core/src/provider) |
| 单跑 exporter            | [`ParquetExporter`](../crates/core/src/exporter)         |

每个 trait 都是 `Send + Sync` 且返回带具体错误类型的 `Result`，方便测试
里塞 stub。

### 2.4 prelude

```rust
use tfparser_core::prelude::*;
```

囊括常用的 ~14 个名字：`parse`、`Parser`、`ParserBuilder`、`Workspace`、
`Component`、`Module`、`Resource`、`Diagnostic`、`Severity`、`Result`、
`Error`、`ExportOptions`、`ExportReport`、`Exporter`、`ParquetExporter`。

## 3. 运行示例

```sh
# 最简一行
cargo run -p tfparser-core --example parse_one_liner -- ./fixtures/single-component

# 解析 + 写出四张 Parquet 表
cargo run -p tfparser-core --example parse_and_export -- ./fixtures/single-component ./out
```

## 4. 编译 / 测试 / 静态检查

`Makefile` 是单一事实源；CI 跑的就是这些 target：

```sh
make ci          # build + test + fmt-check + clippy -D warnings + cargo doc + cargo deny
make bench       # criterion 微基准（target/criterion/）
make fuzz-hcl-loader   # 10 分钟 fuzz 跑 loader
```

更快的本地循环：

```sh
cargo test -p tfparser-core              # 仅 core 单元/集成测试
cargo test -p tfparser               # CLI 集成测试
cargo test -p tfparser-core --doc        # 文档测试
cargo clippy --workspace --all-targets -- -D warnings
cargo +nightly fmt --all
```

## 5. 工作区 lint 不变量

| Lint                                                              | 原因                                                      |
| ----------------------------------------------------------------- | --------------------------------------------------------- |
| `unsafe_code = forbid`                                            | 健全性合约；禁止 `unsafe`。                               |
| `unwrap_used` / `expect_used` / `panic` / `indexing_slicing` deny | 来自外部输入的可达 panic 一律禁止；测试可按模块 opt-out。 |
| `print_stdout` / `print_stderr` deny                              | 除 CLI / 示例之外用 `tracing`。                           |
| `missing_docs` warn                                               | 所有公共项都需要文档。                                    |
| `pedantic` warn                                                   | 项目偏好；少量 `#[allow]` 必须在代码里附理由。            |

完整说明见 [`./CLAUDE.md`](../CLAUDE.md)。

## 6. 新增 pipeline 阶段

Pipeline 是线性的（见 [`pipeline.rs`](../crates/core/src/pipeline.rs)）：
discovery → loader → projection → terragrunt → evaluator → graph →
provider → （exporter）。新增一步只需三处改动：

1. **trait 定义** — 在对应模块声明一个 `pub trait`，单方法消费上一步输出
   产生下一步输入；要求 `Send + Sync`。对照已有
   trait（例如
   [`TerragruntResolver`](../crates/core/src/terragrunt/mod.rs)）来定形。
2. **默认实现** — 提供一个 `Default<Stage>` 结构，避免 trait 仅存在抽象。
3. **接线** — 在 `DefaultPipeline::run` 内插入正确位置，并把新增选项透传
   到 `PipelineOptions` 与 `ParserBuilder`。

记得同步更新 [`tests/`](../crates/core/tests) 下的集成测试，把 fixture
往返锁住；若新阶段引入新的 Parquet 列，必须先改
[`specs/10-data-model.md`](../specs/10-data-model.md) 再动 schema。

## 7. 错误模型

整库统一：

```rust
type Result<T> = std::result::Result<T, tfparser_core::Error>;
```

`Error` 是 `#[non_exhaustive]`，通过 `#[from]` 包住阶段专属错误
（`Provider`、`Export` 等）。变体是“增”而不“改”——字段重命名是禁忌；非
致命信息走 `Workspace::diagnostics`。

## 8. 性能与可复现

- `make bench` 在 `large-monorepo` fixture 上跑 `criterion`；使用
  `make bench-save-baseline` 保存基线，再用 `make bench-vs-baseline`
  按 10% 回归预算把关。
- 想要字节级稳定的 Parquet，固定 `ExportOptions::parsed_at_ms` 并用
  zstd-3（默认值）。
- `workspace.manifest.json` 携带所有产物的 SHA-256；CI 中用
  `tfparser verify` 检测 schema 或数据漂移。

## 9. 新增文档

新文件丢进 `./docs/`。按
[项目约定](../CLAUDE.md#documentation)，每篇新文档都必须在
[`docs/index.md`](./index.md) 登记。命名约定：

- 面向最终用户 → `docs/<主题>.en.md` + `docs/<主题>.zh.md`
- 设计调研类 → `docs/research/<主题>.md`

需要变更设计的内容请走 `./specs/`，并相应更新
[`specs/index.md`](../specs/index.md)。
