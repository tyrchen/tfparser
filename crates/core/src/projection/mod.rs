//! `RawComponent` → IR projection.
//!
//! Phase 3 closes M0 by turning the loader's flat list of [`RawBlock`]s into
//! the typed IR shapes downstream code consumes: [`Resource`], [`ProviderBlock`],
//! [`ModuleCall`], [`Variable`], [`Local`], [`Output`], and the workspace-level
//! [`Component`].
//!
//! Per [10-data-model.md § 2.2] and [20-parquet-exporter.md § 3], the projection
//! is **structural**: it copies fields verbatim out of the [`AttributeMap`] the
//! loader produced. No evaluation, no template expansion, no reference
//! resolution. Anything that needs the evaluator stays as
//! [`Expression::Unresolved`] (Phase 4 will reduce them).
//!
//! Provider aliases on resources land as [`ProviderRef`]; the lookup is a
//! syntactic check that the right-hand side of `provider = aws.<alias>` is an
//! [`Expression::Unresolved`] with a non-`Var`/`Local` symbol kind. Anything
//! richer (a conditional, a function call) is treated as "no alias known" and
//! the resource keeps `provider_ref = None`.
//!
//! [10-data-model.md § 2.2]: ../../../specs/10-data-model.md
//! [20-parquet-exporter.md § 3]: ../../../specs/20-parquet-exporter.md

use std::sync::Arc;

use crate::{
    Diagnostic, Severity,
    diagnostic::Diagnostic as Diag,
    ir::{
        Address, AttributeMap, BlockKind, Component, ComponentId, Expression, Local, ModuleCall,
        ModuleSource, Output, ProviderBlock, ProviderRef, Resource, ResourceKind, SymbolKind,
        Value, Variable,
    },
    loader::{RawBlock, RawComponent},
};

/// Project a [`RawComponent`] into a fully-typed [`Component`].
///
/// `component_id` is assigned by the orchestrator (1-based, per
/// [`ComponentId`]). The returned [`Component`] carries an empty
/// `terragrunt` field (populated in Phase 6) and `state_backend = None`
/// until either the `terraform { backend ... }` block extractor or the
/// Terragrunt resolver fills it.
///
/// Diagnostics surfaced by the projection (malformed addresses, unsupported
/// shapes) are appended to `out_diagnostics`.
#[must_use]
pub fn project_component(
    raw: &RawComponent,
    component_id: ComponentId,
    out_diagnostics: &mut Vec<Diagnostic>,
) -> Component {
    let mut variables: Vec<Variable> = Vec::new();
    let mut locals: Vec<Local> = Vec::new();
    let mut providers: Vec<ProviderBlock> = Vec::new();
    let mut resources: Vec<Resource> = Vec::new();
    let mut module_calls: Vec<ModuleCall> = Vec::new();
    let mut outputs: Vec<Output> = Vec::new();

    for block in &raw.raw_blocks {
        match block.kind {
            BlockKind::Resource | BlockKind::Data => {
                match project_resource(block, out_diagnostics) {
                    Some(r) => resources.push(r),
                    None => out_diagnostics.push(Diag::new(
                        Severity::Warn,
                        "TF1301",
                        format!(
                            "resource/data block missing labels in {}",
                            block.source.display()
                        ),
                    )),
                }
            }
            BlockKind::Provider => {
                if let Some(p) = project_provider(block, out_diagnostics) {
                    providers.push(p);
                }
            }
            BlockKind::Module => {
                if let Some(m) = project_module_call(block, out_diagnostics) {
                    module_calls.push(m);
                }
            }
            BlockKind::Variable => {
                if let Some(v) = project_variable(block) {
                    variables.push(v);
                }
            }
            BlockKind::Locals => project_locals_into(block, &mut locals),
            BlockKind::Output => {
                if let Some(o) = project_output(block) {
                    outputs.push(o);
                }
            }
            BlockKind::Terraform
            | BlockKind::Include
            | BlockKind::Generate
            | BlockKind::Dependency
            | BlockKind::Inputs
            | BlockKind::Unknown => {
                // Captured by other phases (Terragrunt, the terraform-block
                // backend extractor, etc.). Phase 3 skips them — they
                // round-trip through `raw_blocks` so later phases still see
                // them.
            }
        }
    }

    Component::builder()
        .id(component_id)
        .path(Arc::clone(&raw.path))
        .kind(raw.kind)
        .variables(variables)
        .locals(locals)
        .providers(providers)
        .resources(resources)
        .modules(module_calls)
        .outputs(outputs)
        .build()
}

fn project_resource(block: &RawBlock, diags: &mut Vec<Diagnostic>) -> Option<Resource> {
    let (type_label, name_label) = match (block.labels.first(), block.labels.get(1)) {
        (Some(t), Some(n)) => (Arc::clone(t), Arc::clone(n)),
        _ => return None,
    };
    let (kind, addr_str) = match block.kind {
        BlockKind::Resource => (ResourceKind::Managed, format!("{type_label}.{name_label}")),
        BlockKind::Data => (
            ResourceKind::Data,
            format!("data.{type_label}.{name_label}"),
        ),
        _ => return None,
    };
    let address = match Address::new(&addr_str) {
        Ok(a) => a,
        Err(err) => {
            diags.push(Diag::new(
                Severity::Warn,
                "TF1302",
                format!(
                    "invalid resource address `{addr_str}` in {}: {err}",
                    block.source.display()
                ),
            ));
            return None;
        }
    };

    let mut attributes: AttributeMap = Vec::with_capacity(block.body.len());
    let mut count_expr: Option<Expression> = None;
    let mut for_each_expr: Option<Expression> = None;
    let mut provider_ref: Option<ProviderRef> = None;
    let mut depends_on: Vec<Address> = Vec::new();

    for (key, value) in &block.body {
        match key.as_ref() {
            "count" => count_expr = Some(value.clone()),
            "for_each" => for_each_expr = Some(value.clone()),
            "provider" => {
                provider_ref = extract_provider_ref(value);
                if provider_ref.is_none() {
                    diags.push(Diag::new(
                        Severity::Trace,
                        "TF1303",
                        format!(
                            "ignored non-static provider expression on {addr_str} in {}",
                            block.source.display()
                        ),
                    ));
                }
            }
            "depends_on" => depends_on.extend(extract_depends_on(value, diags)),
            _ => attributes.push((Arc::clone(key), value.clone())),
        }
    }

    Some(
        Resource::builder()
            .address(address)
            .kind(kind)
            .type_(Arc::clone(&type_label))
            .name(Arc::clone(&name_label))
            .provider_ref(provider_ref)
            .count_expr(count_expr)
            .for_each_expr(for_each_expr)
            .depends_on(depends_on)
            .attributes(attributes)
            .span(block.span.clone())
            .build(),
    )
}

fn project_provider(block: &RawBlock, diags: &mut Vec<Diagnostic>) -> Option<ProviderBlock> {
    let local_name = block.labels.first().cloned().or_else(|| {
        diags.push(Diag::new(
            Severity::Warn,
            "TF1304",
            format!("provider block missing label in {}", block.source.display()),
        ));
        None
    })?;

    let mut alias: Option<Arc<str>> = None;
    let mut region_expr: Option<Expression> = None;
    let mut profile_expr: Option<Expression> = None;
    let mut raw: AttributeMap = Vec::with_capacity(block.body.len());

    for (key, value) in &block.body {
        match key.as_ref() {
            "alias" => {
                if let Expression::Literal(Value::Str(s)) = value {
                    alias = Some(Arc::clone(s));
                } else {
                    raw.push((Arc::clone(key), value.clone()));
                }
            }
            "region" => {
                region_expr = Some(value.clone());
                raw.push((Arc::clone(key), value.clone()));
            }
            "profile" => {
                profile_expr = Some(value.clone());
                raw.push((Arc::clone(key), value.clone()));
            }
            _ => raw.push((Arc::clone(key), value.clone())),
        }
    }

    Some(
        ProviderBlock::builder()
            .local_name(local_name)
            .alias(alias)
            .region_expr(region_expr)
            .profile_expr(profile_expr)
            .raw(raw)
            .span(block.span.clone())
            .build(),
    )
}

fn project_module_call(block: &RawBlock, diags: &mut Vec<Diagnostic>) -> Option<ModuleCall> {
    let name = block.labels.first()?;
    let addr_str = format!("module.{name}");
    let address = match Address::new(&addr_str) {
        Ok(a) => a,
        Err(err) => {
            diags.push(Diag::new(
                Severity::Warn,
                "TF1305",
                format!(
                    "invalid module address `{addr_str}` in {}: {err}",
                    block.source.display()
                ),
            ));
            return None;
        }
    };
    let mut source_raw: Arc<str> = Arc::from("");
    let mut inputs: AttributeMap = Vec::new();
    let mut count_expr: Option<Expression> = None;
    let mut for_each_expr: Option<Expression> = None;
    let mut providers: Vec<(Arc<str>, ProviderRef)> = Vec::new();

    for (key, value) in &block.body {
        match key.as_ref() {
            "source" => {
                if let Expression::Literal(Value::Str(s)) = value {
                    source_raw = Arc::clone(s);
                }
            }
            "count" => count_expr = Some(value.clone()),
            "for_each" => for_each_expr = Some(value.clone()),
            "providers" => providers.extend(extract_providers_map(value)),
            _ => inputs.push((Arc::clone(key), value.clone())),
        }
    }

    let source = ModuleSource::classify(source_raw.as_ref());
    Some(
        ModuleCall::builder()
            .address(address)
            .source_raw(source_raw)
            .source(source)
            .providers(providers)
            .inputs(inputs)
            .count_expr(count_expr)
            .for_each_expr(for_each_expr)
            .span(block.span.clone())
            .build(),
    )
}

fn project_variable(block: &RawBlock) -> Option<Variable> {
    let name = block.labels.first()?;
    let mut description: Option<Arc<str>> = None;
    let mut type_expr: Option<Expression> = None;
    let mut default: Option<Expression> = None;
    let mut sensitive = false;
    for (key, value) in &block.body {
        match key.as_ref() {
            "description" => {
                if let Expression::Literal(Value::Str(s)) = value {
                    description = Some(Arc::clone(s));
                }
            }
            "type" => type_expr = Some(value.clone()),
            "default" => default = Some(value.clone()),
            "sensitive" => {
                if let Expression::Literal(Value::Bool(b)) = value {
                    sensitive = *b;
                }
            }
            _ => {}
        }
    }
    Some(
        Variable::builder()
            .name(Arc::clone(name))
            .description(description)
            .type_expr(type_expr)
            .default(default)
            .sensitive(sensitive)
            .span(block.span.clone())
            .build(),
    )
}

fn project_locals_into(block: &RawBlock, out: &mut Vec<Local>) {
    for (key, value) in &block.body {
        out.push(
            Local::builder()
                .name(Arc::clone(key))
                .value(value.clone())
                .span(block.span.clone())
                .build(),
        );
    }
}

fn project_output(block: &RawBlock) -> Option<Output> {
    let name = block.labels.first()?;
    let mut value_expr: Option<Expression> = None;
    let mut description: Option<Arc<str>> = None;
    let mut sensitive = false;
    for (key, expr) in &block.body {
        match key.as_ref() {
            "value" => value_expr = Some(expr.clone()),
            "description" => {
                if let Expression::Literal(Value::Str(s)) = expr {
                    description = Some(Arc::clone(s));
                }
            }
            "sensitive" => {
                if let Expression::Literal(Value::Bool(b)) = expr {
                    sensitive = *b;
                }
            }
            _ => {}
        }
    }
    let value = value_expr?;
    Some(
        Output::builder()
            .name(Arc::clone(name))
            .value(value)
            .description(description)
            .sensitive(sensitive)
            .span(block.span.clone())
            .build(),
    )
}

/// Parse `provider = aws` or `provider = aws.<alias>` into a [`ProviderRef`].
///
/// Accepts only syntactic provider identifiers: a bare identifier (lowered
/// as [`SymbolKind::Other`]) or a single-dot traversal that the lowerer
/// classified as [`SymbolKind::Resource`] (the shape `<ident>.<ident>`
/// matches the resource-ref heuristic in
/// `crates/core/src/loader/lowering.rs::symbol_kind_for`). Rejects
/// `var.x` / `local.x` / `path.module` / `terraform.workspace` /
/// `dependency.x` / `each.value` because none of those name a provider.
fn extract_provider_ref(expr: &Expression) -> Option<ProviderRef> {
    let Expression::Unresolved(s) = expr else {
        return None;
    };
    if !matches!(s.kind, SymbolKind::Other | SymbolKind::Resource) {
        return None;
    }
    let source: &str = s.source.as_ref();
    if !is_provider_identifier(source) {
        return None;
    }
    let mut parts = source.splitn(2, '.');
    let local_name: Arc<str> = Arc::from(parts.next()?);
    let alias: Option<Arc<str>> = parts.next().map(Arc::from);
    Some(
        ProviderRef::builder()
            .local_name(local_name)
            .alias(alias)
            .span(s.span.clone())
            .build(),
    )
}

/// `^[a-z_][a-z0-9_]*(\.[a-z_][a-z0-9_]*)?$` — provider local name
/// optionally followed by `.alias`. Identifier rules mirror HCL's
/// (lowercase, digits, underscore; first char not a digit).
fn is_provider_identifier(s: &str) -> bool {
    fn is_segment(seg: &str) -> bool {
        let mut bytes = seg.bytes();
        match bytes.next() {
            Some(b) if b.is_ascii_lowercase() || b == b'_' => {}
            _ => return false,
        }
        bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }
    let mut parts = s.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    if !is_segment(first) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(second) => is_segment(second) && parts.next().is_none(),
    }
}

/// Parse `depends_on = [<addr>, <addr>, ...]` into a list of [`Address`].
///
/// Each element must be a static reference (an [`Expression::Unresolved`]
/// with an [`Address::address_hint`] that parses cleanly). Anything else is
/// dropped with a low-severity diagnostic — graph-phase code will pick up
/// the implicit references through `attribute_json` traversal.
fn extract_depends_on(expr: &Expression, diags: &mut Vec<Diagnostic>) -> Vec<Address> {
    let mut out: Vec<Address> = Vec::new();
    let elements: &[Expression] = match expr {
        Expression::Literal(Value::List(items)) => {
            for item in items {
                if let Value::Str(s) = item {
                    match Address::new(s.as_ref()) {
                        Ok(a) => out.push(a),
                        Err(err) => diags.push(Diag::new(
                            Severity::Trace,
                            "TF1306",
                            format!("depends_on entry rejected (`{s}`): {err}"),
                        )),
                    }
                }
            }
            return out;
        }
        Expression::Array(items) => items.as_slice(),
        _ => return out,
    };
    for item in elements {
        match item {
            Expression::Unresolved(s) => {
                if let Some(hint) = &s.address_hint {
                    out.push(hint.clone());
                } else if let Ok(addr) = Address::new(s.source.as_ref()) {
                    out.push(addr);
                } else {
                    diags.push(Diag::new(
                        Severity::Trace,
                        "TF1306",
                        format!("depends_on entry not a static reference: {}", s.source),
                    ));
                }
            }
            Expression::Literal(Value::Str(s)) => match Address::new(s.as_ref()) {
                Ok(a) => out.push(a),
                Err(err) => diags.push(Diag::new(
                    Severity::Trace,
                    "TF1306",
                    format!("depends_on entry rejected (`{s}`): {err}"),
                )),
            },
            _ => diags.push(Diag::new(
                Severity::Trace,
                "TF1306",
                "depends_on entry not a static reference",
            )),
        }
    }
    out
}

/// Parse `providers = { aws = aws.main }` into a vector of `(alias, ProviderRef)`.
fn extract_providers_map(expr: &Expression) -> Vec<(Arc<str>, ProviderRef)> {
    let mut out: Vec<(Arc<str>, ProviderRef)> = Vec::new();
    let Expression::Object(entries) = expr else {
        return out;
    };
    for (key, value) in entries {
        let key_str: Arc<str> = match key {
            Expression::Literal(Value::Str(s)) => Arc::clone(s),
            _ => continue,
        };
        if let Some(reference) = extract_provider_ref(value) {
            out.push((key_str, reference));
        }
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{
        ir::{ComponentKind, Span, Symbolic},
        loader::{HclEditLoader, LoaderLimits},
    };

    fn raw_from_source(src: &str) -> RawComponent {
        let path: Arc<Path> = Arc::from(Path::new("/tmp/x.tf"));
        let limits = LoaderLimits::default();
        let parsed = HclEditLoader.parse_bytes(src.as_bytes(), &path, &limits);
        let mut raw = RawComponent::new(Arc::from(Path::new("")), ComponentKind::Component);
        raw.raw_blocks.extend(parsed.blocks);
        raw.diagnostics.extend(parsed.diagnostics);
        raw
    }

    #[test]
    fn test_should_project_resource_with_count_and_provider() {
        let src = r#"
resource "aws_iam_role" "service" {
  provider = aws.main
  count    = 3
  name     = "x"
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        assert_eq!(comp.resources.len(), 1);
        let r = &comp.resources[0];
        assert_eq!(r.address.as_str(), "aws_iam_role.service");
        assert!(r.count_expr.is_some());
        let provider_ref = r.provider_ref.as_ref().expect("provider_ref");
        assert_eq!(provider_ref.local_name.as_ref(), "aws");
        assert_eq!(provider_ref.alias.as_deref(), Some("main"));
        assert!(r.attributes.iter().any(|(k, _)| k.as_ref() == "name"));
        assert!(r.attributes.iter().all(|(k, _)| k.as_ref() != "count"));
        assert!(r.attributes.iter().all(|(k, _)| k.as_ref() != "provider"));
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn test_should_project_data_block_with_data_prefix() {
        let src = r#"data "aws_caller_identity" "self" {}"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        assert_eq!(comp.resources.len(), 1);
        let r = &comp.resources[0];
        assert_eq!(r.kind, ResourceKind::Data);
        assert_eq!(r.address.as_str(), "data.aws_caller_identity.self");
    }

    #[test]
    fn test_should_project_provider_with_alias() {
        let src = r#"
provider "aws" {
  alias  = "main"
  region = "us-east-1"
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        assert_eq!(comp.providers.len(), 1);
        let p = &comp.providers[0];
        assert_eq!(p.local_name.as_ref(), "aws");
        assert_eq!(p.alias.as_deref(), Some("main"));
        assert!(p.region_expr.is_some());
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn test_should_project_module_call_with_source_and_inputs() {
        let src = r#"
module "vpc" {
  source = "../../modules/vpc"
  cidr   = "10.0.0.0/16"
  providers = {
    aws = aws.main
  }
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        assert_eq!(comp.modules.len(), 1);
        let m = &comp.modules[0];
        assert_eq!(m.address.as_str(), "module.vpc");
        assert_eq!(m.source_raw.as_ref(), "../../modules/vpc");
        assert!(matches!(m.source, ModuleSource::Local(_)));
        assert!(m.inputs.iter().any(|(k, _)| k.as_ref() == "cidr"));
        assert_eq!(m.providers.len(), 1);
        assert_eq!(m.providers[0].0.as_ref(), "aws");
        assert_eq!(m.providers[0].1.alias.as_deref(), Some("main"));
    }

    #[test]
    fn test_should_project_locals_block() {
        let src = r#"
locals {
  name   = "northwind"
  region = "us-west-2"
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        assert_eq!(comp.locals.len(), 2);
        let names: Vec<&str> = comp.locals.iter().map(|l| l.name.as_ref()).collect();
        assert_eq!(names, vec!["name", "region"]);
    }

    #[test]
    fn test_should_project_variable_with_default_and_sensitive() {
        let src = r#"
variable "db_password" {
  type      = string
  default   = "ignored-in-debug"
  sensitive = true
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        assert_eq!(comp.variables.len(), 1);
        let v = &comp.variables[0];
        assert!(v.sensitive);
        assert!(v.default.is_some());
        assert!(v.type_expr.is_some());
    }

    #[test]
    fn test_should_project_output_with_value() {
        let src = r#"
output "role_arn" {
  value = aws_iam_role.r.arn
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        assert_eq!(comp.outputs.len(), 1);
        assert_eq!(comp.outputs[0].name.as_ref(), "role_arn");
    }

    #[test]
    fn test_should_project_depends_on_static_addresses() {
        let src = r#"
resource "aws_iam_role" "r" {
  depends_on = [aws_iam_role.parent]
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        let r = &comp.resources[0];
        assert_eq!(r.depends_on.len(), 1);
        assert_eq!(r.depends_on[0].as_str(), "aws_iam_role.parent");
    }

    #[test]
    fn test_should_drop_non_static_provider_to_none() {
        let src = r#"
resource "aws_iam_role" "r" {
  provider = var.x
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        let r = &comp.resources[0];
        assert!(r.provider_ref.is_none());
        assert!(diags.iter().any(|d| d.severity == Severity::Trace));
    }

    #[test]
    fn test_should_extract_provider_ref_without_alias() {
        let s = Symbolic::builder()
            .kind(SymbolKind::Other)
            .source(Arc::<str>::from("aws"))
            .span(Span::synthetic())
            .build();
        let expr = Expression::Unresolved(s);
        let pr = extract_provider_ref(&expr).expect("provider ref");
        assert_eq!(pr.local_name.as_ref(), "aws");
        assert!(pr.alias.is_none());
    }

    #[test]
    fn test_should_reject_var_x_as_provider_ref() {
        let s = Symbolic::builder()
            .kind(SymbolKind::Var)
            .source(Arc::<str>::from("var.x"))
            .span(Span::synthetic())
            .build();
        let expr = Expression::Unresolved(s);
        assert!(extract_provider_ref(&expr).is_none());
    }

    #[test]
    fn test_should_reject_path_module_as_provider_ref() {
        // path.module / terraform.workspace / each.value / dependency.foo
        // all parse to non-Resource/Other SymbolKinds and must be rejected
        // even if their syntactic shape happens to look like `id.id`.
        for (kind, source) in [
            (SymbolKind::Path, "path.module"),
            (SymbolKind::Terraform, "terraform.workspace"),
            (SymbolKind::Iteration, "each.value"),
            (SymbolKind::TerragruntDependency, "dependency.vpc"),
        ] {
            let s = Symbolic::builder()
                .kind(kind)
                .source(Arc::<str>::from(source))
                .span(Span::synthetic())
                .build();
            assert!(
                extract_provider_ref(&Expression::Unresolved(s)).is_none(),
                "expected None for {source:?}"
            );
        }
    }

    #[test]
    fn test_should_reject_too_many_dot_segments_as_provider_ref() {
        // Three-part shape (`aws.main.something`) must not lift to
        // ProviderRef; that's a resource attribute reference.
        let s = Symbolic::builder()
            .kind(SymbolKind::Resource)
            .source(Arc::<str>::from("aws_iam_role.r.arn"))
            .span(Span::synthetic())
            .build();
        assert!(extract_provider_ref(&Expression::Unresolved(s)).is_none());
    }

    #[test]
    fn test_provider_identifier_charset() {
        assert!(is_provider_identifier("aws"));
        assert!(is_provider_identifier("aws.main"));
        assert!(is_provider_identifier("_foo"));
        assert!(!is_provider_identifier("Aws.main"));
        assert!(!is_provider_identifier("aws main"));
        assert!(!is_provider_identifier("aws."));
        assert!(!is_provider_identifier(".aws"));
        assert!(!is_provider_identifier("aws.main.extra"));
        assert!(!is_provider_identifier("3aws"));
    }

    #[test]
    fn test_should_project_resource_with_for_each_unresolved() {
        let src = r#"
resource "aws_iam_role" "r" {
  for_each = var.roles
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        let r = &comp.resources[0];
        assert!(matches!(r.for_each_expr, Some(Expression::Unresolved(_))));
    }

    #[test]
    fn test_should_skip_unknown_blocks_silently() {
        let src = r#"
terraform {
  required_version = ">= 1.5.0"
}
"#;
        let raw = raw_from_source(src);
        let mut diags = Vec::new();
        let comp = project_component(&raw, ComponentId::from_index(0), &mut diags);
        assert!(comp.resources.is_empty());
        assert!(comp.providers.is_empty());
        assert!(diags.is_empty(), "{diags:?}");
    }
}
