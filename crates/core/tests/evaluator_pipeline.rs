//! Phase 4 / M1 integration test: end-to-end discovery → loader →
//! projection → evaluator on a synthetic `multi-provider`-shaped fixture.
//!
//! Pinned exit criteria (per `specs/91-impl-plan.md` § 7):
//!
//! 1. `region = var.region` reduces to the literal value supplied via `repo_vars` (and via variable
//!    `default` when no override).
//! 2. A cycle in locals surfaces as an `Error`-severity diagnostic.
//! 3. `file("../../etc/passwd")` returns `Error::PathEscape`.
//!
//! The fixture is built **in-memory** (no on-disk file) to keep the test
//! hermetic and free of the on-disk `fixtures/multi-provider` shape that
//! ships as a literal `region = "us-east-1"`. Phase 4 sees the same shape
//! after the loader / projection, so this exercises the production path.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::needless_raw_string_hashes
)]

use std::{path::Path, sync::Arc};

use tfparser_core::{
    Evaluator, Severity,
    eval::{CallCx, EnvVarMode, EvalContext, EvalLimits, FuncRegistry, HclEvaluator},
    ir::{ComponentId, ComponentKind, Value},
    loader::{HclEditLoader, LoaderLimits, RawComponent},
    projection::project_component,
};

fn build_component(src: &str) -> tfparser_core::ir::Component {
    let path: Arc<Path> = Arc::from(Path::new("/tmp/eval-fixture.tf"));
    let limits = LoaderLimits::default();
    let parsed = HclEditLoader.parse_bytes(src.as_bytes(), &path, &limits);
    let mut raw = RawComponent::new(Arc::from(Path::new("")), ComponentKind::Component);
    raw.raw_blocks.extend(parsed.blocks);
    let mut diags = Vec::new();
    project_component(&raw, ComponentId::from_index(0), &mut diags)
}

fn make_ctx(workspace_root: &Path) -> EvalContext {
    EvalContext::new(
        Arc::from(workspace_root),
        None,
        EnvVarMode::default(),
        vec![(Arc::from("region"), Value::Str(Arc::from("us-east-2")))],
        Vec::new(),
        Arc::new(FuncRegistry::default_with_stdlib()),
        EvalLimits::default(),
    )
}

#[test]
fn test_should_resolve_multi_provider_region_from_var() {
    let src = r#"
variable "region" {}

provider "aws" {
  alias  = "main"
  region = var.region
}

provider "aws" {
  alias  = "backup"
  region = var.region
}

resource "aws_s3_bucket" "primary" {
  provider = aws.main
  bucket   = "northwind-primary"
  tags = {
    Region = var.region
  }
}
"#;
    let component = build_component(src);
    let evald = HclEvaluator::new()
        .evaluate(&component, &make_ctx(Path::new("/tmp/repo")))
        .unwrap();

    // Both providers' region_expr now carries the literal binding.
    assert_eq!(evald.providers.len(), 2);
    for p in &evald.providers {
        let region = p.region_expr.as_ref().expect("region_expr");
        assert_eq!(
            region.as_literal(),
            Some(&Value::Str(Arc::from("us-east-2"))),
            "expected resolved literal for provider {:?}",
            p.alias.as_deref()
        );
    }

    // The resource attribute carrying `var.region` inside a map literal
    // collapses to a fully-resolved map.
    assert_eq!(evald.resources.len(), 1);
    let tags = evald.resources[0]
        .attributes
        .iter()
        .find(|(k, _)| k.as_ref() == "tags")
        .expect("tags attr")
        .1
        .clone();
    assert!(matches!(tags.as_literal(), Some(Value::Map(_)),));
}

#[test]
fn test_should_resolve_region_via_variable_default_when_no_repo_var() {
    let src = r#"
variable "region" {
  default = "us-west-1"
}

provider "aws" {
  region = var.region
}
"#;
    let component = build_component(src);
    let mut ctx = make_ctx(Path::new("/tmp/repo"));
    ctx.repo_vars.clear();
    let evald = HclEvaluator::new().evaluate(&component, &ctx).unwrap();
    let region = evald.providers[0]
        .region_expr
        .as_ref()
        .expect("region_expr");
    assert_eq!(
        region.as_literal(),
        Some(&Value::Str(Arc::from("us-west-1"))),
    );
}

#[test]
fn test_should_emit_cycle_diagnostic_on_self_cycle() {
    let src = r#"
locals {
  a = local.a
}
"#;
    let component = build_component(src);
    let evald = HclEvaluator::new()
        .evaluate(&component, &make_ctx(Path::new("/tmp/repo")))
        .unwrap();
    let cycle_diag = evald
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error && &*d.code == "TF1401")
        .expect("cycle diagnostic");
    assert!(cycle_diag.message.contains("local.a"));
}

#[test]
fn test_should_emit_cycle_diagnostic_on_two_node_cycle() {
    let src = r#"
locals {
  a = local.b
  b = local.a
}
"#;
    let component = build_component(src);
    let evald = HclEvaluator::new()
        .evaluate(&component, &make_ctx(Path::new("/tmp/repo")))
        .unwrap();
    let cycle_diag = evald
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error && &*d.code == "TF1401")
        .expect("cycle diagnostic");
    assert!(cycle_diag.message.contains("local.a"));
    assert!(cycle_diag.message.contains("local.b"));
}

#[test]
fn test_should_reject_path_escape_via_file_function() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();

    let funcs = FuncRegistry::default_with_stdlib();
    let file_fn = funcs.get("file").expect("file func registered");
    let env = EnvVarMode::default();
    let limits = EvalLimits::default();
    let cx = CallCx::new(&root, &env, &limits);
    let err = file_fn
        .call(&[Value::Str(Arc::from("../../etc/passwd"))], &cx)
        .unwrap_err();
    let s = format!("{err:?}");
    assert!(s.contains("PathEscape"), "{s}");
}

#[test]
fn test_should_be_deterministic_over_repeated_evaluations() {
    let src = r#"
variable "region" {}

locals {
  zone = "z-${var.region}"
}

resource "aws_s3_bucket" "b" {
  bucket = local.zone
}
"#;
    let component = build_component(src);
    let ctx = make_ctx(Path::new("/tmp/repo"));
    let a = HclEvaluator::new().evaluate(&component, &ctx).unwrap();
    let b = HclEvaluator::new().evaluate(&component, &ctx).unwrap();
    assert_eq!(
        format!("{:?}", a.resources),
        format!("{:?}", b.resources),
        "evaluator must be deterministic"
    );
    assert_eq!(
        format!("{:?}", a.locals),
        format!("{:?}", b.locals),
        "locals must be deterministic"
    );
    // The resolved value should be the cascade-applied template.
    let bucket = a.resources[0]
        .attributes
        .iter()
        .find(|(k, _)| k.as_ref() == "bucket")
        .unwrap()
        .1
        .clone();
    assert_eq!(
        bucket.as_literal(),
        Some(&Value::Str(Arc::from("z-us-east-2")))
    );
}

#[test]
fn test_should_be_monotone_when_extra_var_binding_added() {
    // Monotonicity (spec 13 § 10): adding a binding never *removes* a
    // resolved value. Compare two runs of the same component, the second
    // with one extra var binding: every resource attribute that resolved
    // in run A must also resolve in run B.
    let src = r#"
variable "region" {}
variable "team"   {}

resource "aws_s3_bucket" "b" {
  bucket = var.region
  tags   = {
    Team = var.team
  }
}
"#;
    let component = build_component(src);

    let mut ctx_a = make_ctx(Path::new("/tmp/repo"));
    ctx_a.repo_vars = vec![(Arc::from("region"), Value::Str(Arc::from("us-east-2")))];
    let mut ctx_b = ctx_a.clone();
    ctx_b
        .repo_vars
        .push((Arc::from("team"), Value::Str(Arc::from("ops"))));

    let a = HclEvaluator::new().evaluate(&component, &ctx_a).unwrap();
    let b = HclEvaluator::new().evaluate(&component, &ctx_b).unwrap();

    for (k, v_a) in &a.resources[0].attributes {
        if v_a.as_literal().is_some() {
            let v_b = &b.resources[0]
                .attributes
                .iter()
                .find(|(name, _)| name == k)
                .expect("same attr in B")
                .1;
            assert!(
                v_b.as_literal().is_some(),
                "attr `{k}` resolved in A but not B (monotonicity violated)"
            );
        }
    }
}
