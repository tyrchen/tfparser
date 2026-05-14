//! Provider configuration IR — both the declaration ([`ProviderBlock`])
//! and the per-resource reference ([`ProviderRef`]).
//!
//! Per [80-glossary.md]: "Provider block / Provider ref / Provider source"
//! are three distinct things. They are three distinct types here.
//!
//! [80-glossary.md]: ../../specs/80-glossary.md

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::ir::{AttributeMap, Expression, Span};

/// Optional `assume_role { role_arn = "..." }` sub-block of a provider.
///
/// Used by [16-provider-resolver.md § 4](../../specs/16-provider-resolver.md)
/// to extract a cross-account account-id from the role ARN.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct AssumeRole {
    /// `role_arn` expression as written in source.
    pub role_arn_expr: Expression,
    /// Span of the `assume_role { ... }` block.
    pub span: Span,
}

/// A `provider "aws" {}` declaration.
///
/// Field order matches [10-data-model.md § 2.2]. Construct via the generated
/// [`ProviderBlockBuilder`].
///
/// `Debug` redacts `raw` past the first few keys to avoid logging
/// potentially-sensitive provider configuration at INFO level. See
/// [70-security.md § 5](../../specs/70-security.md).
#[derive(Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct ProviderBlock {
    /// Local name of the provider (e.g. `"aws"`).
    pub local_name: Arc<str>,

    /// Alias, if the block had `alias = "..."`.
    #[builder(default)]
    pub alias: Option<Arc<str>>,

    /// Provider source address from `required_providers` (e.g.
    /// `"hashicorp/aws"`), if known.
    #[builder(default)]
    pub source_addr: Option<Arc<str>>,

    /// `region = ...` expression.
    #[builder(default)]
    pub region_expr: Option<Expression>,

    /// `profile = ...` expression.
    #[builder(default)]
    pub profile_expr: Option<Expression>,

    /// Optional `assume_role { ... }` sub-block.
    #[builder(default)]
    pub assume_role: Option<AssumeRole>,

    /// Verbatim top-level attributes of the provider body.
    #[builder(default)]
    pub raw: AttributeMap,

    /// Span of the opening `provider` keyword.
    pub span: Span,
}

impl std::fmt::Debug for ProviderBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderBlock")
            .field("local_name", &self.local_name)
            .field("alias", &self.alias)
            .field("source_addr", &self.source_addr)
            .field("region_expr", &self.region_expr.as_ref().map(|_| "<expr>"))
            .field(
                "profile_expr",
                &self.profile_expr.as_ref().map(|_| "<expr>"),
            )
            .field(
                "assume_role",
                &self.assume_role.as_ref().map(|_| "<assume_role>"),
            )
            .field(
                "raw",
                &format!("<{} attributes (redacted)>", self.raw.len()),
            )
            .field("span", &self.span)
            .finish()
    }
}

/// A resource-side reference to a provider via `provider = aws.<alias>`.
///
/// `alias = None` means the default `aws` provider was used.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct ProviderRef {
    /// Local name of the provider (e.g. `"aws"`).
    pub local_name: Arc<str>,

    /// Alias, if the reference had one (e.g. `"main"`).
    #[builder(default)]
    pub alias: Option<Arc<str>>,

    /// Source span of the `provider = ...` attribute.
    pub span: Span,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ir::Span;

    #[test]
    fn test_should_build_minimal_provider_block() {
        let p = ProviderBlock::builder()
            .local_name(Arc::<str>::from("aws"))
            .span(Span::synthetic())
            .build();
        assert_eq!(&*p.local_name, "aws");
        assert!(p.alias.is_none());
        assert!(p.raw.is_empty());
    }

    #[test]
    fn test_should_redact_raw_attrs_in_debug() {
        let p = ProviderBlock::builder()
            .local_name(Arc::<str>::from("aws"))
            .raw(vec![(
                Arc::<str>::from("secret_access_key"),
                crate::ir::Expression::Literal(crate::ir::Value::Str(Arc::<str>::from(
                    "very-secret",
                ))),
            )])
            .span(Span::synthetic())
            .build();
        let debug = format!("{p:?}");
        assert!(!debug.contains("very-secret"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }

    #[test]
    fn test_should_serde_round_trip_provider_ref() {
        let r = ProviderRef {
            local_name: Arc::<str>::from("aws"),
            alias: Some(Arc::<str>::from("main")),
            span: Span::synthetic(),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: ProviderRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
