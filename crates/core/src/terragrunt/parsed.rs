//! Internal projection of a parsed Terragrunt file.
//!
//! A `terragrunt.hcl` carries a mix of block kinds — `locals`, `inputs`,
//! `include`, `generate`, `dependency`, plus the `terraform { ... }` /
//! `remote_state { ... }` blocks the state-backend extractor uses. Here we
//! project the loader's flat `RawBlock` list into a structured
//! [`ParsedTerragrunt`] the resolver walks per file.
//!
//! Projection is **structural** — no evaluation, no path resolution.
//! Expressions are kept as-is for the resolver's evaluator pass to
//! consume.

use std::{path::Path, sync::Arc};

use crate::{
    Diagnostic, Severity,
    diagnostic::Diagnostic as Diag,
    ir::{AttributeMap, BlockKind, Expression, Local, Span},
    loader::{ParseBytesResult, RawBlock},
};

/// Projected Terragrunt file contents.
///
/// Constructed by [`project`] from the loader's [`ParseBytesResult`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub(super) struct ParsedTerragrunt {
    /// `locals { ... }` entries, flattened across all `locals` blocks.
    pub locals: Vec<Local>,
    /// Right-hand side of the top-level `inputs = { ... }` block, if any.
    pub inputs: Option<AttributeMap>,
    /// `include "name" { ... }` blocks.
    pub includes: Vec<IncludeBlockRaw>,
    /// `generate "label" { ... }` blocks.
    pub generates: Vec<GenerateBlockRaw>,
    /// `dependency "name" { ... }` blocks.
    pub dependencies: Vec<DependencyBlockRaw>,
    /// `remote_state { ... }` blocks (Terragrunt's native state-backend
    /// shape); attribute map kept verbatim.
    pub remote_state: Option<AttributeMap>,
    /// `terraform { ... }` blocks. Used by the state-backend extractor.
    pub terraform: Vec<AttributeMap>,
    /// Per-file projection diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Raw form of an `include "name" { path = <expr>; merge_strategy? }` block.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub(super) struct IncludeBlockRaw {
    /// `name` label (e.g. `"root"`). Empty when the block had no label.
    pub label: Arc<str>,
    /// `path = <expr>` — must evaluate to a string before the resolver can
    /// recurse.
    pub path_expr: Option<Expression>,
    /// `merge_strategy = <expr>` — optional; defaults to `deep_map_only`.
    pub merge_strategy_expr: Option<Expression>,
    /// Span of the `include` keyword.
    pub span: Span,
}

/// Raw form of a `generate "label" { path, if_exists, contents } block`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub(super) struct GenerateBlockRaw {
    /// Block label (e.g. `"backend"`).
    pub label: Arc<str>,
    /// Attribute map, kept verbatim for the resolver's expression pass.
    pub attrs: AttributeMap,
    /// Span of the `generate` keyword.
    pub span: Span,
}

/// Raw form of a `dependency "name" { config_path, mock_outputs? }` block.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub(super) struct DependencyBlockRaw {
    /// Block label.
    pub name: Arc<str>,
    /// Attribute map kept verbatim.
    pub attrs: AttributeMap,
    /// Span.
    pub span: Span,
}

/// Project a parsed file's [`ParseBytesResult`] into a [`ParsedTerragrunt`].
///
/// `source_path` is recorded on synthetic spans only (the loader's spans
/// already carry the file path).
pub(super) fn project(parsed: &ParseBytesResult, _source_path: &Arc<Path>) -> ParsedTerragrunt {
    let mut out = ParsedTerragrunt {
        diagnostics: parsed.diagnostics.clone(),
        ..ParsedTerragrunt::default()
    };

    for block in &parsed.blocks {
        match block.kind {
            BlockKind::Locals => {
                for (name, value) in &block.body {
                    out.locals.push(
                        Local::builder()
                            .name(Arc::clone(name))
                            .value(value.clone())
                            .span(block.span.clone())
                            .build(),
                    );
                }
            }
            BlockKind::Inputs => {
                // The loader currently lowers `inputs = { ... }` as a
                // top-level attribute (Unknown block with one body entry),
                // but in case it ever lands here we take the attrs as-is.
                out.inputs = Some(block.body.clone());
            }
            BlockKind::Include => {
                project_include(block, &mut out);
            }
            BlockKind::Generate => {
                project_generate(block, &mut out);
            }
            BlockKind::Dependency => {
                project_dependency(block, &mut out);
            }
            BlockKind::Terraform => {
                out.terraform.push(block.body.clone());
            }
            BlockKind::Unknown => {
                // Top-level `inputs = { ... }` lowers to a single Unknown
                // block with one attribute named `inputs`. Hoist it.
                if let Some((key, value)) = block.body.first()
                    && key.as_ref() == "inputs"
                    && let Expression::Object(_) | Expression::Literal(_) = value
                {
                    out.inputs = Some(object_to_attribute_map(value));
                } else if let Some((key, value)) = block.body.first()
                    && key.as_ref() == "remote_state"
                {
                    out.remote_state = Some(object_to_attribute_map(value));
                }
                // Anything else (`name = "value"` top-level attrs) the
                // resolver currently does not need; left silent.
            }
            _ => {
                // Other block kinds (resource/data/provider/variable/...
                // inside terragrunt.hcl) are non-canonical; record a
                // diagnostic and skip.
                out.diagnostics.push(Diag::new(
                    Severity::Trace,
                    "TG2101",
                    format!(
                        "unexpected block kind `{:?}` in terragrunt file; ignored",
                        block.kind
                    ),
                ));
            }
        }
    }

    out
}

fn project_include(block: &RawBlock, out: &mut ParsedTerragrunt) {
    let label = block
        .labels
        .first()
        .cloned()
        .unwrap_or_else(|| Arc::from(""));
    let mut path_expr: Option<Expression> = None;
    let mut merge_strategy_expr: Option<Expression> = None;
    for (k, v) in &block.body {
        match k.as_ref() {
            "path" => path_expr = Some(v.clone()),
            "merge_strategy" => merge_strategy_expr = Some(v.clone()),
            _ => {}
        }
    }
    out.includes.push(IncludeBlockRaw {
        label,
        path_expr,
        merge_strategy_expr,
        span: block.span.clone(),
    });
}

fn project_generate(block: &RawBlock, out: &mut ParsedTerragrunt) {
    let label = block
        .labels
        .first()
        .cloned()
        .unwrap_or_else(|| Arc::from(""));
    out.generates.push(GenerateBlockRaw {
        label,
        attrs: block.body.clone(),
        span: block.span.clone(),
    });
}

fn project_dependency(block: &RawBlock, out: &mut ParsedTerragrunt) {
    let name = block
        .labels
        .first()
        .cloned()
        .unwrap_or_else(|| Arc::from(""));
    out.dependencies.push(DependencyBlockRaw {
        name,
        attrs: block.body.clone(),
        span: block.span.clone(),
    });
}

/// The HCL loader represents `inputs = { ... }` as a one-attribute
/// `Unknown` block whose value is an [`Expression::Object`]. To produce an
/// `AttributeMap` for further processing we walk the object entries and,
/// for every entry whose key is a literal string, push `(key, value)`.
/// Non-string keys are dropped (they could appear via `(local.x) = "..."`
/// but are not Terragrunt-canonical for `inputs`).
fn object_to_attribute_map(value: &Expression) -> AttributeMap {
    match value {
        Expression::Object(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                Expression::Literal(crate::ir::Value::Str(s)) => Some((Arc::clone(s), v.clone())),
                _ => None,
            })
            .collect(),
        Expression::Literal(crate::ir::Value::Map(entries)) => entries
            .iter()
            .map(|(k, v)| (Arc::clone(k), Expression::Literal(v.clone())))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]
mod tests {
    use super::*;
    use crate::{
        ir::Span,
        loader::{HclEditLoader, LoaderLimits, ParseBytesResult},
    };

    fn parse(text: &str) -> ParseBytesResult {
        let path: Arc<Path> = Arc::from(Path::new("terragrunt.hcl"));
        HclEditLoader::new().parse_bytes(text.as_bytes(), &path, &LoaderLimits::default())
    }

    #[test]
    fn test_should_project_locals_block() {
        let parsed = parse("locals { region = \"us-east-2\" }");
        let path: Arc<Path> = Arc::from(Path::new("/x/terragrunt.hcl"));
        let pt = project(&parsed, &path);
        assert_eq!(pt.locals.len(), 1);
        assert_eq!(&*pt.locals[0].name, "region");
    }

    #[test]
    fn test_should_project_include_with_label() {
        let parsed = parse("include \"root\" { path = \"x\" }");
        let path: Arc<Path> = Arc::from(Path::new("/x/terragrunt.hcl"));
        let pt = project(&parsed, &path);
        assert_eq!(pt.includes.len(), 1);
        assert_eq!(&*pt.includes[0].label, "root");
        assert!(pt.includes[0].path_expr.is_some());
    }

    #[test]
    fn test_should_project_generate_block() {
        let parsed = parse(
            "generate \"backend\" {\n  path = \"backend.tf\"\n  if_exists = \
             \"overwrite_terragrunt\"\n  contents = \"terraform {}\"\n}",
        );
        let path: Arc<Path> = Arc::from(Path::new("/x/terragrunt.hcl"));
        let pt = project(&parsed, &path);
        assert_eq!(pt.generates.len(), 1);
        assert_eq!(&*pt.generates[0].label, "backend");
    }

    #[test]
    fn test_should_project_dependency_block() {
        let parsed = parse("dependency \"vpc\" { config_path = \"../net\" }");
        let path: Arc<Path> = Arc::from(Path::new("/x/terragrunt.hcl"));
        let pt = project(&parsed, &path);
        assert_eq!(pt.dependencies.len(), 1);
        assert_eq!(&*pt.dependencies[0].name, "vpc");
    }

    #[test]
    fn test_should_project_inputs_attribute() {
        // Top-level `inputs = { foo = "bar" }` is lowered as an Unknown
        // block whose body has a single `inputs` key whose value is the
        // object expression.
        let parsed = parse("inputs = { foo = \"bar\" }");
        let path: Arc<Path> = Arc::from(Path::new("/x/terragrunt.hcl"));
        let pt = project(&parsed, &path);
        let inputs = pt.inputs.expect("inputs projected");
        assert_eq!(inputs.len(), 1);
        assert_eq!(&*inputs[0].0, "foo");
    }

    #[test]
    fn test_span_synthetic_returns_synthetic_marker() {
        // Sanity check used by parsed.diagnostics surfaces (not under
        // test elsewhere). Just confirms the helper compiles in this
        // module.
        let s = Span::synthetic();
        assert!(s.file.as_os_str().is_empty());
    }
}
