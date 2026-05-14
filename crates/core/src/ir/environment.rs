//! Workspace-wide environment values (`staging`, `production`, …).
//!
//! Per [00-prd.md] and [14-terragrunt.md], an environment carries the
//! cascade-resolved AWS account/region/profile plus a `locals` map sourced
//! from `terraform/environments/<env>.terragrunt.hcl`. The Terragrunt
//! resolver populates this in Phase 6.
//!
//! [00-prd.md]: ../../specs/00-prd.md
//! [14-terragrunt.md]: ../../specs/14-terragrunt.md

use std::{path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::ir::{AccountId, Map, Region};

/// A named environment (`staging`, `production`, etc.).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct Environment {
    /// Environment name (e.g. `staging`).
    pub name: Arc<str>,

    /// AWS account id (12 digits). `None` if the source did not declare one.
    #[builder(default)]
    pub aws_account_id: Option<AccountId>,

    /// AWS region. `None` if the source did not declare one.
    #[builder(default)]
    pub aws_region: Option<Region>,

    /// AWS shared-config profile name. `None` if the source did not declare
    /// one.
    #[builder(default)]
    pub aws_profile: Option<Arc<str>>,

    /// Path of the source file that defined this environment (typically
    /// `terraform/environments/<name>.terragrunt.hcl`).
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub source_file: Arc<Path>,

    /// Resolved environment-level locals (every other key from the source
    /// file).
    #[builder(default)]
    pub locals: Map,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::ir::{AccountId, Region};

    #[test]
    fn test_should_build_minimal_environment() {
        let env = Environment::builder()
            .name(Arc::<str>::from("staging"))
            .source_file(Arc::<Path>::from(PathBuf::from(
                "terraform/environments/staging.terragrunt.hcl",
            )))
            .build();
        assert_eq!(&*env.name, "staging");
        assert!(env.locals.is_empty());
        assert!(env.aws_account_id.is_none());
    }

    #[test]
    fn test_should_carry_validated_aws_metadata() {
        let env = Environment::builder()
            .name(Arc::<str>::from("production"))
            .aws_account_id(Some(AccountId::new("100000000001").unwrap()))
            .aws_region(Some(Region::new("us-west-2").unwrap()))
            .aws_profile(Some(Arc::<str>::from("primary")))
            .source_file(Arc::<Path>::from(PathBuf::from("a.hcl")))
            .build();
        assert_eq!(
            env.aws_account_id.as_ref().map(AccountId::as_str),
            Some("100000000001")
        );
        assert_eq!(
            env.aws_region.as_ref().map(Region::as_str),
            Some("us-west-2")
        );
    }

    #[test]
    fn test_should_serde_round_trip_environment() {
        let env = Environment::builder()
            .name(Arc::<str>::from("staging"))
            .aws_account_id(Some(AccountId::new("100000000001").unwrap()))
            .aws_region(Some(Region::new("us-west-2").unwrap()))
            .source_file(Arc::<Path>::from(PathBuf::from("a.hcl")))
            .build();
        let json = serde_json::to_string(&env).unwrap();
        let back: Environment = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }
}
