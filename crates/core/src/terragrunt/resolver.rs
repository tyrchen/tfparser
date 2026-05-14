//! Top-level Terragrunt resolver.
//!
//! [`TerragruntResolver::resolve`] is the Phase 6 entry point. Given a
//! component directory and a [`TgContext`], it walks the `include` load
//! chain, resolves Terragrunt-specific functions, applies the configured
//! merge strategy, evaluates the resulting `locals`/`inputs`, and emits a
//! [`TerragruntConfig`] suitable for the evaluator's `cascade_locals`
//! and `repo_vars` (per [13-evaluator.md]).
//!
//! Per [14-terragrunt.md § 2-3].
//!
//! [14-terragrunt.md § 2-3]: ../../../specs/14-terragrunt.md
//! [13-evaluator.md]: ../../../specs/13-evaluator.md

// The resolver's hot-path functions are spec-driven and not amenable to
// extraction without losing the locality the spec § 3 walks step by step.
// Per CLAUDE.md § Code Style ("Unless absolutely necessary, function should
// not be more than 150 lines"), 100–150-line spec-walks are acceptable; we
// silence the clippy::too_many_lines pedantic lint module-wide rather than
// invent intermediate types only the resolver would use.
#![allow(clippy::too_many_lines)]

use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

use dashmap::DashMap;

use crate::{
    Diagnostic, LimitKind, Result, Severity,
    diagnostic::Diagnostic as Diag,
    eval::{CallCx, EvalLimits, FuncRegistry, FuncRegistryBuilder, HclFunc},
    ir::{
        AttributeMap, DependencyBlock, Expression, GenerateBlock, IncludePath, Map, StateBackend,
        TerragruntConfig, Value,
    },
    loader::{HclEditLoader, LoaderLimits},
    terragrunt::{
        context::TgContext,
        funcs::{
            FindInParentFoldersFn, FindInParentFoldersFromFn, GetParentTerragruntDirFn,
            GetRepoRootFn, GetTerragruntDirFn, PathRelativeFromIncludeFn, PathRelativeToIncludeFn,
            TgState, TryFn,
        },
        merge::{MergeStrategy, merge_locals},
        parsed::{self, ParsedTerragrunt},
    },
    util::paths::{self, SymlinkPolicy},
};

/// Trait every Terragrunt resolver implements. Phase 6 ships exactly one
/// implementation: [`FsTerragruntResolver`].
pub trait TerragruntResolver: Send + Sync + std::fmt::Debug {
    /// Resolve the Terragrunt configuration at `component_dir` (the dir
    /// containing the component's `terragrunt.hcl`).
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error`] only on fatal I/O failures the resolver
    /// cannot continue past (e.g. the component dir disappears mid-walk).
    /// Per-include cycles, path-escapes, function errors are surfaced as
    /// [`Diagnostic`]s on the returned [`TerragruntConfig`].
    fn resolve(&self, component_dir: &Path, ctx: &TgContext) -> Result<TerragruntConfig>;
}

/// Default resolver — reads `terragrunt.hcl` files from the filesystem
/// and threads them through the merge cascade.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct FsTerragruntResolver;

impl FsTerragruntResolver {
    /// Construct a default resolver.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Memoised parse-and-resolve result. Stored under canonical path in the
/// per-call [`DashMap`] so any `read_terragrunt_config` call site that
/// re-references the same file gets the cached body.
#[derive(Clone, Debug)]
pub(super) struct ResolvedTerragrunt {
    pub locals: Map,
    pub inputs: Map,
}

impl TerragruntResolver for FsTerragruntResolver {
    fn resolve(&self, component_dir: &Path, ctx: &TgContext) -> Result<TerragruntConfig> {
        let component_dir_arc: Arc<Path> = Arc::from(component_dir);

        // Validate `component_dir` is inside the workspace root.
        if paths::canonicalize_inside(component_dir, &ctx.workspace_root, SymlinkPolicy::Follow)
            .is_err()
        {
            return Ok(TerragruntConfig::builder()
                .component_dir(Arc::clone(&component_dir_arc))
                .diagnostics(vec![Diag::new(
                    Severity::Warn,
                    "TG2001",
                    format!(
                        "component dir `{}` is outside workspace root `{}`",
                        component_dir.display(),
                        ctx.workspace_root.display()
                    ),
                )])
                .build());
        }

        // Per-resolution shared state — the TG funcs close over an `Arc`
        // of this so every function sees the live workspace root,
        // component dir, and active include.
        let tg_state = Arc::new(TgState::new(
            Arc::clone(&ctx.workspace_root),
            Arc::clone(&component_dir_arc),
        ));
        let memo: Arc<DashMap<Arc<Path>, Arc<ResolvedTerragrunt>>> = Arc::new(DashMap::new());
        let stack: Arc<std::sync::Mutex<Vec<Arc<Path>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let inflight: Arc<DashMap<Arc<Path>, ()>> = Arc::new(DashMap::new());

        let mut diagnostics: Vec<Diagnostic> = Vec::new();
        let loader = HclEditLoader::new();
        let loader_limits = LoaderLimits::default();
        let eval_limits = EvalLimits::default();

        // `ReadTerragruntConfigFn` recursively reduces parent locals — and
        // those locals may themselves call `read_terragrunt_config`,
        // `find_in_parent_folders`, etc. To dispatch correctly the function
        // needs the *same* TG registry it lives in. We break the cycle via
        // an `Arc<OnceLock<Arc<FuncRegistry>>>` populated immediately after
        // the registry is built (F-021).
        let registry_slot: Arc<OnceLock<Arc<FuncRegistry>>> = Arc::new(OnceLock::new());
        let funcs = Arc::new(build_func_registry(
            Arc::clone(&tg_state),
            Arc::clone(&memo),
            Arc::clone(&stack),
            Arc::clone(&inflight),
            Arc::clone(&registry_slot),
            ctx,
            loader,
            loader_limits,
            eval_limits,
        ));
        let _ = registry_slot.set(Arc::clone(&funcs));

        let tg_hcl = component_dir_arc.join("terragrunt.hcl");
        let Some(parsed_root) = read_and_project(
            &tg_hcl,
            &ctx.workspace_root,
            loader,
            &loader_limits,
            &mut diagnostics,
        ) else {
            // No terragrunt.hcl in the component dir → return an empty
            // config. The caller (orchestrator) decides whether to feed
            // it to the evaluator.
            return Ok(TerragruntConfig::builder()
                .component_dir(Arc::clone(&component_dir_arc))
                .diagnostics(diagnostics)
                .build());
        };

        // Resolve the include chain, deepest-first list.
        let mut visited: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        let chain = resolve_include_chain(
            &parsed_root,
            &tg_hcl,
            ctx,
            &funcs,
            &tg_state,
            loader,
            &loader_limits,
            &eval_limits,
            &mut visited,
            0,
            &mut diagnostics,
        );

        // Apply the merge cascade. Deepest-first means we walk the chain
        // in order and merge each parent into the next child.
        let merged = apply_cascade(&chain, &mut diagnostics);

        // Evaluate the merged locals using a recursive worklist (locals
        // can reference each other, e.g. `merged_vars = merge(env_vars.locals, ...)`).
        let effective_locals = evaluate_locals(
            &merged.locals,
            &funcs,
            &tg_state,
            &ctx.workspace_root,
            &ctx.env_var_mode,
            &eval_limits,
        );

        // Inputs: take the merged inputs and reduce with effective locals in scope.
        let inputs = if let Some(inputs_attrs) = &merged.inputs {
            evaluate_inputs(
                inputs_attrs,
                &effective_locals,
                &funcs,
                &tg_state,
                &ctx.workspace_root,
                &ctx.env_var_mode,
                &eval_limits,
            )
        } else {
            Map::new()
        };

        // Reduce generate / dependency attribute expressions against the
        // effective locals so heredoc contents with `${local.X}`
        // interpolations collapse to a single Literal(Str) we can capture.
        let reduced_generates = reduce_generates(
            &merged.generates,
            &effective_locals,
            &funcs,
            &ctx.workspace_root,
            &ctx.env_var_mode,
            &eval_limits,
        );
        let reduced_dependencies = reduce_dependencies(
            &merged.dependencies,
            &effective_locals,
            &funcs,
            &ctx.workspace_root,
            &ctx.env_var_mode,
            &eval_limits,
        );

        let generates = build_generates(&reduced_generates);
        let dependencies = build_dependencies(&reduced_dependencies, &component_dir_arc);
        let state_backend = extract_state_backend(&merged, loader, &loader_limits);

        // Build the IncludePath list from the chain (deepest last per spec).
        let includes: Vec<IncludePath> = chain
            .iter()
            .filter_map(|entry| {
                entry.include_origin.as_ref().map(|origin| {
                    IncludePath::builder()
                        .path(Arc::clone(origin))
                        .label(entry.include_label.clone())
                        .span(entry.span.clone())
                        .build()
                })
            })
            .collect();

        // Append any diagnostics produced by `read_terragrunt_config` calls
        // that landed in the memo (they accumulate in the resolved entry,
        // not in the surrounding scope).
        Ok(TerragruntConfig::builder()
            .component_dir(component_dir_arc)
            .effective_locals(effective_locals)
            .inputs(inputs)
            .includes(includes)
            .generates(generates)
            .dependencies(dependencies)
            .state_backend(state_backend)
            .diagnostics(diagnostics)
            .build())
    }
}

/// One link in the include load chain — the parsed Terragrunt body plus
/// metadata for the merge step.
#[derive(Debug)]
struct ChainEntry {
    parsed: ParsedTerragrunt,
    merge_strategy: MergeStrategy,
    /// Absolute path of the include's source file (the parent), or `None`
    /// for the child terragrunt.hcl at the chain's tail.
    include_origin: Option<Arc<Path>>,
    include_label: Option<Arc<str>>,
    span: crate::ir::Span,
}

/// Resolve the include load chain starting from `child`.
///
/// The returned vector is in **deepest-first** order: the parent(s) come
/// before the child. The child itself is the last entry.
///
/// `visited` is a *shared* set threaded through the recursion so cycles
/// across multiple recursion levels are caught — `a includes b includes a`
/// would otherwise overflow the stack because each recursive call would
/// start with a fresh local set.
#[allow(clippy::too_many_arguments)]
fn resolve_include_chain(
    child: &ParsedTerragrunt,
    child_path: &Path,
    ctx: &TgContext,
    funcs: &Arc<FuncRegistry>,
    tg_state: &Arc<TgState>,
    loader: HclEditLoader,
    loader_limits: &LoaderLimits,
    eval_limits: &EvalLimits,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
    depth: u32,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ChainEntry> {
    let mut chain: Vec<ChainEntry> = Vec::new();
    if depth >= ctx.max_include_depth {
        diagnostics.push(Diag::limit(
            LimitKind::IncludeDepth,
            "TG2002",
            format!(
                "terragrunt include depth cap ({}) exceeded",
                ctx.max_include_depth
            ),
        ));
        chain.push(ChainEntry {
            parsed: child.clone(),
            merge_strategy: MergeStrategy::DeepMapOnly,
            include_origin: None,
            include_label: None,
            span: crate::ir::Span::synthetic(),
        });
        return chain;
    }
    if let Ok(canonical) =
        paths::canonicalize_inside(child_path, &ctx.workspace_root, SymlinkPolicy::Follow)
    {
        visited.insert(canonical);
    }

    for include in &child.includes {
        let Some(path_expr) = &include.path_expr else {
            diagnostics.push(
                Diag::new(
                    Severity::Warn,
                    "TG2003",
                    format!("include `{}` missing `path = ...` attribute", include.label),
                )
                .with_span(include.span.clone()),
            );
            continue;
        };
        let Some(path_value) = reduce_to_string(
            path_expr,
            &Map::new(),
            funcs,
            tg_state,
            &ctx.workspace_root,
            &ctx.env_var_mode,
            eval_limits,
        ) else {
            diagnostics.push(
                Diag::new(
                    Severity::Warn,
                    "TG2004",
                    format!(
                        "include `{}` path expression did not reduce to a string",
                        include.label
                    ),
                )
                .with_span(include.span.clone()),
            );
            continue;
        };
        let candidate = Path::new(path_value.as_ref()).to_path_buf();
        let canonical = match paths::canonicalize_inside(
            &candidate,
            &ctx.workspace_root,
            SymlinkPolicy::Follow,
        ) {
            Ok(p) => p,
            Err(err) => {
                diagnostics.push(
                    Diag::new(
                        Severity::Warn,
                        "TG2005",
                        format!(
                            "include `{}` path `{}` did not resolve under workspace root: {err}",
                            include.label,
                            candidate.display()
                        ),
                    )
                    .with_span(include.span.clone()),
                );
                continue;
            }
        };
        if !visited.insert(canonical.clone()) {
            diagnostics.push(
                Diag::new(
                    Severity::Warn,
                    "TG2006",
                    format!(
                        "terragrunt include cycle detected at `{}`",
                        canonical.display()
                    ),
                )
                .with_span(include.span.clone()),
            );
            continue;
        }
        let Some(parent_parsed) = read_and_project(
            &canonical,
            &ctx.workspace_root,
            loader,
            loader_limits,
            diagnostics,
        ) else {
            continue;
        };

        // Update active include for the funcs that consult it.
        if let Ok(mut g) = tg_state.active_include.lock() {
            *g = Some(Arc::from(canonical.as_path()));
        }

        // Recurse on the parent's own includes before merging this one.
        let mut sub = resolve_include_chain(
            &parent_parsed,
            &canonical,
            ctx,
            funcs,
            tg_state,
            loader,
            loader_limits,
            eval_limits,
            visited,
            depth + 1,
            diagnostics,
        );
        chain.append(&mut sub);

        let strategy = parse_merge_strategy(include.merge_strategy_expr.as_ref());
        chain.push(ChainEntry {
            parsed: parent_parsed,
            merge_strategy: strategy,
            include_origin: Some(Arc::from(canonical.as_path())),
            include_label: Some(Arc::clone(&include.label)),
            span: include.span.clone(),
        });
    }

    // Append the child as the tail of the chain (deepest last).
    chain.push(ChainEntry {
        parsed: child.clone(),
        merge_strategy: MergeStrategy::DeepMapOnly,
        include_origin: None,
        include_label: None,
        span: child
            .locals
            .first()
            .map_or_else(crate::ir::Span::synthetic, |l| l.span.clone()),
    });

    chain
}

/// Apply the merge cascade described by a load chain. Returns the final
/// merged [`ParsedTerragrunt`].
fn apply_cascade(chain: &[ChainEntry], _diagnostics: &mut Vec<Diagnostic>) -> ParsedTerragrunt {
    if chain.is_empty() {
        return ParsedTerragrunt::default();
    }
    // Start from the parent-most (chain[0]) and walk down.
    let mut acc = ParsedTerragrunt::default();
    for entry in chain {
        // Merge locals per the entry's strategy.
        let parent_locals = literal_map_for(&acc.locals);
        let child_locals = literal_map_for(&entry.parsed.locals);
        let merged_locals = merge_locals(&parent_locals, &child_locals, entry.merge_strategy);
        // Accumulate non-literal locals across layers — both the parent's
        // and the child's. The child's entries override the parent's by
        // name. Without this, a parent layer's `merged_vars = merge(...)`
        // (non-literal until the evaluator runs) gets dropped the moment
        // the cascade moves on to a child layer (F-023 fix).
        let prior_non_literals: Vec<crate::ir::Local> = acc
            .locals
            .iter()
            .filter(|l| !matches!(l.value, Expression::Literal(_)))
            .cloned()
            .collect();
        acc.locals =
            map_to_locals_with_inherited(&merged_locals, &entry.parsed, &prior_non_literals);

        // Inputs: only inherit the parent's inputs when the child does
        // not declare one. (Terragrunt-canonical behaviour: a child
        // override replaces the inputs block entirely.)
        if entry.parsed.inputs.is_some() {
            acc.inputs.clone_from(&entry.parsed.inputs);
        }

        // `generate` blocks: child entries override by label, additive otherwise.
        for g in &entry.parsed.generates {
            if let Some(slot) = acc.generates.iter_mut().find(|x| x.label == g.label) {
                *slot = g.clone();
            } else {
                acc.generates.push(g.clone());
            }
        }
        // `dependency` blocks: same shape as `generate` — by-name.
        for d in &entry.parsed.dependencies {
            if let Some(slot) = acc.dependencies.iter_mut().find(|x| x.name == d.name) {
                *slot = d.clone();
            } else {
                acc.dependencies.push(d.clone());
            }
        }
        // `terraform { ... }` blocks accumulate; the state-backend
        // extractor picks the first one with `backend "s3" { ... }`.
        acc.terraform.extend(entry.parsed.terraform.iter().cloned());
        if entry.parsed.remote_state.is_some() {
            acc.remote_state.clone_from(&entry.parsed.remote_state);
        }
        acc.diagnostics
            .extend(entry.parsed.diagnostics.iter().cloned());
    }
    acc
}

/// Build a `Map` of `(name → Value)` from the [`Local`](crate::ir::Local) entries
/// whose `value` is already a `Literal`. Used as the input to the
/// `merge_locals` cascade — non-literal locals propagate via `evaluate_locals`
/// below.
fn literal_map_for(locals: &[crate::ir::Local]) -> Map {
    locals
        .iter()
        .filter_map(|l| match &l.value {
            Expression::Literal(v) => Some((Arc::clone(&l.name), v.clone())),
            _ => None,
        })
        .collect()
}

/// Convert a merged `Map` back into the `Local` shape expected by
/// downstream walkers. Non-literal locals from the source `parsed`
/// (typically references to other locals or to `read_terragrunt_config`
/// outputs) are retained verbatim so the subsequent evaluator pass can
/// reduce them.
///
/// `inherited` carries non-literal locals from earlier cascade layers
/// (parent → child); the child layer's non-literals override any
/// inherited entry with the same name. This is the F-023 fix: without
/// the inherited list, a parent's `merged_vars = merge(...)` would be
/// dropped when the cascade moves on to the child.
fn map_to_locals_with_inherited(
    merged_map: &Map,
    parsed: &ParsedTerragrunt,
    inherited: &[crate::ir::Local],
) -> Vec<crate::ir::Local> {
    let mut out: Vec<crate::ir::Local> = Vec::new();
    // 1) Start with inherited non-literals.
    for l in inherited {
        out.push(l.clone());
    }
    // 2) Add this layer's non-literals; later override earlier on name.
    for l in parsed
        .locals
        .iter()
        .filter(|l| !matches!(l.value, Expression::Literal(_)))
    {
        if let Some(slot) = out.iter_mut().find(|x| x.name == l.name) {
            *slot = l.clone();
        } else {
            out.push(l.clone());
        }
    }
    // 3) Append the merged literal map. Literals win over any non-literal sharing the same name (a
    //    downstream layer that knows the value overrides an upstream layer that needed reduction).
    for (k, v) in merged_map {
        let local = crate::ir::Local::builder()
            .name(Arc::clone(k))
            .value(Expression::Literal(v.clone()))
            .span(crate::ir::Span::synthetic())
            .build();
        if let Some(slot) = out.iter_mut().find(|x| x.name == *k) {
            *slot = local;
        } else {
            out.push(local);
        }
    }
    out
}

/// Iteratively reduce locals against themselves until a fixpoint is
/// reached. Each pass reduces every local against the resolved-so-far
/// map; partials stay as-is.
fn evaluate_locals(
    locals: &[crate::ir::Local],
    funcs: &Arc<FuncRegistry>,
    _tg_state: &Arc<TgState>,
    workspace_root: &Path,
    env_var_mode: &crate::eval::EnvVarMode,
    eval_limits: &EvalLimits,
) -> Map {
    use crate::eval::reduce::{Scope, reduce_expression};
    let mut resolved: Map = Map::new();
    let mut pending: Vec<crate::ir::Local> = locals.to_vec();

    // Bound iterations to defend against cyclic locals (the resolver
    // does not run a full Tarjan; cycles surface as Unresolved leaves).
    for _ in 0..16 {
        let mut made_progress = false;
        let mut next_pending: Vec<crate::ir::Local> = Vec::new();
        for local in pending.drain(..) {
            let scope = Scope::new(
                Map::new(),
                resolved.clone(),
                workspace_root,
                env_var_mode,
                eval_limits,
                funcs,
                None,
            );
            let reduced = reduce_expression(&local.value, &scope);
            if let Expression::Literal(v) = &reduced {
                resolved.push((Arc::clone(&local.name), v.clone()));
                made_progress = true;
            } else {
                next_pending.push(
                    crate::ir::Local::builder()
                        .name(Arc::clone(&local.name))
                        .value(reduced)
                        .span(local.span.clone())
                        .build(),
                );
            }
        }
        pending = next_pending;
        if !made_progress || pending.is_empty() {
            break;
        }
    }
    resolved
}

/// Reduce the merged `inputs = { ... }` against the effective locals.
fn evaluate_inputs(
    inputs_attrs: &AttributeMap,
    effective_locals: &Map,
    funcs: &Arc<FuncRegistry>,
    _tg_state: &Arc<TgState>,
    workspace_root: &Path,
    env_var_mode: &crate::eval::EnvVarMode,
    eval_limits: &EvalLimits,
) -> Map {
    use crate::eval::reduce::{Scope, reduce_expression};
    let scope = Scope::new(
        Map::new(),
        effective_locals.clone(),
        workspace_root,
        env_var_mode,
        eval_limits,
        funcs,
        None,
    );
    let mut out: Map = Vec::with_capacity(inputs_attrs.len());
    for (name, expr) in inputs_attrs {
        let reduced = reduce_expression(expr, &scope);
        if let Expression::Literal(v) = reduced {
            out.push((Arc::clone(name), v));
        }
    }
    out
}

/// Reduce every attribute expression in a list of raw `generate` blocks
/// against the effective locals. Used to collapse heredoc `contents` with
/// `${local.X}` interpolations into a plain `Literal(Str)` so
/// `build_generates` can capture them.
fn reduce_generates(
    raw: &[parsed::GenerateBlockRaw],
    effective_locals: &Map,
    funcs: &Arc<FuncRegistry>,
    workspace_root: &Path,
    env_var_mode: &crate::eval::EnvVarMode,
    eval_limits: &EvalLimits,
) -> Vec<parsed::GenerateBlockRaw> {
    use crate::eval::reduce::{Scope, reduce_expression};
    let scope = Scope::new(
        Map::new(),
        effective_locals.clone(),
        workspace_root,
        env_var_mode,
        eval_limits,
        funcs,
        None,
    );
    raw.iter()
        .map(|g| parsed::GenerateBlockRaw {
            label: Arc::clone(&g.label),
            attrs: g
                .attrs
                .iter()
                .map(|(k, v)| (Arc::clone(k), reduce_expression(v, &scope)))
                .collect(),
            span: g.span.clone(),
        })
        .collect()
}

/// Reduce every attribute in a list of raw `dependency` blocks against the
/// effective locals.
fn reduce_dependencies(
    raw: &[parsed::DependencyBlockRaw],
    effective_locals: &Map,
    funcs: &Arc<FuncRegistry>,
    workspace_root: &Path,
    env_var_mode: &crate::eval::EnvVarMode,
    eval_limits: &EvalLimits,
) -> Vec<parsed::DependencyBlockRaw> {
    use crate::eval::reduce::{Scope, reduce_expression};
    let scope = Scope::new(
        Map::new(),
        effective_locals.clone(),
        workspace_root,
        env_var_mode,
        eval_limits,
        funcs,
        None,
    );
    raw.iter()
        .map(|d| parsed::DependencyBlockRaw {
            name: Arc::clone(&d.name),
            attrs: d
                .attrs
                .iter()
                .map(|(k, v)| (Arc::clone(k), reduce_expression(v, &scope)))
                .collect(),
            span: d.span.clone(),
        })
        .collect()
}

/// Build the `Vec<GenerateBlock>` to surface in [`TerragruntConfig`].
///
/// Best-effort: an attribute that did not collapse to a `Literal(Str)`
/// (e.g. `contents` is a `TemplateConcat` carrying an unresolved
/// `local.merged_vars.aws_account_id` interpolation) is rendered into a
/// placeholder string so downstream consumers still get a row. Sentinel
/// fragments in the rendered output (`${...}`) mark the residue.
fn build_generates(raw: &[parsed::GenerateBlockRaw]) -> Vec<GenerateBlock> {
    raw.iter()
        .filter_map(|r| {
            let mut path: Option<Arc<Path>> = None;
            let mut if_exists: Option<Arc<str>> = None;
            let mut contents: Option<Arc<str>> = None;
            for (k, v) in &r.attrs {
                match k.as_ref() {
                    "path" => {
                        path = render_expr_to_string(v).map(|s| Arc::from(Path::new(s.as_ref())));
                    }
                    "if_exists" => {
                        if_exists = render_expr_to_string(v);
                    }
                    "contents" => {
                        contents = render_expr_to_string(v);
                    }
                    _ => {}
                }
            }
            match (path, if_exists, contents) {
                (Some(p), Some(ie), Some(c)) => Some(
                    GenerateBlock::builder()
                        .label(Arc::clone(&r.label))
                        .path(p)
                        .if_exists(ie)
                        .contents(c)
                        .span(r.span.clone())
                        .build(),
                ),
                _ => None,
            }
        })
        .collect()
}

/// Render a `generate "backend"` heredoc body for re-parsing as HCL.
///
/// Unlike [`render_expr_to_string`] (which keeps `${unresolved}` markers so
/// the rendered string is faithful for storage), this drops unresolved
/// segments entirely. The reason is positional: a placeholder like
/// `${local.backend_profile_block}` sitting at HCL body level — between
/// attributes, not inside a string — re-parses as a syntax error and pulls
/// down the surrounding `terraform { backend "s3" { ... } }` block with it.
/// Dropping the unresolved parts leaves static fields (kind, bucket,
/// region, etc.) recoverable; the few attributes that lived entirely
/// inside the interpolation are lost, which is the best we can do without
/// running a real Terragrunt evaluation.
fn render_contents_for_reparse(expr: &Expression) -> Option<Arc<str>> {
    match expr {
        Expression::Literal(Value::Str(s)) => Some(Arc::clone(s)),
        Expression::TemplateConcat(parts) => {
            let mut out = String::new();
            for part in parts {
                match part {
                    Expression::Literal(Value::Str(s)) => out.push_str(s),
                    Expression::Literal(Value::Int(n)) => out.push_str(&n.to_string()),
                    Expression::Literal(Value::Bool(b)) => out.push_str(&b.to_string()),
                    // Unresolved or other non-literal parts: dropped on
                    // purpose; see fn-level docs.
                    _ => {}
                }
            }
            Some(Arc::from(out))
        }
        _ => None,
    }
}

/// Render an expression as best-effort string for `generate` attribute
/// capture. Literals collapse exactly; `TemplateConcat` parts render with
/// `${unresolved-source}` markers for any non-literal subexpression.
fn render_expr_to_string(expr: &Expression) -> Option<Arc<str>> {
    match expr {
        Expression::Literal(Value::Str(s)) => Some(Arc::clone(s)),
        Expression::Literal(Value::Int(n)) => Some(Arc::from(n.to_string())),
        Expression::Literal(Value::Number(n)) => Some(Arc::from(n.to_string())),
        Expression::Literal(Value::Bool(b)) => Some(Arc::from(b.to_string())),
        Expression::TemplateConcat(parts) => {
            let mut out = String::new();
            for part in parts {
                match part {
                    Expression::Literal(Value::Str(s)) => out.push_str(s),
                    Expression::Literal(Value::Int(n)) => out.push_str(&n.to_string()),
                    Expression::Literal(Value::Bool(b)) => out.push_str(&b.to_string()),
                    Expression::Unresolved(sym) => {
                        out.push_str("${");
                        out.push_str(&sym.source);
                        out.push('}');
                    }
                    _ => return None,
                }
            }
            Some(Arc::from(out))
        }
        _ => None,
    }
}

/// Build the `Vec<DependencyBlock>` to surface in [`TerragruntConfig`].
fn build_dependencies(
    raw: &[parsed::DependencyBlockRaw],
    component_dir: &Arc<Path>,
) -> Vec<DependencyBlock> {
    raw.iter()
        .filter_map(|r| {
            let mut config_path: Option<Arc<Path>> = None;
            let mut mock_outputs: AttributeMap = Vec::new();
            for (k, v) in &r.attrs {
                match (k.as_ref(), v) {
                    ("config_path", Expression::Literal(Value::Str(s))) => {
                        let joined = component_dir.join(s.as_ref());
                        config_path = Some(Arc::from(joined.as_path()));
                    }
                    ("mock_outputs", Expression::Object(entries)) => {
                        mock_outputs = entries
                            .iter()
                            .filter_map(|(k, v)| match k {
                                Expression::Literal(Value::Str(s)) => {
                                    Some((Arc::clone(s), v.clone()))
                                }
                                _ => None,
                            })
                            .collect();
                    }
                    _ => {}
                }
            }
            config_path.map(|cp| {
                DependencyBlock::builder()
                    .name(Arc::clone(&r.name))
                    .config_path(cp)
                    .mock_outputs(mock_outputs)
                    .span(r.span.clone())
                    .build()
            })
        })
        .collect()
}

/// Extract the state backend either from a `terraform { backend "s3" { ... } }`
/// block within a Terragrunt body, or from sub-parsing a
/// `generate "backend"` block's `contents` string.
fn extract_state_backend(
    merged: &ParsedTerragrunt,
    loader: HclEditLoader,
    loader_limits: &LoaderLimits,
) -> Option<StateBackend> {
    // First try the `terraform { backend ... }` shape.
    for tf_body in &merged.terraform {
        if let Some(backend) = backend_from_terraform_body(tf_body) {
            return Some(backend);
        }
    }
    // Then sub-parse each `generate "backend"` block's contents.
    for g in &merged.generates {
        if g.label.as_ref() != "backend" {
            continue;
        }
        let contents_str = g.attrs.iter().find_map(|(k, v)| {
            if k.as_ref() == "contents" {
                return render_contents_for_reparse(v);
            }
            None
        });
        let Some(contents) = contents_str else {
            continue;
        };
        let synthetic_path: Arc<Path> = Arc::from(Path::new("generated_backend.tf"));
        let parsed = loader.parse_bytes(contents.as_bytes(), &synthetic_path, loader_limits);
        for block in &parsed.blocks {
            if !matches!(block.kind, crate::ir::BlockKind::Terraform) {
                continue;
            }
            if let Some(backend) = backend_from_terraform_body(&block.body) {
                return Some(backend);
            }
        }
    }
    None
}

fn backend_from_terraform_body(body: &AttributeMap) -> Option<StateBackend> {
    // `terraform { backend "s3" { ... } }` lowers under our loader as a
    // single nested-block attribute keyed `"backend"`. The block labels
    // live inside the resulting `Expression::Object` under the synthetic
    // `__labels__` key (per spec defect S-006 / S-019). Pulling the first
    // element of `__labels__` gives the backend kind; previously we
    // hardcoded `"s3"` (F-022).
    for (k, v) in body {
        if k.as_ref() == "backend"
            && let Expression::Object(entries) = v
        {
            let mut attrs: AttributeMap = Vec::new();
            let mut kind: Arc<str> = Arc::from("s3");
            for (kk, vv) in entries {
                let Expression::Literal(Value::Str(name)) = kk else {
                    continue;
                };
                if name.as_ref() == "__labels__"
                    && let Expression::Literal(Value::List(labels)) = vv
                    && let Some(Value::Str(label)) = labels.first()
                {
                    kind = Arc::clone(label);
                    continue;
                }
                attrs.push((Arc::clone(name), vv.clone()));
            }
            return Some(
                StateBackend::builder()
                    .kind(kind)
                    .attributes(attrs)
                    .span(crate::ir::Span::synthetic())
                    .build(),
            );
        }
    }
    None
}

fn parse_merge_strategy(expr: Option<&Expression>) -> MergeStrategy {
    let Some(Expression::Literal(Value::Str(s))) = expr else {
        return MergeStrategy::default();
    };
    MergeStrategy::parse(s.as_ref()).unwrap_or_default()
}

/// Read a Terragrunt file from `path` and project it. Returns `None`
/// when the file does not exist or the parse failed (the diagnostics
/// already captured the failure).
fn read_and_project(
    path: &Path,
    workspace_root: &Path,
    loader: HclEditLoader,
    loader_limits: &LoaderLimits,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ParsedTerragrunt> {
    let canonical = paths::canonicalize_inside(path, workspace_root, SymlinkPolicy::Follow).ok()?;
    let Ok(bytes) = std::fs::read(&canonical) else {
        return None;
    };
    let canonical_arc: Arc<Path> = Arc::from(canonical.as_path());
    let parsed_bytes = loader.parse_bytes(&bytes, &canonical_arc, loader_limits);
    let parsed = parsed::project(&parsed_bytes, &canonical_arc);
    diagnostics.extend(parsed.diagnostics.iter().cloned());
    Some(parsed)
}

/// Evaluate an [`Expression`] to a `string` Value via the reducer. Used
/// for include `path` expressions, generate path attributes, etc.
#[allow(clippy::too_many_arguments)]
fn reduce_to_string(
    expr: &Expression,
    extra_locals: &Map,
    funcs: &Arc<FuncRegistry>,
    _tg_state: &Arc<TgState>,
    workspace_root: &Path,
    env_var_mode: &crate::eval::EnvVarMode,
    eval_limits: &EvalLimits,
) -> Option<Arc<str>> {
    use crate::eval::reduce::{Scope, reduce_expression};
    let scope = Scope::new(
        Map::new(),
        extra_locals.clone(),
        workspace_root,
        env_var_mode,
        eval_limits,
        funcs,
        None,
    );
    match reduce_expression(expr, &scope) {
        Expression::Literal(Value::Str(s)) => Some(s),
        _ => None,
    }
}

/// Build a function registry pre-loaded with the HCL/Terraform stdlib +
/// Terragrunt-specific functions. The TG funcs share an `Arc<TgState>`
/// via closure.
#[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
fn build_func_registry(
    tg_state: Arc<TgState>,
    memo: Arc<DashMap<Arc<Path>, Arc<ResolvedTerragrunt>>>,
    stack: Arc<std::sync::Mutex<Vec<Arc<Path>>>>,
    inflight: Arc<DashMap<Arc<Path>, ()>>,
    registry_slot: Arc<OnceLock<Arc<FuncRegistry>>>,
    ctx: &TgContext,
    loader: HclEditLoader,
    loader_limits: LoaderLimits,
    eval_limits: EvalLimits,
) -> FuncRegistry {
    let mut b: FuncRegistryBuilder = FuncRegistry::default_with_stdlib().to_builder();
    b.register(
        "get_terragrunt_dir",
        Arc::new(GetTerragruntDirFn {
            state: Arc::clone(&tg_state),
        }) as Arc<dyn HclFunc>,
    );
    b.register(
        "get_repo_root",
        Arc::new(GetRepoRootFn {
            state: Arc::clone(&tg_state),
        }) as Arc<dyn HclFunc>,
    );
    b.register(
        "get_parent_terragrunt_dir",
        Arc::new(GetParentTerragruntDirFn {
            state: Arc::clone(&tg_state),
        }) as Arc<dyn HclFunc>,
    );
    b.register(
        "find_in_parent_folders",
        Arc::new(FindInParentFoldersFn {
            state: Arc::clone(&tg_state),
        }) as Arc<dyn HclFunc>,
    );
    b.register(
        "find_in_parent_folders_from",
        Arc::new(FindInParentFoldersFromFn {
            state: Arc::clone(&tg_state),
        }) as Arc<dyn HclFunc>,
    );
    b.register(
        "path_relative_to_include",
        Arc::new(PathRelativeToIncludeFn {
            state: Arc::clone(&tg_state),
        }) as Arc<dyn HclFunc>,
    );
    b.register(
        "path_relative_from_include",
        Arc::new(PathRelativeFromIncludeFn {
            state: Arc::clone(&tg_state),
        }) as Arc<dyn HclFunc>,
    );
    b.register("try", Arc::new(TryFn) as Arc<dyn HclFunc>);
    b.register(
        "read_terragrunt_config",
        Arc::new(ReadTerragruntConfigFn {
            state: Arc::clone(&tg_state),
            workspace_root: Arc::clone(&ctx.workspace_root),
            memo,
            stack,
            inflight,
            registry_slot,
            loader,
            loader_limits,
            eval_limits,
            env_var_mode: ctx.env_var_mode.clone(),
        }) as Arc<dyn HclFunc>,
    );
    b.build()
}

/// `read_terragrunt_config(path, fallback?)` — parses the target
/// Terragrunt file, evaluates its locals/inputs, memoises the result by
/// canonical path, and returns a `Value::Map` shaped as
/// `{ locals = { ... }, inputs = { ... } }` so callers can do
/// `local.env_vars.locals.aws_region` style descents.
#[derive(Debug)]
struct ReadTerragruntConfigFn {
    state: Arc<TgState>,
    workspace_root: Arc<Path>,
    memo: Arc<DashMap<Arc<Path>, Arc<ResolvedTerragrunt>>>,
    stack: Arc<std::sync::Mutex<Vec<Arc<Path>>>>,
    inflight: Arc<DashMap<Arc<Path>, ()>>,
    /// Late-bound holder of the registry this function lives in. Populated
    /// right after the registry is constructed (F-021). When we recurse to
    /// reduce a parent's locals, we use the full TG registry so functions
    /// like `find_in_parent_folders` and `get_repo_root` remain dispatchable.
    registry_slot: Arc<OnceLock<Arc<FuncRegistry>>>,
    loader: HclEditLoader,
    loader_limits: LoaderLimits,
    eval_limits: EvalLimits,
    env_var_mode: crate::eval::EnvVarMode,
}

impl HclFunc for ReadTerragruntConfigFn {
    fn call(
        &self,
        args: &[Value],
        _cx: &CallCx<'_>,
    ) -> std::result::Result<Value, crate::eval::FuncError> {
        let target_path: &str = match args.first() {
            Some(Value::Str(s)) => s.as_ref(),
            None => {
                return Err(crate::eval::FuncError::Arity {
                    name: Arc::from("read_terragrunt_config"),
                    expected: 1,
                    got: 0,
                });
            }
            Some(_) => {
                return Err(crate::eval::FuncError::Type {
                    name: Arc::from("read_terragrunt_config"),
                    index: 0,
                    expected: "string",
                    got: "non-string",
                });
            }
        };

        let candidate = Path::new(target_path).to_path_buf();
        let canonical = match paths::canonicalize_inside(
            &candidate,
            &self.workspace_root,
            SymlinkPolicy::Follow,
        ) {
            Ok(p) => Arc::<Path>::from(p),
            Err(_) => {
                // Path escape or missing file → fallback.
                return Ok(args.get(1).cloned().unwrap_or(Value::Map(Vec::new())));
            }
        };

        if let Some(entry) = self.memo.get(&canonical) {
            return Ok(read_result_to_value(&entry));
        }

        // Cycle check via the per-resolution include stack.
        let cycle = match self.stack.lock() {
            Ok(g) => g.iter().any(|p| Arc::ptr_eq(p, &canonical)),
            Err(p) => p.into_inner().iter().any(|p| Arc::ptr_eq(p, &canonical)),
        };
        if cycle {
            return Err(crate::eval::FuncError::Other {
                name: Arc::from("read_terragrunt_config"),
                message: Arc::from(format!(
                    "terragrunt read cycle at `{}`",
                    canonical.display()
                )),
            });
        }
        // Single-flight: refuse to recurse if another in-flight read is on the same path.
        if self.inflight.insert(Arc::clone(&canonical), ()).is_some() {
            return Ok(args.get(1).cloned().unwrap_or(Value::Map(Vec::new())));
        }

        if let Ok(mut g) = self.stack.lock() {
            g.push(Arc::clone(&canonical));
        }

        let Ok(bytes) = std::fs::read(canonical.as_ref()) else {
            self.inflight.remove(&canonical);
            if let Ok(mut g) = self.stack.lock() {
                g.pop();
            }
            return Ok(args.get(1).cloned().unwrap_or(Value::Map(Vec::new())));
        };
        let parsed_bytes = self
            .loader
            .parse_bytes(&bytes, &canonical, &self.loader_limits);
        let parsed = parsed::project(&parsed_bytes, &canonical);

        // Use the same TG registry this function lives in so transitive
        // `read_terragrunt_config` / `find_in_parent_folders` / `get_env`
        // calls inside the parent's locals dispatch correctly (F-021).
        // The OnceLock is populated right after the resolver builds the
        // registry; if it's somehow empty (shouldn't happen), fall back
        // to the stdlib-only registry to keep the call best-effort.
        let nested_funcs: Arc<FuncRegistry> = self
            .registry_slot
            .get()
            .map_or_else(|| Arc::new(FuncRegistry::default_with_stdlib()), Arc::clone);

        let literal_map: Map = literal_map_for(&parsed.locals);
        let resolved = evaluate_locals(
            &parsed.locals,
            &nested_funcs,
            &self.state,
            &self.workspace_root,
            &self.env_var_mode,
            &self.eval_limits,
        );
        let merged_locals = merge_locals(&literal_map, &resolved, MergeStrategy::DeepMapOnly);
        let inputs = if let Some(input_attrs) = &parsed.inputs {
            evaluate_inputs(
                input_attrs,
                &merged_locals,
                &nested_funcs,
                &self.state,
                &self.workspace_root,
                &self.env_var_mode,
                &self.eval_limits,
            )
        } else {
            Map::new()
        };

        let resolved_entry = Arc::new(ResolvedTerragrunt {
            locals: merged_locals,
            inputs,
        });
        self.memo
            .insert(Arc::clone(&canonical), Arc::clone(&resolved_entry));

        if let Ok(mut g) = self.stack.lock() {
            g.pop();
        }
        self.inflight.remove(&canonical);

        Ok(read_result_to_value(&resolved_entry))
    }
}

/// Render a [`ResolvedTerragrunt`] as the `Value::Map` shape callers
/// expect:
/// `{ locals = { ... }, inputs = { ... } }`.
fn read_result_to_value(entry: &ResolvedTerragrunt) -> Value {
    let out: Map = vec![
        (Arc::from("locals"), Value::Map(entry.locals.clone())),
        (Arc::from("inputs"), Value::Map(entry.inputs.clone())),
    ];
    Value::Map(out)
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

    fn write_file(root: &Path, rel: &str, body: &str) -> std::path::PathBuf {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn test_resolves_component_with_root_include() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        write_file(&root, "root.hcl", "locals { aws_region = \"us-east-2\" }\n");
        write_file(
            &root,
            "services/api/terragrunt.hcl",
            "include \"root\" {\n  path = find_in_parent_folders(\"root.hcl\")\n}\n",
        );

        let ctx = TgContext::new(Arc::from(root.as_path()));
        let cfg = FsTerragruntResolver::new()
            .resolve(&root.join("services/api"), &ctx)
            .unwrap();

        // The parent's `aws_region = "us-east-2"` should be visible.
        assert!(
            cfg.effective_locals
                .iter()
                .any(|(k, v)| &**k == "aws_region"
                    && matches!(v, Value::Str(s) if s.as_ref() == "us-east-2")),
            "diags={:?} locals={:?}",
            cfg.diagnostics,
            cfg.effective_locals
        );
        assert_eq!(cfg.includes.len(), 1);
    }

    #[test]
    fn test_detects_include_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        // a.hcl includes b.hcl; b.hcl includes a.hcl.
        write_file(
            &root,
            "a.hcl",
            "include \"b\" { path = find_in_parent_folders(\"b.hcl\") }\n",
        );
        write_file(
            &root,
            "b.hcl",
            "include \"a\" { path = find_in_parent_folders(\"a.hcl\") }\n",
        );
        write_file(
            &root,
            "x/terragrunt.hcl",
            "include \"a\" { path = find_in_parent_folders(\"a.hcl\") }\n",
        );

        let ctx = TgContext::new(Arc::from(root.as_path()));
        let cfg = FsTerragruntResolver::new()
            .resolve(&root.join("x"), &ctx)
            .unwrap();
        assert!(
            cfg.diagnostics.iter().any(|d| &*d.code == "TG2006"),
            "expected TG2006 cycle diag; got {:?}",
            cfg.diagnostics
        );
    }

    #[test]
    fn test_path_escape_in_find_in_parent_folders_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        // No file to find; fallback returns the literal string.
        write_file(
            &root,
            "x/terragrunt.hcl",
            "locals { x = find_in_parent_folders(\"missing.hcl\", \"fallback.hcl\") }\n",
        );
        let ctx = TgContext::new(Arc::from(root.as_path()));
        let cfg = FsTerragruntResolver::new()
            .resolve(&root.join("x"), &ctx)
            .unwrap();
        assert!(
            cfg.effective_locals
                .iter()
                .any(|(k, v)| &**k == "x"
                    && matches!(v, Value::Str(s) if s.as_ref() == "fallback.hcl")),
            "{:?}",
            cfg.effective_locals
        );
    }

    #[test]
    fn test_captures_generate_block() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        write_file(
            &root,
            "x/terragrunt.hcl",
            "generate \"backend\" {\n  path = \"backend.tf\"\n  if_exists = \
             \"overwrite_terragrunt\"\n  contents = \"terraform {}\"\n}\n",
        );
        let ctx = TgContext::new(Arc::from(root.as_path()));
        let cfg = FsTerragruntResolver::new()
            .resolve(&root.join("x"), &ctx)
            .unwrap();
        assert_eq!(cfg.generates.len(), 1);
        assert_eq!(&*cfg.generates[0].label, "backend");
    }

    /// A `generate "backend"` heredoc whose `contents` body mixes static
    /// HCL with `${...}` interpolations (e.g. `${path_relative_to_include()}`
    /// inside the `key` attribute, plus a body-level `${local.x}` injection
    /// line) must still surface a usable [`StateBackend`]. Before the fix,
    /// `extract_state_backend` only accepted `Expression::Literal(Value::Str)`
    /// for the heredoc and silently bailed when it lowered to
    /// `TemplateConcat`, so every component that wired its backend through
    /// a templated `generate` block came out empty.
    #[test]
    fn test_should_extract_backend_from_templated_generate_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        write_file(
            &root,
            "root.hcl",
            "locals {\n  profile_block = \"\"\n}\ngenerate \"backend\" {\n  path = \
             \"backend.tf\"\n  if_exists = \"overwrite_terragrunt\"\n  contents = \
             <<EOF\nterraform {\n  backend \"s3\" {\n    bucket = \"my-tfstate\"\n    region = \
             \"us-west-2\"\n    key    = \
             \"acct/${path_relative_to_include()}.tfstate\"\n${local.profile_block}\n  \
             }\n}\nEOF\n}\n",
        );
        write_file(
            &root,
            "svc/terragrunt.hcl",
            "include \"root\" { path = find_in_parent_folders(\"root.hcl\") }\n",
        );

        let ctx = TgContext::new(Arc::from(root.as_path()));
        let cfg = FsTerragruntResolver::new()
            .resolve(&root.join("svc"), &ctx)
            .unwrap();
        let backend = cfg
            .state_backend
            .as_ref()
            .expect("backend should be extracted from templated generate contents");
        assert_eq!(&*backend.kind, "s3");
        // The `region` is captured into the verbatim attribute map at
        // this stage; the provider-resolver pass later promotes it onto
        // `state_region` (see `provider::resolver`), so we assert at the
        // attribute-map level here.
        let attr_str = |name: &str| -> Option<Arc<str>> {
            backend
                .attributes
                .iter()
                .find(|(k, _)| &**k == name)
                .and_then(|(_, v)| match v {
                    Expression::Literal(Value::Str(s)) => Some(Arc::clone(s)),
                    _ => None,
                })
        };
        assert_eq!(attr_str("bucket").as_deref(), Some("my-tfstate"));
        assert_eq!(attr_str("region").as_deref(), Some("us-west-2"));
    }

    #[test]
    fn test_captures_dependency_block() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        write_file(
            &root,
            "x/terragrunt.hcl",
            "dependency \"vpc\" { config_path = \"../net\" }\n",
        );
        let ctx = TgContext::new(Arc::from(root.as_path()));
        let cfg = FsTerragruntResolver::new()
            .resolve(&root.join("x"), &ctx)
            .unwrap();
        assert_eq!(cfg.dependencies.len(), 1);
        assert_eq!(&*cfg.dependencies[0].name, "vpc");
    }
}
