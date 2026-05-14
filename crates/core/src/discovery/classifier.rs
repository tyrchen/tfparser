//! Directory classifier.
//!
//! Implements the heuristics in [11-discovery.md § 3.2]. Decides whether a
//! given directory is a component, a module, an environments dir, or
//! something else, based on a **regex-grade scan** over the file bytes
//! (anchored line-start patterns; not a real parser).
//!
//! Per [11-discovery.md § 3.3] and [70-security.md § 3.4]: we use the
//! linear-time [`regex`] crate exclusively. The patterns sit behind a
//! [`regex::RegexSet`] so a single pass over each file's bytes covers all
//! probes.
//!
//! [11-discovery.md § 3.2]: ../../../specs/11-discovery.md
//! [11-discovery.md § 3.3]: ../../../specs/11-discovery.md
//! [70-security.md § 3.4]: ../../../specs/70-security.md

use std::{path::Path, sync::OnceLock};

use regex::RegexSet;

use super::{
    options::DiscoveryOptions,
    types::{DirKind, DiscoveredFile},
};
use crate::ir::FileExt;

/// Why the classifier picked the kind it did. Surfaced in
/// `Discovered.diagnostics` and in CLI verbose output for auditability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ClassificationReason {
    /// `terragrunt.hcl` containing an `include` block.
    TerragruntInclude,
    /// `.tf` file declaring `terraform { backend "..." {} }`.
    BackendBlock,
    /// `.tf` file declaring at least one `resource`/`data` block, and the
    /// dir does not match any `module_glob`.
    HasResources,
    /// Path matched the user-configured (or default) `module_glob`.
    ModuleGlob,
    /// `.tf` files declared `variable` blocks but no `resource`/`data`
    /// blocks (a common library-module shape).
    VariablesOnly,
    /// The directory is named `environments` and lives directly under the
    /// workspace root.
    EnvironmentsDir,
    /// The dir has only files / data / READMEs that we don't parse.
    DataOnly,
    /// Could not classify — emitted alongside [`DirKind::Other`].
    Unknown,
}

// ----------------------------------------------------------------------------
// Probe patterns — anchored line-start regexes per spec § 3.3.
// ----------------------------------------------------------------------------

const PATTERN_TERRAFORM_BLOCK: &str = r"(?m)^\s*terraform\s*\{";
const PATTERN_BACKEND_BLOCK: &str = r#"(?m)^\s*backend\s+"[^"]+"\s*\{"#;
const PATTERN_RESOURCE_BLOCK: &str = r#"(?m)^\s*resource\s+"[^"]+"\s+"[^"]+"\s*\{"#;
const PATTERN_DATA_BLOCK: &str = r#"(?m)^\s*data\s+"[^"]+"\s+"[^"]+"\s*\{"#;
const PATTERN_VARIABLE_BLOCK: &str = r#"(?m)^\s*variable\s+"[^"]+"\s*\{"#;
const PATTERN_INCLUDE_BLOCK: &str = r#"(?m)^\s*include(\s+"[^"]*")?\s*\{"#;

const IDX_TERRAFORM: usize = 0;
const IDX_BACKEND: usize = 1;
const IDX_RESOURCE: usize = 2;
const IDX_DATA: usize = 3;
const IDX_VARIABLE: usize = 4;
const IDX_INCLUDE: usize = 5;

/// Lazily-built shared `RegexSet`. Building it once per process saves the
/// (cheap but non-trivial) NFA construction on every discovered directory.
fn probe_set() -> &'static RegexSet {
    static PROBES: OnceLock<RegexSet> = OnceLock::new();
    PROBES.get_or_init(|| {
        // Building from a known-good set of constants — failure here means a
        // code-level regression. We surface it as the empty set so the
        // classifier degrades to "no match" rather than panicking.
        RegexSet::new([
            PATTERN_TERRAFORM_BLOCK,
            PATTERN_BACKEND_BLOCK,
            PATTERN_RESOURCE_BLOCK,
            PATTERN_DATA_BLOCK,
            PATTERN_VARIABLE_BLOCK,
            PATTERN_INCLUDE_BLOCK,
        ])
        .unwrap_or_else(|_| RegexSet::empty())
    })
}

/// What [`probe_file`] found in a single file's contents.
///
/// Six independent boolean signals, one per probe pattern. Modelled as
/// flags rather than a state-machine enum because all six can co-occur
/// (a single `terragrunt.hcl` may carry `terraform`, `backend`, and
/// `include` blocks at once).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub(super) struct FileSignals {
    pub terraform_block: bool,
    pub backend_block: bool,
    pub resource_block: bool,
    pub data_block: bool,
    pub variable_block: bool,
    pub include_block: bool,
}

impl FileSignals {
    pub(super) fn merge(&mut self, other: Self) {
        self.terraform_block |= other.terraform_block;
        self.backend_block |= other.backend_block;
        self.resource_block |= other.resource_block;
        self.data_block |= other.data_block;
        self.variable_block |= other.variable_block;
        self.include_block |= other.include_block;
    }
}

/// Scan `bytes` once with the shared `RegexSet` and return the set of
/// matching probes. Linear-time; bounded by the byte length per
/// [70-security.md § 3.4](../../../specs/70-security.md).
pub(super) fn probe_file(bytes: &[u8]) -> FileSignals {
    let set = probe_set();
    // `RegexSet::matches` works on &str. Try to interpret the bytes as UTF-8;
    // if that fails (binary file masquerading as `.tf`), no signals — the
    // loader will surface the parse error later.
    let Ok(s) = std::str::from_utf8(bytes) else {
        return FileSignals::default();
    };
    let matches = set.matches(s);
    FileSignals {
        terraform_block: matches.matched(IDX_TERRAFORM),
        backend_block: matches.matched(IDX_BACKEND),
        resource_block: matches.matched(IDX_RESOURCE),
        data_block: matches.matched(IDX_DATA),
        variable_block: matches.matched(IDX_VARIABLE),
        include_block: matches.matched(IDX_INCLUDE),
    }
}

/// Decide a directory's kind from the aggregate of its file signals plus the
/// contextual hints (whether the dir matches `module_glob`, whether it sits
/// at the workspace root, whether it carries a `terragrunt.hcl` file).
///
/// Returns `(DirKind, ClassificationReason, ambiguous)` where `ambiguous` is
/// `true` if the heuristics tripped multiple plausible classifications and
/// the caller should emit an ambiguity diagnostic.
pub(super) fn classify(
    rel_path: &Path,
    signals: FileSignals,
    files: &[DiscoveredFile],
    opts: &DiscoveryOptions,
    is_workspace_child_named_environments: bool,
) -> (DirKind, ClassificationReason, bool) {
    if is_workspace_child_named_environments {
        return (
            DirKind::Environments,
            ClassificationReason::EnvironmentsDir,
            false,
        );
    }

    let module_glob_match = opts.module_globs.is_match(rel_path);
    let has_terragrunt_hcl = files.iter().any(|f| f.ext == FileExt::TerragruntHcl);

    // Component checks (priority order per spec § 3.2):
    let (component_reason, component_picked) = if has_terragrunt_hcl && signals.include_block {
        (Some(ClassificationReason::TerragruntInclude), true)
    } else if signals.backend_block && signals.terraform_block {
        (Some(ClassificationReason::BackendBlock), true)
    } else if (signals.resource_block || signals.data_block) && !module_glob_match {
        (Some(ClassificationReason::HasResources), true)
    } else {
        (None, false)
    };

    if component_picked {
        // The spec resolves the (component vs module-glob) tie in favour of
        // component; remember the ambiguity for the caller.
        let ambiguous = component_picked && module_glob_match;
        return (
            DirKind::Component,
            component_reason.unwrap_or(ClassificationReason::Unknown),
            ambiguous,
        );
    }

    if module_glob_match {
        return (DirKind::Module, ClassificationReason::ModuleGlob, false);
    }

    let only_variables = signals.variable_block
        && !signals.resource_block
        && !signals.data_block
        && !signals.backend_block
        && !has_terragrunt_hcl;
    if only_variables {
        return (DirKind::Module, ClassificationReason::VariablesOnly, false);
    }

    let any_hcl = files.iter().any(|f| f.ext.is_hcl());
    if !any_hcl && !files.is_empty() {
        return (DirKind::Files, ClassificationReason::DataOnly, false);
    }

    (DirKind::Other, ClassificationReason::Unknown, false)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn file(rel: &str, ext: FileExt) -> DiscoveredFile {
        DiscoveredFile {
            path: Arc::from(Path::new(rel)),
            ext,
            size: 0,
        }
    }

    #[test]
    fn test_probe_detects_resource_block() {
        let s = b"\nresource \"aws_iam_role\" \"r\" {\n}\n";
        let sig = probe_file(s);
        assert!(sig.resource_block);
        assert!(!sig.backend_block);
    }

    #[test]
    fn test_probe_detects_terraform_with_backend() {
        let s = b"terraform {\n  backend \"s3\" {\n    bucket = \"x\"\n  }\n}\n";
        let sig = probe_file(s);
        assert!(sig.terraform_block);
        assert!(sig.backend_block);
    }

    #[test]
    fn test_probe_detects_variable_block() {
        let s = b"variable \"name\" {\n  type = string\n}\n";
        let sig = probe_file(s);
        assert!(sig.variable_block);
        assert!(!sig.resource_block);
    }

    #[test]
    fn test_probe_detects_terragrunt_include_with_label() {
        let s = b"include \"root\" {\n  path = find_in_parent_folders()\n}\n";
        let sig = probe_file(s);
        assert!(sig.include_block);
    }

    #[test]
    fn test_probe_detects_bare_include() {
        let s = b"include {\n  path = find_in_parent_folders()\n}\n";
        let sig = probe_file(s);
        assert!(sig.include_block);
    }

    #[test]
    fn test_probe_returns_no_signals_for_invalid_utf8() {
        let s = &[0xFFu8, 0xFE, 0xFD];
        let sig = probe_file(s);
        assert!(!sig.resource_block);
    }

    #[test]
    fn test_classify_terragrunt_dir_with_include_as_component() {
        let opts = DiscoveryOptions::defaults();
        let files = vec![file("terragrunt.hcl", FileExt::TerragruntHcl)];
        let signals = FileSignals {
            include_block: true,
            ..Default::default()
        };
        let (kind, reason, ambiguous) =
            classify(Path::new("services/api"), signals, &files, &opts, false);
        assert_eq!(kind, DirKind::Component);
        assert_eq!(reason, ClassificationReason::TerragruntInclude);
        assert!(!ambiguous);
    }

    #[test]
    fn test_classify_backend_dir_as_component() {
        let opts = DiscoveryOptions::defaults();
        let files = vec![file("main.tf", FileExt::Tf)];
        let signals = FileSignals {
            terraform_block: true,
            backend_block: true,
            ..Default::default()
        };
        let (kind, reason, _) = classify(Path::new("backend-only"), signals, &files, &opts, false);
        assert_eq!(kind, DirKind::Component);
        assert_eq!(reason, ClassificationReason::BackendBlock);
    }

    #[test]
    fn test_classify_resources_only_as_component() {
        let opts = DiscoveryOptions::defaults();
        let files = vec![file("main.tf", FileExt::Tf)];
        let signals = FileSignals {
            resource_block: true,
            ..Default::default()
        };
        let (kind, _, _) = classify(Path::new("svc"), signals, &files, &opts, false);
        assert_eq!(kind, DirKind::Component);
    }

    #[test]
    fn test_classify_module_glob_match_as_module() {
        let opts = DiscoveryOptions::defaults();
        let files = vec![file("main.tf", FileExt::Tf)];
        let signals = FileSignals {
            variable_block: true,
            ..Default::default()
        };
        let (kind, reason, _) =
            classify(Path::new("modules/iam-role"), signals, &files, &opts, false);
        assert_eq!(kind, DirKind::Module);
        assert_eq!(reason, ClassificationReason::ModuleGlob);
    }

    #[test]
    fn test_classify_variables_only_as_module() {
        let opts = DiscoveryOptions::defaults();
        let files = vec![file("variables.tf", FileExt::Tf)];
        let signals = FileSignals {
            variable_block: true,
            ..Default::default()
        };
        let (kind, reason, _) = classify(Path::new("library"), signals, &files, &opts, false);
        assert_eq!(kind, DirKind::Module);
        assert_eq!(reason, ClassificationReason::VariablesOnly);
    }

    #[test]
    fn test_classify_environments_dir_at_root() {
        let opts = DiscoveryOptions::defaults();
        let (kind, reason, _) = classify(
            Path::new("environments"),
            FileSignals::default(),
            &[],
            &opts,
            true,
        );
        assert_eq!(kind, DirKind::Environments);
        assert_eq!(reason, ClassificationReason::EnvironmentsDir);
    }

    #[test]
    fn test_classify_data_only_dir_as_files() {
        let opts = DiscoveryOptions::defaults();
        let files = vec![file("policy.json", FileExt::Json)];
        let (kind, reason, _) = classify(
            Path::new("svc/policies"),
            FileSignals::default(),
            &files,
            &opts,
            false,
        );
        assert_eq!(kind, DirKind::Files);
        assert_eq!(reason, ClassificationReason::DataOnly);
    }

    #[test]
    fn test_classify_unknown_dir_as_other() {
        let opts = DiscoveryOptions::defaults();
        let (kind, reason, _) = classify(
            Path::new("empty"),
            FileSignals::default(),
            &[],
            &opts,
            false,
        );
        assert_eq!(kind, DirKind::Other);
        assert_eq!(reason, ClassificationReason::Unknown);
    }

    #[test]
    fn test_classify_component_inside_modules_glob_marks_ambiguous() {
        // A `terragrunt.hcl include` under `modules/` is unusual but the
        // spec demands the component classification wins and an ambiguity
        // diagnostic is emitted.
        let opts = DiscoveryOptions::defaults();
        let files = vec![file("terragrunt.hcl", FileExt::TerragruntHcl)];
        let signals = FileSignals {
            include_block: true,
            ..Default::default()
        };
        let (kind, _, ambiguous) = classify(
            Path::new("modules/odd-component"),
            signals,
            &files,
            &opts,
            false,
        );
        assert_eq!(kind, DirKind::Component);
        assert!(ambiguous);
    }
}
