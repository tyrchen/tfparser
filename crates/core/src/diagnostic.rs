//! Non-fatal diagnostics attached to the workspace IR.
//!
//! Diagnostics are how the parser reports problems without aborting the
//! whole run. Per [70-security.md § 3.2], every configurable resource limit
//! has a corresponding [`LimitKind`] so a breach surfaces with structured
//! context rather than a free-form string.
//!
//! [70-security.md § 3.2]: ../../specs/70-security.md

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::ir::Span;

/// How serious a diagnostic is.
///
/// Severities are **not** error vs success — every variant is non-fatal at
/// the workspace level. They drive UI rendering and CLI exit codes (the
/// CLI may map `Error` severities to exit code 8 if `--fail-on-diagnostics`
/// is set; see [50-cli.md § 4.3](../../specs/50-cli.md)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Severity {
    /// Likely-not-actionable signal — e.g. "skipped unknown extension".
    Trace,
    /// Hint about something the user may want to fix.
    Info,
    /// Probably a problem; parse can still complete.
    Warn,
    /// Definite problem; the affected entity may be missing rows / fields.
    Error,
}

/// Which configurable limit fired.
///
/// Mirrors every cap pinned in [70-security.md § 3.2](../../specs/70-security.md).
/// New variants are additive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LimitKind {
    /// Per-file byte cap exceeded (loader / discovery).
    FileSize,
    /// Workspace-wide file-count cap exceeded (discovery).
    TotalFiles,
    /// Walk-depth cap exceeded (discovery).
    WalkDepth,
    /// Per-file block-count cap exceeded (loader).
    BlocksPerFile,
    /// AST/attribute depth cap exceeded (loader).
    AttributeDepth,
    /// Template-concat parts cap exceeded (loader).
    TemplateParts,
    /// Terragrunt include-depth cap exceeded.
    IncludeDepth,
    /// Evaluator iteration cap exceeded.
    EvalIterations,
    /// Function argument-count cap exceeded.
    FuncArgs,
    /// Rendered list length cap exceeded.
    ListLength,
    /// Rendered string size cap exceeded.
    StringSize,
    /// `count` / `for_each` expansion cap exceeded (graph phase).
    Expansion,
    /// Profile map / AWS config file size cap exceeded.
    ConfigFileSize,
}

/// A non-fatal diagnostic.
///
/// `code` is a stable, machine-readable identifier like `"TF1001"`; tooling
/// uses it to allowlist / ignore specific diagnostics without matching the
/// message string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Diagnostic {
    /// Severity.
    pub severity: Severity,
    /// Stable identifier (e.g. `"TF1001"`, `"TG2003"`).
    pub code: Arc<str>,
    /// Human-readable message; safe to log at the diagnostic's severity.
    pub message: Arc<str>,
    /// Optional location.
    #[builder(default)]
    pub span: Option<Span>,
    /// Optional fix-it / suggestion to surface in CLI output.
    #[builder(default)]
    pub suggestion: Option<Arc<str>>,
}

impl Diagnostic {
    /// Construct a diagnostic.
    #[must_use]
    pub fn new(
        severity: Severity,
        code: impl Into<Arc<str>>,
        message: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            severity,
            code: code.into(),
            message: message.into(),
            span: None,
            suggestion: None,
        }
    }

    /// Add a span.
    #[must_use]
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Add a suggestion.
    #[must_use]
    pub fn with_suggestion(mut self, suggestion: impl Into<Arc<str>>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_should_compose_diagnostic_with_builder_style() {
        let d = Diagnostic::new(Severity::Warn, "TF1001", "skipped malformed file")
            .with_suggestion("re-run with --verbose for the parse error");
        assert_eq!(d.severity, Severity::Warn);
        assert_eq!(&*d.code, "TF1001");
        assert!(d.suggestion.is_some());
    }

    #[test]
    fn test_should_serde_round_trip_diagnostic() {
        let d = Diagnostic::new(Severity::Error, "TG2003", "include cycle detected");
        let json = serde_json::to_string(&d).unwrap();
        let back: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn test_should_serialize_severity_kebab_case() {
        let json = serde_json::to_string(&Severity::Warn).unwrap();
        assert_eq!(json, "\"warn\"");
    }

    #[test]
    fn test_should_serialize_limit_kind_kebab_case() {
        let json = serde_json::to_string(&LimitKind::IncludeDepth).unwrap();
        assert_eq!(json, "\"include-depth\"");
    }

    #[test]
    fn test_should_order_severities_naturally() {
        assert!(Severity::Trace < Severity::Info);
        assert!(Severity::Info < Severity::Warn);
        assert!(Severity::Warn < Severity::Error);
    }
}
