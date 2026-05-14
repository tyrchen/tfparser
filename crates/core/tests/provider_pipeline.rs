//! Phase 7 integration: provider resolver fills `account_id` / `region` /
//! `state_account_id` / `state_region`, and meets the ≥95% coverage target
//! from spec 91 § 10.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::cast_precision_loss,
    clippy::similar_names,
    clippy::doc_markdown
)]

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use tfparser_core::{
    AccountId, Address, AssumeRole, Component, ComponentId, ComponentKind, DefaultProviderResolver,
    Expression, ProviderBlock, ProviderContext, ProviderRef, ProviderResolver, Region, Resource,
    ResourceKind, Span, StateBackend, Value, Workspace, empty_profile_map, load_yaml_profile_map,
};

fn span() -> Span {
    Span::synthetic()
}

fn resource(addr: &str, alias: Option<&str>) -> Resource {
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

fn provider(alias: Option<&str>, profile: Option<&str>, region: Option<&str>) -> ProviderBlock {
    ProviderBlock::builder()
        .local_name(Arc::<str>::from("aws"))
        .alias(alias.map(Arc::<str>::from))
        .profile_expr(profile.map(|p| Expression::Literal(Value::Str(Arc::from(p)))))
        .region_expr(region.map(|r| Expression::Literal(Value::Str(Arc::from(r)))))
        .span(span())
        .build()
}

fn write_yaml(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

/// Build a 6-component workspace where 20 of 20 resources have a resolvable
/// `account_id` — coverage 100 %, well past the 95 % gate in spec 91 § 10.
///
/// Two of the components mix profile, assume_role, and terragrunt cascade
/// resolution paths so the test exercises all three branches of spec § 4.
#[test]
fn test_should_meet_95pct_account_id_coverage_on_synthetic_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let yaml = tmp.path().join("profile-map.yaml");
    write_yaml(
        &yaml,
        r#"
profiles:
  primary:
    account_id: "100000000001"
    account_name: "primary"
    region: "us-west-2"
  secondary:
    account_id: "200000000002"
    account_name: "secondary"
    region: "eu-west-1"
  backend-writer:
    account_id: "300000000003"
    account_name: "backend"
    region: "us-east-1"
"#,
    );
    let map = load_yaml_profile_map(&yaml).unwrap();

    // Component A: 5 resources hitting the default provider's profile chain.
    let comp_a = Component::builder()
        .id(ComponentId::from_index(0))
        .path(Arc::<Path>::from(PathBuf::from("svc/a")))
        .kind(ComponentKind::Component)
        .providers(vec![provider(None, Some("primary"), Some("us-west-2"))])
        .resources(
            (0..5)
                .map(|i| resource(&format!("aws_iam_role.r{i}"), None))
                .collect::<Vec<_>>(),
        )
        .build();

    // Component B: 5 resources via alias=main → profile=secondary.
    let comp_b = Component::builder()
        .id(ComponentId::from_index(1))
        .path(Arc::<Path>::from(PathBuf::from("svc/b")))
        .kind(ComponentKind::Component)
        .providers(vec![provider(
            Some("main"),
            Some("secondary"),
            Some("eu-west-1"),
        )])
        .resources(
            (0..5)
                .map(|i| resource(&format!("aws_iam_role.r{i}"), Some("main")))
                .collect::<Vec<_>>(),
        )
        .build();

    // Component C: 4 resources via assume_role (ARN-only); plus 1 state backend.
    let cross_provider = ProviderBlock::builder()
        .local_name(Arc::<str>::from("aws"))
        .alias(Some(Arc::<str>::from("cross")))
        .region_expr(Some(Expression::Literal(Value::Str(Arc::from(
            "ap-southeast-1",
        )))))
        .assume_role(Some(
            AssumeRole::builder()
                .role_arn_expr(Expression::Literal(Value::Str(Arc::from(
                    "arn:aws:iam::400000000004:role/cross",
                ))))
                .span(span())
                .build(),
        ))
        .span(span())
        .build();

    let comp_c = Component::builder()
        .id(ComponentId::from_index(2))
        .path(Arc::<Path>::from(PathBuf::from("svc/c")))
        .kind(ComponentKind::Component)
        .providers(vec![cross_provider])
        .resources(
            (0..4)
                .map(|i| resource(&format!("aws_iam_role.r{i}"), Some("cross")))
                .collect::<Vec<_>>(),
        )
        .state_backend(Some(
            StateBackend::builder()
                .kind(Arc::<str>::from("s3"))
                .attributes(vec![
                    (
                        Arc::from("profile"),
                        Expression::Literal(Value::Str(Arc::from("backend-writer"))),
                    ),
                    (
                        Arc::from("region"),
                        Expression::Literal(Value::Str(Arc::from("us-east-1"))),
                    ),
                ])
                .span(span())
                .build(),
        ))
        .build();

    // Component D: 6 resources falling through to terragrunt cascade.
    let comp_d = Component::builder()
        .id(ComponentId::from_index(3))
        .path(Arc::<Path>::from(PathBuf::from("svc/d")))
        .kind(ComponentKind::Component)
        .providers(vec![]) // no provider block declared
        .resources(
            (0..6)
                .map(|i| resource(&format!("aws_iam_role.r{i}"), None))
                .collect::<Vec<_>>(),
        )
        .terragrunt(Some(
            tfparser_core::TerragruntConfig::builder()
                .component_dir(Arc::<Path>::from(PathBuf::from("/repo/svc/d")))
                .effective_locals(vec![
                    (
                        Arc::from("aws_account_id"),
                        Value::Str(Arc::from("200000000002")),
                    ),
                    (Arc::from("aws_region"), Value::Str(Arc::from("eu-west-1"))),
                ])
                .build(),
        ))
        .build();

    let mut ws = Workspace::builder()
        .root(Arc::<Path>::from(PathBuf::from("/tmp/repo")))
        .components(vec![comp_a, comp_b, comp_c, comp_d])
        .build();

    DefaultProviderResolver
        .resolve(&mut ws, &ProviderContext::new(map))
        .unwrap();

    let total: usize = ws.components.iter().map(|c| c.resources.len()).sum();
    let resolved: usize = ws
        .components
        .iter()
        .flat_map(|c| c.resources.iter())
        .filter(|r| r.account_id.is_some())
        .count();
    let pct = (resolved as f64) / (total as f64);
    assert!(
        pct >= 0.95,
        "coverage {resolved}/{total} = {pct:.3} below 95% gate"
    );

    // Spot-checks pin the chain branches.
    let r_a0 = &ws.components[0].resources[0];
    assert_eq!(
        r_a0.account_id.as_ref().map(AccountId::as_str),
        Some("100000000001")
    );
    assert_eq!(r_a0.region.as_ref().map(Region::as_str), Some("us-west-2"));

    let r_c0 = &ws.components[2].resources[0];
    assert_eq!(
        r_c0.account_id.as_ref().map(AccountId::as_str),
        Some("400000000004")
    );

    let state = ws.components[2].state_backend.as_ref().unwrap();
    assert_eq!(
        state.state_account_id.as_ref().map(AccountId::as_str),
        Some("300000000003")
    );

    let r_d0 = &ws.components[3].resources[0];
    assert_eq!(
        r_d0.account_id.as_ref().map(AccountId::as_str),
        Some("200000000002")
    );
}

#[test]
fn test_should_emit_one_missing_profile_diagnostic_workspace_wide() {
    let map = empty_profile_map();
    let comp = Component::builder()
        .id(ComponentId::from_index(0))
        .path(Arc::<Path>::from(PathBuf::from("svc")))
        .kind(ComponentKind::Component)
        .providers(vec![
            provider(Some("a"), Some("phantom"), None),
            provider(Some("b"), Some("phantom"), None),
        ])
        .resources(vec![
            resource("aws_iam_role.x", Some("a")),
            resource("aws_iam_role.y", Some("b")),
        ])
        .build();
    let mut ws = Workspace::builder()
        .root(Arc::<Path>::from(PathBuf::from("/tmp/repo")))
        .components(vec![comp])
        .build();
    DefaultProviderResolver
        .resolve(&mut ws, &ProviderContext::new(map))
        .unwrap();

    let count = ws
        .diagnostics
        .iter()
        .filter(|d| d.code.as_ref() == "TF1601")
        .count();
    assert_eq!(count, 1, "expected one dedup'd diagnostic, got {count}");
}

#[test]
fn test_resolve_is_deterministic_across_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let yaml = tmp.path().join("profile-map.yaml");
    write_yaml(
        &yaml,
        r#"
profiles:
  primary:
    account_id: "100000000001"
    account_name: "primary"
"#,
    );
    let map = load_yaml_profile_map(&yaml).unwrap();
    let comp = Component::builder()
        .id(ComponentId::from_index(0))
        .path(Arc::<Path>::from(PathBuf::from("svc")))
        .kind(ComponentKind::Component)
        .providers(vec![provider(None, Some("primary"), None)])
        .resources(vec![resource("aws_iam_role.r", None)])
        .build();

    let snapshot = |ws: &Workspace| -> Vec<(Option<String>, Option<String>)> {
        ws.components
            .iter()
            .flat_map(|c| c.resources.iter())
            .map(|r| {
                (
                    r.account_id.as_ref().map(|a| a.as_str().to_string()),
                    r.region.as_ref().map(|r| r.as_str().to_string()),
                )
            })
            .collect()
    };

    let mut ws_a = Workspace::builder()
        .root(Arc::<Path>::from(PathBuf::from("/tmp/repo")))
        .components(vec![comp.clone()])
        .build();
    let mut ws_b = Workspace::builder()
        .root(Arc::<Path>::from(PathBuf::from("/tmp/repo")))
        .components(vec![comp])
        .build();

    DefaultProviderResolver
        .resolve(&mut ws_a, &ProviderContext::new(Arc::clone(&map)))
        .unwrap();
    DefaultProviderResolver
        .resolve(&mut ws_b, &ProviderContext::new(map))
        .unwrap();
    assert_eq!(snapshot(&ws_a), snapshot(&ws_b));
}
