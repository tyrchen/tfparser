# User Guide

> *Looking for the design specs? See [`./specs/`](../specs/). Looking for*
> *contributor docs? See [`dev-guide.en.md`](./dev-guide.en.md). 中文版：*
> *[user-guide.zh.md](./user-guide.zh.md).*

`tfparser` reads a Terraform / Terragrunt source repository and emits four
canonical Parquet tables (`resources`, `dependencies`, `components`,
`modules`) plus a SHA-256 manifest, without running `terraform plan`.

## 1. Install

```sh
# from a checkout
cargo install --path apps/cli --locked

# once published
cargo install tfparser
```

Verify:

```sh
tfparser --version    # tfparser 0.1.0
tfparser --help
```

## 2. Parse a workspace

```sh
tfparser parse ./my-tf-repo -o ./out
```

The output directory is created on demand. Re-running into the same
directory needs `--overwrite`.

Output:

```text
✓ wrote 35715 rows (1483403 bytes) in 128 ms
  - ./out/resources.parquet
  - ./out/dependencies.parquet
  - ./out/components.parquet
  - ./out/modules.parquet
  - ./out/workspace.manifest.json
658 diagnostic(s)
```

The diagnostic count is informational. Increase verbosity with `-v` /
`-vv` / `-vvv` to see each diagnostic — most are non-fatal warnings
(unresolvable references, missing profile-map entries).

## 3. Query the tables

Any Arrow-compatible engine works; DuckDB is the most ergonomic.

```sql
-- which components hold the most resources?
SELECT component_path, resource_count
FROM read_parquet('./out/components.parquet')
ORDER BY resource_count DESC
LIMIT 10;

-- which security-group rules reference a given SG?
SELECT r1.address AS rule, r1.file, r1.line
FROM read_parquet('./out/dependencies.parquet') d
JOIN read_parquet('./out/resources.parquet') r1
  ON d.from_address = r1.address
JOIN read_parquet('./out/resources.parquet') r2
  ON d.to_address   = r2.address
WHERE r2.address = 'aws_security_group.web'
  AND r1.resource_type = 'aws_security_group_rule';
```

The full column layouts are in
[`specs/10-data-model.md`](../specs/10-data-model.md).

## 4. Pinning the environment + cascade

Terragrunt repos typically branch their cascade on
`get_env("TF_VAR_environment")`. Two ways to surface the right values:

```sh
# 1. Pin terraform.workspace + a default region
tfparser parse ./repo \
  --environment production \
  --region us-west-2 \
  -o ./out

# 2. Bind repo-level Terraform variables
tfparser parse ./repo \
  --var environment=production \
  --var region=us-west-2 \
  -o ./out
```

By default the env-var sandbox is **strict** — `get_env("FOO")` returns
`Unresolved` unless `--allow-env FOO` opts the name in. Switch the policy
with `--env-mode`:

| Mode               | Behaviour                                                                                                                                                     |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `strict` (default) | `get_env` returns Unresolved unless allowlisted.                                                                                                              |
| `passthrough`      | `get_env` reads the actual process env. Useful for `TF_VAR_*`-shaped workflows; data may leak into `attributes_json`, so prefer the strict + allowlist combo. |
| `mock`             | `get_env` always returns the caller's default (or `""`). Reproducible / hermetic.                                                                             |

## 5. AWS profile / account resolution

If your code calls `provider "aws" { profile = "..." }`, supply a profile
map so the resolver can fill `account_id` / `region` on every resource:

```sh
# YAML profile map (spec 16 § 3.2)
tfparser parse ./repo --profile-map ./profiles.yaml -o ./out

# or directly from ~/.aws/config (spec 16 § 3.1)
tfparser parse ./repo --aws-config ~/.aws/config -o ./out
```

`--strict-providers` upgrades any missing profile reference to a hard
diagnostic (exit code 6) instead of a silent fallback.

## 6. Reproducible builds

Pin `parsed_at` and use deterministic compression for byte-stable output:

```sh
tfparser parse ./repo -o ./out \
  --parsed-at 2026-01-01T00:00:00Z \
  --compression zstd --zstd-level 3
```

The manifest's `command_line` field redacts arguments matching
`*token*` / `*secret*` / `*password*` automatically.

## 7. Verify a prior run

```sh
tfparser verify --dir ./out
# or
tfparser verify --manifest ./out/workspace.manifest.json
```

Each entry's SHA-256 is recomputed from disk and matched against the
manifest. Drift exits non-zero.

## 8. Exit codes

Per [`specs/50-cli.md § 4.3`](../specs/50-cli.md):

| Code | Class                             |
| ---- | --------------------------------- |
| 0    | success                           |
| 2    | validation error (bad flag value) |
| 3    | I/O                               |
| 4    | resource limit / graph build      |
| 5    | Terragrunt resolver               |
| 6    | provider resolver                 |
| 7    | exporter                          |
| 1    | anything else                     |

## 9. Library use

If you'd rather drive the pipeline from Rust code, see the
[developer guide](./dev-guide.en.md) and the runnable examples under
[`crates/core/examples`](../crates/core/examples).

```rust,no_run
let workspace = tfparser_core::parse("./my-tf-repo")?;
println!("{} components", workspace.components.len());
# Ok::<_, tfparser_core::Error>(())
```
