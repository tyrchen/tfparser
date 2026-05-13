# 16 — Provider / Account / Region Resolver

Status: draft v1 · Owner: tfparser-core · Depends on: [15-resource-graph.md](./15-resource-graph.md)

## 1. Purpose

Given a `Workspace` post-expansion, fill in `account_id`, `account_name`, `region`, `state_account_id`, `state_region` on every resource by walking the **provider alias → provider block → profile/assume-role/cascade → external profile map** chain documented in [multi-account-resolution.md](../docs/research/multi-account-resolution.md).

This is the **last** transformation before the exporter. It is also the only component that depends on out-of-repo information (a `ProfileMap`).

## 2. Interface

```rust
// crates/core/src/provider/mod.rs
pub trait ProviderResolver: Send + Sync {
    fn resolve(&self, ws: &mut Workspace, ctx: &ProviderContext) -> Result<()>;
}

pub struct DefaultProviderResolver;

pub struct ProviderContext {
    pub profile_map:    Arc<ProfileMap>,
    pub default_region: Option<Arc<str>>,
    pub strict:         bool,                     // if true, an unmappable profile is an error
}

pub struct ProfileMap {
    entries: HashMap<Arc<str>, ProfileEntry>,     // key = profile name
}

pub struct ProfileEntry {
    pub account_id:   Arc<str>,
    pub account_name: Arc<str>,                   // human label, may equal account_id
    pub region:       Option<Arc<str>>,
    pub role_arn:     Option<Arc<str>>,
}
```

`ProfileMap` is `Arc`-wrapped and updated via `ArcSwap` if the resolver is re-run; CLAUDE.md § Async & Concurrency calls this out as the right tool for infrequently-updated shared data.

## 3. Profile map loaders

Provided implementations (selected via CLI / config):

### 3.1 `aws_config` loader

Parses `~/.aws/config` (or any path the user supplies). Format is INI-ish with `[profile <name>]` sections. We extract:
- `sso_account_id = "..."` if present.
- `role_arn = "arn:aws:iam::<id>:role/..."` — parse the account ID out of the ARN.
- `region = "..."`.
- `source_profile = "..."` chains: if `account_id` not directly available, follow `source_profile` and copy from the chain (max 8 hops).

Use the [`rust-ini`](https://crates.io/crates/rust-ini) crate. (One more dep, but reliable.) Alternative: hand-roll with `winnow` (per CLAUDE.md § Type Design — winnow is preferred for parsing). For this scope, `rust-ini` saves time and is well-maintained.

### 3.2 `file` loader

User-supplied YAML:

```yaml
profiles:
  main-developer:
    account_id: "370025973162"
    account_name: "primary"
    region: "us-west-2"
  softwaremansion-developer:
    account_id: "999999999999"
    account_name: "softwaremansion"
```

Validated via the `validator` crate per CLAUDE.md § Input Validation: account IDs match `^\d{12}$`, names ≤ 64 bytes char-set-allowlisted.

### 3.3 `none` loader

Empty map. `account_id` / `account_name` columns will be `""`. The parser remains usable end-to-end.

## 4. Resolution algorithm

For each `Resource` in the workspace:

```text
let provider = match resource.provider_ref {
    Some(ref pr) => component.providers.find(pr.alias),
    None         => component.providers.find_default(),
};

let region = first_resolved([
    provider.region_expr.as_literal(),
    component.terragrunt.effective_locals.get("aws_region"),
    workspace.environments.find(component.env).map(|e| e.aws_region),
    ctx.default_region.clone(),
]);

let account_id = first_resolved([
    provider.assume_role.as_ref().and_then(|ar| extract_account_id(&ar.role_arn)),
    provider.profile_expr.as_literal()
        .and_then(|p| ctx.profile_map.lookup(p.as_str()).map(|e| e.account_id.clone())),
    component.terragrunt.effective_locals.get("aws_account_id"),
]);

let account_name = ctx.profile_map.lookup(profile?)?.account_name.clone();

resource.account_id  = account_id.unwrap_or_default();
resource.region      = region.unwrap_or_default();
resource.account_name= account_name.unwrap_or_default();
```

State-account / state-region come from the component's `state_backend`:
- `state_backend.profile` → profile map → account_id.
- `state_backend.role_arn` (if used in S3 backend with `assume_role`) → account_id from ARN.
- `state_backend.region` → directly.

### 4.1 Role-ARN parsing

```rust
fn extract_account_id(role_arn: &str) -> Option<Arc<str>> {
    // arn:aws:iam::123456789012:role/<name>
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^arn:aws:iam::(\d{12}):role/").unwrap());
    re.captures(role_arn).map(|c| Arc::from(c.get(1).unwrap().as_str()))
}
```

The regex is a strict allowlist (`\d{12}`). Anything else returns `None`. Adversarial inputs in the form `arn:...::abc:role/...` return `None` cleanly — no panic, no rejection from regex (linear-time guarantee per `regex` crate).

### 4.2 Provider chain resolution

A subtle case: a component declares `provider "aws" { profile = var.aws_main_profile }` and `var.aws_main_profile` defaulted to `"main-developer"`. After the evaluator phase, `provider.profile_expr` is `Literal(Str("main-developer"))`. Our lookup in `ProfileMap` finds the entry. Account resolved.

If `var.aws_main_profile` had been overridden by a tfvars file to `"other-profile"`, that change flows through the evaluator and the resolver sees the override.

If the profile evaluates to `Unresolved`, we cannot resolve account from profile. We fall through to `cascade_locals.aws_account_id`, which is set by Terragrunt for the whole-component default account.

### 4.3 Module-expanded resources

After [15-resource-graph.md § Expansion](./15-resource-graph.md), an expanded resource carries the *callsite's* `ProviderRef` (the module's `providers = { aws = aws.main }` mapping rewrote it). So the resolver sees `aws.main` and resolves against the parent component's `provider "aws" { alias = "main" }`. **No special-case logic** for modules — the rewrite happens at expansion time, the resolver just walks the same chain.

## 5. Invariants

- **I-PROV-1**: Resolution is **deterministic** given `Workspace` + `ProviderContext`.
- **I-PROV-2**: Unresolvable profile yields `""` in the output (or `Error` if `strict = true`).
- **I-PROV-3**: Account IDs are always exactly 12 digits when non-empty; the resolver validates and rejects any other shape (per CLAUDE.md § Input Validation — allowlist).
- **I-PROV-4**: A resource never gets an account ID from another resource's provider — the chain is strictly per-resource.
- **I-PROV-5**: The resolver writes back into `Workspace` in place, but only the fields it owns (`account_id`, `account_name`, `region`, `state_account_id`, `state_region`). It never mutates expressions, names, or addresses.

## 6. Diagnostics

- `MissingProfileMapping { profile }` — emitted once per distinct unresolved profile (deduplicated). Helps users build out their `profile-map.yaml`.
- `ProviderAliasNotFound { alias, component }` — a resource references `provider = aws.foo` but no provider block with that alias exists. Probable bug in the source repo. Emitted per resource.
- `RoleArnMalformed { role_arn }` — fallthrough; rare.

## 7. Performance

- `ProfileMap.lookup` is `O(1)` via `HashMap`. The resolver does at most 2 lookups per resource (profile, state-profile).
- Resolution is per-component, parallel across components (`rayon`). At reference scale (~40k resources post-expansion): target ≤ **100 ms**.

## 8. Testing

- Synthesised profile map with 5 profiles + 3 components with various alias setups → assert exact `account_id` per resource.
- Role-ARN-only provider (no profile): `assume_role { role_arn = "arn:aws:iam::123456789012:role/x" }` → `account_id = "123456789012"`.
- Profile cascade vs explicit alias: a resource without `provider = aws.<alias>` and with no default provider declared → empty + diagnostic.
- AWS-config loader: a fixture `~/.aws/config` with `sso_account_id` and `role_arn` cases → both extracted.

## 9. CLAUDE.md anchoring

- **Validation**: account ID is a newtype `AccountId(Box<str>)` with private constructor enforcing `^\d{12}$`. Same for `Region` (`^[a-z0-9-]{1,32}$`).
- **Security**: file inputs sandboxed; `~/.aws/config` is read via `tilde` expansion + canonicalisation; size-capped (256 KiB).
- **Concurrency**: `Arc<ProfileMap>`; resolver itself stateless.
- **Errors**: `thiserror`; `MissingProfileMapping` is a `Diagnostic`, not an `Err`.

## 10. Cross-references

- ← Depends on: [15-resource-graph.md](./15-resource-graph.md), [13-evaluator.md](./13-evaluator.md)
- → Consumed by: [20-parquet-exporter.md](./20-parquet-exporter.md)
- ↔ Research: [multi-account-resolution.md](../docs/research/multi-account-resolution.md)
- ↔ Decisions: [99-key-decisions.md](./99-key-decisions.md) — D9 (profile map is external input)
