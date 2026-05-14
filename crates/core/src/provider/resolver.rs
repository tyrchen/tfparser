//! `DefaultProviderResolver` — the last fill phase before the exporter.
//!
//! Per [16-provider-resolver.md § 4]. The resolver walks each component,
//! resolves `provider = aws.<alias>` references to a [`ProviderBlock`], and
//! for every [`Resource`] populates `account_id`, `account_name`, and
//! `region`. It also fills `state_account_id` / `state_region` on the
//! component's [`StateBackend`].
//!
//! Resolution is **deterministic** given `(Workspace, ProviderContext)` —
//! I-PROV-1. The resolver is `Send + Sync`; mutation is performed on the
//! borrowed `Workspace` in place — I-PROV-5 (the resolver writes only the
//! fields it owns).
//!
//! [16-provider-resolver.md § 4]: ../../../specs/16-provider-resolver.md

use std::{
    collections::{BTreeSet, HashSet},
    sync::Arc,
};

use regex::Regex;

use crate::{
    Result,
    diagnostic::{Diagnostic, Severity},
    ir::{
        AccountId, AssumeRole, Component, Expression, ProviderBlock, ProviderRef, Region,
        StateBackend, Value, Workspace,
    },
    provider::{
        error::ProviderError,
        profile_map::{ProfileEntry, ProfileMap},
    },
};

/// Per-resolution context — `Arc<ProfileMap>` plus operator-driven flags.
///
/// Per spec § 2.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ProviderContext {
    /// Profile-name → account/region/role map.
    pub profile_map: Arc<ProfileMap>,
    /// Default region used when no other resolution succeeds. CLI binds
    /// `--region` here; otherwise `None`.
    pub default_region: Option<Region>,
    /// When `true`, any profile referenced from source that is **not** in
    /// the profile map raises [`ProviderError::StrictUnresolved`] from
    /// [`DefaultProviderResolver::resolve`]; otherwise we emit a single
    /// diagnostic per distinct profile (I-PROV-2).
    pub strict: bool,
}

impl ProviderContext {
    /// Construct a context with the spec defaults (`strict = false`,
    /// no default region).
    #[must_use]
    pub fn new(profile_map: Arc<ProfileMap>) -> Self {
        Self {
            profile_map,
            default_region: None,
            strict: false,
        }
    }
}

/// Trait the orchestrator (Phase 9 CLI) calls. Phase 7 ships exactly one
/// implementation, [`DefaultProviderResolver`]; downstream tests may
/// swap in a stub.
pub trait ProviderResolver: Send + Sync + std::fmt::Debug {
    /// Fill `account_id` / `account_name` / `region` (resources) and
    /// `state_account_id` / `state_region` (state backends) per spec § 4.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::StrictUnresolved`] only when
    /// `ctx.strict == true` and at least one profile cannot be mapped.
    /// All other anomalies (alias-not-found, malformed role ARN) attach
    /// to `ws.diagnostics` and the call returns `Ok(())`.
    fn resolve(&self, ws: &mut Workspace, ctx: &ProviderContext) -> Result<()>;
}

/// Default implementation per spec § 4. Stateless.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultProviderResolver;

impl DefaultProviderResolver {
    /// Construct a resolver. Free-standing convenience over `default()`.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ProviderResolver for DefaultProviderResolver {
    fn resolve(&self, ws: &mut Workspace, ctx: &ProviderContext) -> Result<()> {
        // Workspace-scoped deduplication: spec § 6 — emit one
        // `MissingProfileMapping` per distinct unresolved profile name,
        // not per resource site.
        let mut unresolved: BTreeSet<Arc<str>> = BTreeSet::new();
        let mut new_diags: Vec<Diagnostic> = Vec::new();

        for component in &mut ws.components {
            resolve_component(component, ctx, &mut unresolved, &mut new_diags);
        }

        // Emit one diagnostic per distinct unresolved profile.
        for profile in &unresolved {
            new_diags.push(
                Diagnostic::new(
                    Severity::Warn,
                    "TF1601",
                    format!("missing profile-map entry for `{profile}`"),
                )
                .with_suggestion(Arc::<str>::from(
                    "add the profile to your profile-map.yaml or to ~/.aws/config",
                )),
            );
        }

        // Strict mode short-circuits — the workspace is in a consistent
        // post-resolve state, but the operator asked us to fail.
        if ctx.strict && !unresolved.is_empty() {
            let first = unresolved
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| Arc::from("<unknown>"));
            // Diagnostics already attached; surface the strict-mode
            // failure separately so the caller can branch.
            ws.diagnostics.extend(new_diags);
            return Err(ProviderError::StrictUnresolved {
                count: unresolved.len(),
                first,
            }
            .into());
        }
        ws.diagnostics.extend(new_diags);
        Ok(())
    }
}

/// Component-level resolution. Iterates resources once.
fn resolve_component(
    component: &mut Component,
    ctx: &ProviderContext,
    unresolved: &mut BTreeSet<Arc<str>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Pre-index providers by alias (and default = `None`) so per-resource
    // lookups are O(1).
    let providers: Vec<ProviderBlock> = component.providers.clone();
    let mut alias_not_found_emitted: HashSet<Arc<str>> = HashSet::new();

    let cascade_region_v: Option<Region> = component
        .terragrunt
        .as_ref()
        .and_then(cascade_region)
        .or_else(|| ctx.default_region.clone());

    let cascade_account_v: Option<AccountId> =
        component.terragrunt.as_ref().and_then(cascade_account);

    for resource in &mut component.resources {
        let provider = pick_provider(&providers, resource.provider_ref.as_ref());

        if let Some(local) = resource
            .provider_ref
            .as_ref()
            .filter(|_| provider.is_none())
            .map(|r| Arc::clone(&r.local_name))
        {
            let alias = resource
                .provider_ref
                .as_ref()
                .and_then(|r| r.alias.clone())
                .unwrap_or_else(|| Arc::from("default"));
            if alias_not_found_emitted.insert(Arc::clone(&alias)) {
                diagnostics.push(Diagnostic::new(
                    Severity::Warn,
                    "TF1602",
                    format!(
                        "provider alias `{local}.{alias}` referenced by `{}` not declared in \
                         component `{}`",
                        resource.address.as_str(),
                        component.path.display()
                    ),
                ));
            }
        }

        let region = first_resolved_region(
            provider,
            cascade_region_v.as_ref(),
            ctx.default_region.as_ref(),
        );
        let (account_id, account_name) = first_resolved_account(
            provider,
            ctx,
            unresolved,
            diagnostics,
            cascade_account_v.as_ref(),
        );

        resource.region = region;
        resource.account_id = account_id;
        resource.account_name = account_name;
    }

    // State-backend fill per spec § 4 trailing block.
    if let Some(backend) = component.state_backend.as_mut() {
        fill_state_backend(backend, ctx, unresolved, diagnostics);
    }
    if let Some(tg) = component.terragrunt.as_mut()
        && let Some(backend) = tg.state_backend.as_mut()
    {
        fill_state_backend(backend, ctx, unresolved, diagnostics);
    }
}

/// Pick the matching `provider` block for the resource's reference.
fn pick_provider<'a>(
    providers: &'a [ProviderBlock],
    pref: Option<&ProviderRef>,
) -> Option<&'a ProviderBlock> {
    match pref {
        Some(r) => providers.iter().find(|p| {
            p.local_name == r.local_name && p.alias.as_ref().map(Arc::as_ref) == r.alias.as_deref()
        }),
        None => providers
            .iter()
            .find(|p| p.alias.is_none() && &*p.local_name == "aws")
            .or_else(|| providers.iter().find(|p| p.alias.is_none())),
    }
}

fn first_resolved_region(
    provider: Option<&ProviderBlock>,
    cascade: Option<&Region>,
    default: Option<&Region>,
) -> Option<Region> {
    if let Some(p) = provider
        && let Some(r) = p.region_expr.as_ref().and_then(literal_region)
    {
        return Some(r);
    }
    cascade.cloned().or_else(|| default.cloned())
}

/// Resolve `(account_id, account_name)` per spec § 4 / 4.1 / 4.2.
fn first_resolved_account(
    provider: Option<&ProviderBlock>,
    ctx: &ProviderContext,
    unresolved: &mut BTreeSet<Arc<str>>,
    _diagnostics: &mut [Diagnostic],
    cascade: Option<&AccountId>,
) -> (Option<AccountId>, Option<Arc<str>>) {
    // 1. assume_role.role_arn
    if let Some(p) = provider
        && let Some(arn_str) = p.assume_role.as_ref().and_then(assume_role_arn)
        && let Some(id) = extract_account_id(arn_str.as_ref())
    {
        // Account name: look up by ARN in the profile map (typical
        // operator labels their roles with the same account name). When
        // multiple profiles share an account id, take the lexicographically
        // smallest profile name so the result is deterministic
        // (I-PROV-1) rather than `HashMap`-iteration-order-dependent.
        let name = lookup_name_by_account(&ctx.profile_map, &id);
        return (Some(id), name);
    }

    // 2. provider.profile_expr → profile_map.lookup
    if let Some(p) = provider
        && let Some(profile) = p.profile_expr.as_ref().and_then(literal_str)
    {
        if let Some(entry) = ctx.profile_map.lookup(&profile) {
            return (
                Some(entry.account_id.clone()),
                Some(Arc::clone(&entry.account_name)),
            );
        }
        unresolved.insert(profile);
    }

    // 3. cascade locals (Terragrunt-supplied `aws_account_id`)
    if let Some(id) = cascade.cloned() {
        let name = lookup_name_by_account(&ctx.profile_map, &id);
        return (Some(id), name);
    }

    (None, None)
}

/// Reverse-lookup an account-name from the profile map by `AccountId`. When
/// multiple profiles map to the same account, returns the
/// lexicographically-smallest profile's `account_name`. Deterministic by
/// construction — required by I-PROV-1.
fn lookup_name_by_account(map: &ProfileMap, id: &AccountId) -> Option<Arc<str>> {
    let mut hits: Vec<(&Arc<str>, &Arc<str>)> = map
        .iter()
        .filter(|(_, entry)| &entry.account_id == id)
        .map(|(profile, entry)| (profile, &entry.account_name))
        .collect();
    hits.sort_by(|a, b| a.0.cmp(b.0));
    hits.first().map(|(_, name)| Arc::clone(name))
}

fn fill_state_backend(
    backend: &mut StateBackend,
    ctx: &ProviderContext,
    unresolved: &mut BTreeSet<Arc<str>>,
    diagnostics: &mut [Diagnostic],
) {
    let attrs = &backend.attributes;

    // 1. profile → profile_map
    if backend.state_account_id.is_none()
        && let Some(profile) = attr_str(attrs, "profile")
    {
        if let Some(entry) = ctx.profile_map.lookup(&profile) {
            backend.state_account_id = Some(entry.account_id.clone());
        } else {
            unresolved.insert(profile);
        }
    }

    // 2. role_arn → extract_account_id
    if backend.state_account_id.is_none()
        && let Some(arn) = attr_str(attrs, "role_arn")
        && let Some(id) = extract_account_id(&arn)
    {
        backend.state_account_id = Some(id);
    }

    // 3. region (direct)
    if backend.state_region.is_none()
        && let Some(r) = attr_str(attrs, "region")
        && let Ok(parsed) = Region::new(&r)
    {
        backend.state_region = Some(parsed);
    }

    let _ = diagnostics; // diagnostics for malformed values: future hook (P-068).
}

// ----------------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------------

/// Parse an `arn:aws:iam::<12 digits>:role/<...>` and return the 12-digit
/// account id. Returns `None` for any other shape — including
/// `assume_role`-less providers and malformed ARNs.
///
/// Per spec § 4.1.
#[must_use]
pub fn extract_account_id(role_arn: &str) -> Option<AccountId> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        // SAFETY (clippy::unwrap_used): the literal compiles; the
        // matching unit test pins it.
        #[allow(clippy::unwrap_used)]
        {
            Regex::new(r"^arn:aws:iam::(\d{12}):role/").unwrap()
        }
    });
    let caps = re.captures(role_arn)?;
    let m = caps.get(1)?;
    AccountId::new(m.as_str()).ok()
}

fn literal_str(expr: &Expression) -> Option<Arc<str>> {
    match expr {
        Expression::Literal(Value::Str(s)) => Some(Arc::clone(s)),
        _ => None,
    }
}

fn literal_region(expr: &Expression) -> Option<Region> {
    let s = literal_str(expr)?;
    Region::new(s.as_ref()).ok()
}

fn assume_role_arn(ar: &AssumeRole) -> Option<Arc<str>> {
    literal_str(&ar.role_arn_expr)
}

fn attr_str(attrs: &crate::ir::AttributeMap, key: &str) -> Option<Arc<str>> {
    attrs
        .iter()
        .find(|(k, _)| k.as_ref() == key)
        .and_then(|(_, v)| literal_str(v))
}

fn cascade_region(tg: &crate::ir::TerragruntConfig) -> Option<Region> {
    tg.effective_locals
        .iter()
        .find(|(k, _)| k.as_ref() == "aws_region")
        .and_then(|(_, v)| match v {
            Value::Str(s) => Region::new(s.as_ref()).ok(),
            _ => None,
        })
}

fn cascade_account(tg: &crate::ir::TerragruntConfig) -> Option<AccountId> {
    tg.effective_locals
        .iter()
        .find(|(k, _)| k.as_ref() == "aws_account_id")
        .and_then(|(_, v)| match v {
            Value::Str(s) => AccountId::new(s.as_ref()).ok(),
            _ => None,
        })
}

#[allow(dead_code)]
fn _profile_entry_dbg(e: &ProfileEntry) -> &ProfileEntry {
    e
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
    };

    use super::*;
    use crate::{
        ir::{
            Address, AssumeRole, AttributeMap, Component, ComponentId, ComponentKind, Expression,
            ProviderBlock, ProviderRef, Resource, ResourceKind, Span, TerragruntConfig, Workspace,
        },
        provider::profile_map::{ProfileMap, empty},
    };

    // ARN tests

    #[test]
    fn test_should_extract_account_id_from_role_arn() {
        let id = extract_account_id("arn:aws:iam::123456789012:role/admin").unwrap();
        assert_eq!(id.as_str(), "123456789012");
    }

    #[test]
    fn test_should_reject_malformed_arn() {
        assert!(extract_account_id("not-an-arn").is_none());
        assert!(extract_account_id("arn:aws:iam::abc:role/x").is_none());
        assert!(extract_account_id("arn:aws:s3:::bucket").is_none());
    }

    // Resolver: provider chain → role_arn

    fn synth_profile_map_with(profile: &str, account: &str, region: &str) -> Arc<ProfileMap> {
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            f.path(),
            format!(
                "profiles:\n  {profile}:\n    account_id: \"{account}\"\n    account_name: \
                 \"{profile}\"\n    region: \"{region}\"\n"
            ),
        )
        .unwrap();
        crate::provider::profile_map::load_yaml_profile_map(f.path()).unwrap()
    }

    fn span() -> Span {
        Span::synthetic()
    }

    fn resource_with_provider(addr: &str, alias: Option<&str>) -> Resource {
        Resource::builder()
            .address(Address::new(addr).unwrap())
            .kind(ResourceKind::Managed)
            .type_(Arc::<str>::from("aws_iam_role"))
            .name(Arc::<str>::from("r"))
            .provider_ref(alias.map(|a| {
                ProviderRef::builder()
                    .local_name(Arc::<str>::from("aws"))
                    .alias(Some(Arc::<str>::from(a)))
                    .span(span())
                    .build()
            }))
            .span(span())
            .build()
    }

    fn component_with(
        path: &str,
        providers: Vec<ProviderBlock>,
        resources: Vec<Resource>,
    ) -> Component {
        Component::builder()
            .id(ComponentId::from_index(0))
            .path(Arc::<Path>::from(PathBuf::from(path)))
            .kind(ComponentKind::Component)
            .providers(providers)
            .resources(resources)
            .build()
    }

    fn workspace_with(component: Component) -> Workspace {
        Workspace::builder()
            .root(Arc::<Path>::from(PathBuf::from("/tmp/repo")))
            .components(vec![component])
            .build()
    }

    fn provider_with(
        alias: Option<&str>,
        profile: Option<&str>,
        region: Option<&str>,
    ) -> ProviderBlock {
        ProviderBlock::builder()
            .local_name(Arc::<str>::from("aws"))
            .alias(alias.map(Arc::<str>::from))
            .profile_expr(profile.map(|p| Expression::Literal(Value::Str(Arc::from(p)))))
            .region_expr(region.map(|r| Expression::Literal(Value::Str(Arc::from(r)))))
            .span(span())
            .build()
    }

    #[test]
    fn test_should_resolve_account_from_provider_profile() {
        let map = synth_profile_map_with("primary", "100000000001", "us-west-2");
        let resource = resource_with_provider("aws_iam_role.r", Some("main"));
        let provider = provider_with(Some("main"), Some("primary"), Some("us-west-2"));
        let component = component_with("svc", vec![provider], vec![resource]);
        let mut ws = workspace_with(component);

        DefaultProviderResolver
            .resolve(&mut ws, &ProviderContext::new(map))
            .unwrap();
        let r = &ws.components[0].resources[0];
        assert_eq!(
            r.account_id.as_ref().map(AccountId::as_str),
            Some("100000000001")
        );
        assert_eq!(r.region.as_ref().map(Region::as_str), Some("us-west-2"));
        assert_eq!(r.account_name.as_deref(), Some("primary"));
    }

    #[test]
    fn test_should_prefer_assume_role_over_profile() {
        let map = synth_profile_map_with("primary", "100000000001", "us-west-2");
        let mut provider = provider_with(Some("main"), Some("primary"), Some("us-west-2"));
        provider.assume_role = Some(
            AssumeRole::builder()
                .role_arn_expr(Expression::Literal(Value::Str(Arc::from(
                    "arn:aws:iam::999999999999:role/x",
                ))))
                .span(span())
                .build(),
        );
        let resource = resource_with_provider("aws_iam_role.r", Some("main"));
        let component = component_with("svc", vec![provider], vec![resource]);
        let mut ws = workspace_with(component);

        DefaultProviderResolver
            .resolve(&mut ws, &ProviderContext::new(map))
            .unwrap();
        let r = &ws.components[0].resources[0];
        assert_eq!(
            r.account_id.as_ref().map(AccountId::as_str),
            Some("999999999999")
        );
    }

    #[test]
    fn test_should_fall_through_to_terragrunt_cascade_account() {
        let map = empty();
        let resource = resource_with_provider("aws_iam_role.r", None);
        let mut component = component_with("svc", vec![], vec![resource]);
        component.terragrunt = Some(
            TerragruntConfig::builder()
                .component_dir(Arc::<Path>::from(PathBuf::from("/repo/svc")))
                .effective_locals(vec![
                    (
                        Arc::from("aws_account_id"),
                        Value::Str(Arc::from("200000000002")),
                    ),
                    (Arc::from("aws_region"), Value::Str(Arc::from("eu-west-1"))),
                ])
                .build(),
        );
        let mut ws = workspace_with(component);
        DefaultProviderResolver
            .resolve(&mut ws, &ProviderContext::new(map))
            .unwrap();
        let r = &ws.components[0].resources[0];
        assert_eq!(
            r.account_id.as_ref().map(AccountId::as_str),
            Some("200000000002")
        );
        assert_eq!(r.region.as_ref().map(Region::as_str), Some("eu-west-1"));
    }

    #[test]
    fn test_should_emit_missing_profile_diagnostic_once() {
        let map = empty();
        let mut providers = HashMap::<String, ProviderBlock>::new();
        providers.insert("a".into(), provider_with(Some("a"), Some("missing"), None));
        providers.insert("b".into(), provider_with(Some("b"), Some("missing"), None));
        let provider_a = providers.remove("a").unwrap();
        let provider_b = providers.remove("b").unwrap();
        let r1 = resource_with_provider("aws_iam_role.r1", Some("a"));
        let r2 = resource_with_provider("aws_iam_role.r2", Some("b"));
        let component = component_with("svc", vec![provider_a, provider_b], vec![r1, r2]);
        let mut ws = workspace_with(component);

        DefaultProviderResolver
            .resolve(&mut ws, &ProviderContext::new(map))
            .unwrap();
        let missing: Vec<_> = ws
            .diagnostics
            .iter()
            .filter(|d| d.code.as_ref() == "TF1601")
            .collect();
        assert_eq!(missing.len(), 1, "expected dedup; got {missing:?}");
    }

    #[test]
    fn test_should_emit_alias_not_found_diagnostic_per_alias() {
        let map = empty();
        let resource = resource_with_provider("aws_iam_role.r", Some("ghost"));
        let component = component_with("svc", vec![], vec![resource]);
        let mut ws = workspace_with(component);
        DefaultProviderResolver
            .resolve(&mut ws, &ProviderContext::new(map))
            .unwrap();
        assert!(
            ws.diagnostics.iter().any(|d| d.code.as_ref() == "TF1602"),
            "{:?}",
            ws.diagnostics
        );
    }

    #[test]
    fn test_strict_mode_returns_error_on_unresolved_profile() {
        let map = empty();
        let provider = provider_with(Some("main"), Some("ghost"), None);
        let resource = resource_with_provider("aws_iam_role.r", Some("main"));
        let component = component_with("svc", vec![provider], vec![resource]);
        let mut ws = workspace_with(component);
        let mut ctx = ProviderContext::new(map);
        ctx.strict = true;
        let err = DefaultProviderResolver.resolve(&mut ws, &ctx).unwrap_err();
        assert!(
            format!("{err}").contains("ghost"),
            "expected strict error to mention profile: {err}"
        );
    }

    #[test]
    fn test_should_fill_state_backend_account_from_profile() {
        let map = synth_profile_map_with("backend-profile", "300000000003", "ap-southeast-1");
        let attrs: AttributeMap = vec![
            (
                Arc::from("profile"),
                Expression::Literal(Value::Str(Arc::from("backend-profile"))),
            ),
            (
                Arc::from("region"),
                Expression::Literal(Value::Str(Arc::from("ap-southeast-1"))),
            ),
        ];
        let mut backend = StateBackend::builder()
            .kind(Arc::<str>::from("s3"))
            .attributes(attrs)
            .span(span())
            .build();
        let mut unresolved: BTreeSet<Arc<str>> = BTreeSet::new();
        let mut diags: Vec<Diagnostic> = Vec::new();
        fill_state_backend(
            &mut backend,
            &ProviderContext::new(map),
            &mut unresolved,
            &mut diags,
        );
        assert_eq!(
            backend.state_account_id.as_ref().map(AccountId::as_str),
            Some("300000000003")
        );
        assert_eq!(
            backend.state_region.as_ref().map(Region::as_str),
            Some("ap-southeast-1")
        );
    }

    #[test]
    fn test_should_fill_state_backend_account_from_role_arn() {
        let map = empty();
        let attrs: AttributeMap = vec![(
            Arc::from("role_arn"),
            Expression::Literal(Value::Str(Arc::from(
                "arn:aws:iam::444444444444:role/state-writer",
            ))),
        )];
        let mut backend = StateBackend::builder()
            .kind(Arc::<str>::from("s3"))
            .attributes(attrs)
            .span(span())
            .build();
        let mut unresolved: BTreeSet<Arc<str>> = BTreeSet::new();
        let mut diags: Vec<Diagnostic> = Vec::new();
        fill_state_backend(
            &mut backend,
            &ProviderContext::new(map),
            &mut unresolved,
            &mut diags,
        );
        assert_eq!(
            backend.state_account_id.as_ref().map(AccountId::as_str),
            Some("444444444444")
        );
    }
}
