//! Source file classification used during discovery and loading.
//!
//! Per [11-discovery.md § 2], every file the workspace walker emits is
//! classified by extension. Anything outside [`FileExt`] is silently skipped
//! at discovery time.
//!
//! [11-discovery.md § 2]: ../../specs/11-discovery.md

use std::{path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

/// Recognised source-file extensions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FileExt {
    /// `.tf` — Terraform configuration.
    Tf,
    /// `.tfvars` — Terraform variable values.
    Tfvars,
    /// `.hcl` — generic HCL (non-Terragrunt).
    Hcl,
    /// `terragrunt.hcl` and other Terragrunt-shaped `.hcl` files at the
    /// top level of a component dir.
    TerragruntHcl,
    /// `.json` — JSON fragments referenced by Terraform configurations
    /// (e.g. IAM policy files in a `files/` subdir).
    Json,
}

impl FileExt {
    /// Classify a path by its filename. Returns `None` for unrecognised
    /// extensions.
    ///
    /// A file named exactly `terragrunt.hcl` is classified as
    /// [`FileExt::TerragruntHcl`]. Other `.hcl` files (e.g.
    /// `common.terragrunt.hcl`, `staging.terragrunt.hcl`, `root.hcl`) are
    /// classified as [`FileExt::Hcl`] — the Terragrunt resolver decides
    /// downstream whether to treat them as Terragrunt-shaped based on
    /// content.
    #[must_use]
    pub fn classify(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        if name == "terragrunt.hcl" {
            return Some(Self::TerragruntHcl);
        }
        let ext = path.extension()?.to_str()?;
        match ext {
            "tf" => Some(Self::Tf),
            "tfvars" => Some(Self::Tfvars),
            "hcl" => Some(Self::Hcl),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    /// Whether this extension carries HCL syntax (i.e. should be fed to the
    /// HCL loader).
    #[must_use]
    pub const fn is_hcl(self) -> bool {
        matches!(
            self,
            Self::Tf | Self::Hcl | Self::TerragruntHcl | Self::Tfvars
        )
    }
}

/// A single source file inside a [`Component`](crate::ir::Component).
///
/// Cheap to clone — the path is `Arc`-shared with the rest of the IR.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TypedBuilder)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
#[builder(field_defaults(setter(into)))]
pub struct SourceFile {
    /// Path of the file, relative to the workspace root.
    #[serde(with = "crate::ir::path_serde::arc_path")]
    pub path: Arc<Path>,
    /// Classified extension.
    pub ext: FileExt,
    /// Size in bytes, as observed during discovery.
    pub size: u64,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn test_should_classify_terraform_extensions() {
        assert_eq!(
            FileExt::classify(&PathBuf::from("main.tf")),
            Some(FileExt::Tf)
        );
        assert_eq!(
            FileExt::classify(&PathBuf::from("prod.tfvars")),
            Some(FileExt::Tfvars)
        );
        assert_eq!(
            FileExt::classify(&PathBuf::from("root.hcl")),
            Some(FileExt::Hcl)
        );
        assert_eq!(
            FileExt::classify(&PathBuf::from("path/to/terragrunt.hcl")),
            Some(FileExt::TerragruntHcl)
        );
        assert_eq!(
            FileExt::classify(&PathBuf::from("policy.json")),
            Some(FileExt::Json)
        );
    }

    #[test]
    fn test_should_reject_unknown_extension() {
        assert!(FileExt::classify(&PathBuf::from("README.md")).is_none());
    }

    #[test]
    fn test_should_classify_hcl_files_as_hcl_input() {
        for ext in [
            FileExt::Tf,
            FileExt::Tfvars,
            FileExt::Hcl,
            FileExt::TerragruntHcl,
        ] {
            assert!(ext.is_hcl(), "{ext:?} should be HCL-shaped");
        }
        assert!(!FileExt::Json.is_hcl());
    }
}
