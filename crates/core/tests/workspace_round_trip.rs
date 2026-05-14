//! Phase 1 exit criterion: a non-trivial [`Workspace`] (with components,
//! modules, environments, providers, resources, diagnostics, and Terragrunt
//! config) round-trips losslessly through `serde_json`.
//!
//! This pins the IR contract one level above the per-module unit tests: it
//! proves the whole graph composes and serialises as one document.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::indexing_slicing
)]
// Test convenience — this file constructs a deeply-nested fixture; the line
// count is unavoidable without splitting it across multiple files.

use std::{path::PathBuf, sync::Arc};

use tfparser_core::{
    AccountId, Address, AssumeRole, AttributeMap, BinaryOp, Component, ComponentId, ComponentKind,
    DependencyBlock, Diagnostic, Environment, Expression, FileExt, GenerateBlock, IncludePath,
    Local, Map, Module, ModuleCall, ModuleId, ModuleSource, Output, ProviderBlock, ProviderRef,
    Region, Resource, ResourceKind, Severity, SourceFile, Span, StateBackend, SymbolKind, Symbolic,
    TerragruntConfig, Value, Variable, Workspace,
};

fn s(text: &str) -> Arc<str> {
    Arc::from(text)
}

fn synthetic_span(file: &str, line: u32, column: u32) -> Span {
    Span::new(Arc::from(PathBuf::from(file).as_path()), 0..1, line, column).unwrap()
}

fn build_fixture_workspace() -> Workspace {
    let root: Arc<std::path::Path> = Arc::from(PathBuf::from("/repo/large-monorepo"));

    let span = synthetic_span("services/order-service/main.tf", 12, 1);

    let provider_block = ProviderBlock::builder()
        .local_name(s("aws"))
        .alias(Some(s("data")))
        .source_addr(Some(s("hashicorp/aws")))
        .region_expr(Some(Expression::Literal(Value::Str(s("us-east-1")))))
        .profile_expr(Some(Expression::Literal(Value::Str(s(
            "northwind-data-developer",
        )))))
        .assume_role(Some(
            AssumeRole::builder()
                .role_arn_expr(Expression::Literal(Value::Str(s(
                    "arn:aws:iam::100000000002:role/iam-identity-role-terraform-user",
                ))))
                .span(span.clone())
                .build(),
        ))
        .raw(vec![(
            s("region"),
            Expression::Literal(Value::Str(s("us-east-1"))),
        )])
        .span(span.clone())
        .build();

    let provider_ref = ProviderRef::builder()
        .local_name(s("aws"))
        .alias(Some(s("data")))
        .span(span.clone())
        .build();

    let unresolved_env = |span: &Span, hint: Option<Address>| {
        Expression::Unresolved(
            Symbolic::builder()
                .kind(SymbolKind::Var)
                .source(s("var.environment"))
                .address_hint(hint)
                .span(span.clone())
                .build(),
        )
    };

    let resource_attrs: AttributeMap = vec![
        (
            s("name"),
            Expression::TemplateConcat(vec![
                Expression::Literal(Value::Str(s("orders-events-"))),
                unresolved_env(&span, Some(Address::new("var.environment").unwrap())),
            ]),
        ),
        (
            s("versioning"),
            Expression::BinaryOp {
                op: BinaryOp::Eq,
                lhs: Box::new(unresolved_env(&span, None)),
                rhs: Box::new(Expression::Literal(Value::Str(s("production")))),
                span: span.clone(),
            },
        ),
    ];

    let resource = Resource::builder()
        .address(Address::new("module.events.aws_s3_bucket.this").unwrap())
        .kind(ResourceKind::Managed)
        .type_(s("aws_s3_bucket"))
        .name(s("this"))
        .provider_ref(Some(provider_ref.clone()))
        .count_expr(Some(Expression::Literal(Value::Int(1))))
        .for_each_expr(None)
        .depends_on(vec![Address::new("aws_iam_role.lambda").unwrap()])
        .attributes(resource_attrs)
        .span(span.clone())
        .build();

    let module_call = ModuleCall::builder()
        .address(Address::new("module.events").unwrap())
        .source_raw(s("../../modules/s3-bucket"))
        .source(ModuleSource::Local(s("../../modules/s3-bucket")))
        .resolved(Some(ModuleId::from_index(0)))
        .providers(vec![(s("aws"), provider_ref.clone())])
        .inputs(vec![(s("name"), Expression::Literal(Value::Str(s("x"))))])
        .span(span.clone())
        .build();

    let component = Component::builder()
        .id(ComponentId::from_index(0))
        .path(Arc::<std::path::Path>::from(PathBuf::from(
            "services/order-service",
        )))
        .kind(ComponentKind::Component)
        .files(vec![
            SourceFile::builder()
                .path(Arc::<std::path::Path>::from(PathBuf::from(
                    "services/order-service/main.tf",
                )))
                .ext(FileExt::Tf)
                .size(1024_u64)
                .build(),
        ])
        .variables(vec![
            Variable::builder()
                .name(s("environment"))
                .description(Some(s("Deployment environment")))
                .default(Some(Expression::Literal(Value::Str(s("staging")))))
                .span(span.clone())
                .build(),
        ])
        .locals(vec![
            Local::builder()
                .name(s("service_name"))
                .value(Expression::Literal(Value::Str(s("order-service"))))
                .span(span.clone())
                .build(),
        ])
        .providers(vec![provider_block])
        .resources(vec![resource])
        .modules(vec![module_call])
        .outputs(vec![
            Output::builder()
                .name(s("events_bucket"))
                .value(Expression::Unresolved(
                    Symbolic::builder()
                        .kind(SymbolKind::Module)
                        .source(s("module.events.bucket_id"))
                        .address_hint(Some(Address::new("module.events.bucket_id").unwrap()))
                        .span(span.clone())
                        .build(),
                ))
                .description(Some(s("Bucket holding analytics events")))
                .span(span.clone())
                .build(),
        ])
        .terragrunt(Some(
            TerragruntConfig::builder()
                .component_dir(Arc::<std::path::Path>::from(PathBuf::from(
                    "/repo/large-monorepo/services/order-service",
                )))
                .effective_locals(vec![(s("aws_region"), Value::Str(s("us-west-2")))] as Map)
                .inputs(vec![] as Map)
                .includes(vec![
                    IncludePath::builder()
                        .path(Arc::<std::path::Path>::from(PathBuf::from(
                            "/repo/large-monorepo/terraform/root.hcl",
                        )))
                        .label(Some(s("root")))
                        .span(span.clone())
                        .build(),
                ])
                .generates(vec![
                    GenerateBlock::builder()
                        .label(s("backend"))
                        .path(Arc::<std::path::Path>::from(PathBuf::from(
                            "generated_backend.tf",
                        )))
                        .if_exists(s("overwrite_terragrunt"))
                        .contents(s("terraform { backend \"s3\" {} }"))
                        .span(span.clone())
                        .build(),
                ])
                .dependencies(vec![
                    DependencyBlock::builder()
                        .name(s("network"))
                        .config_path(Arc::<std::path::Path>::from(PathBuf::from(
                            "/repo/large-monorepo/terraform/platform/main-network",
                        )))
                        .mock_outputs(vec![] as AttributeMap)
                        .span(span.clone())
                        .build(),
                ])
                .state_backend(Some(
                    StateBackend::builder()
                        .kind(s("s3"))
                        .attributes(vec![] as AttributeMap)
                        .state_account_id(Some(AccountId::new("100000000099").unwrap()))
                        .state_region(Some(Region::new("us-west-2").unwrap()))
                        .span(span.clone())
                        .build(),
                ))
                .diagnostics(vec![])
                .build(),
        ))
        .build();

    let module_body = Component::builder()
        .id(ComponentId::from_index(1))
        .path(Arc::<std::path::Path>::from(PathBuf::from(
            "modules/s3-bucket",
        )))
        .kind(ComponentKind::Module)
        .build();

    let module = Module::builder()
        .id(ModuleId::from_index(0))
        .source(ModuleSource::Local(s("modules/s3-bucket")))
        .canonical_path(Some(Arc::<std::path::Path>::from(PathBuf::from(
            "/repo/large-monorepo/modules/s3-bucket",
        ))))
        .component(module_body)
        .build();

    let environment = Environment::builder()
        .name(s("staging"))
        .aws_account_id(Some(AccountId::new("100000000001").unwrap()))
        .aws_region(Some(Region::new("us-west-2").unwrap()))
        .aws_profile(Some(s("northwind-main-developer")))
        .source_file(Arc::<std::path::Path>::from(PathBuf::from(
            "terraform/environments/staging.terragrunt.hcl",
        )))
        .locals(vec![] as Map)
        .build();

    Workspace::builder()
        .root(root)
        .components(vec![component])
        .modules(vec![module])
        .environments(vec![environment])
        .diagnostics(vec![Diagnostic::new(
            Severity::Warn,
            "TF1001",
            "synthetic test diagnostic",
        )])
        .build()
}

#[test]
fn test_should_round_trip_full_workspace_through_serde_json() {
    let ws = build_fixture_workspace();
    let json = serde_json::to_string(&ws).expect("serialize workspace");
    let back: Workspace = serde_json::from_str(&json).expect("deserialize workspace");
    assert_eq!(ws, back, "round-trip lost data");

    // Cheap sanity checks on the JSON shape — guard against accidental
    // schema renames.
    assert!(
        json.contains("\"environments\""),
        "missing environments field"
    );
    assert!(json.contains("\"modules\""), "missing modules field");
    assert!(
        json.contains("\"providerRef\""),
        "missing providerRef field"
    );
    assert!(json.contains("\"terragrunt\""), "missing terragrunt field");
}

#[test]
fn test_should_be_deterministic_under_repeated_serialization() {
    let ws = build_fixture_workspace();
    let a = serde_json::to_string(&ws).unwrap();
    let b = serde_json::to_string(&ws).unwrap();
    assert_eq!(a, b, "serialization is not deterministic");
}

#[test]
fn test_should_preserve_unresolved_kind_in_canonical_json() {
    let ws = build_fixture_workspace();
    let raw = serde_json::to_string(&ws).unwrap();
    // The tagged-enum representation embeds both `kind: "unresolved"` and
    // the inner `source` field. Either changing should be a conscious break.
    assert!(raw.contains("\"unresolved\""), "lost unresolved tag");
    assert!(raw.contains("var.environment"), "lost symbolic source");
}
