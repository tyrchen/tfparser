//! `locals` fixpoint solver and cycle detection.
//!
//! Per [13-evaluator.md § 3] step 2: topologically order the component's
//! `locals` by their inter-references and evaluate them in dependency
//! order. The worklist algorithm reduces one `Local` at a time, taking the
//! reduced expression and binding it into the scope so later locals can
//! reach it.
//!
//! Cycle detection runs as a Tarjan-style SCC pass over the **declared**
//! dependency graph (every `Local` is a node; every `local.X` reference
//! adds a `X → name` edge). Any SCC of size > 1, or a self-edge, surfaces
//! as [`EvalError::Cycle`]. Participants are sorted by address so the
//! diagnostic is deterministic across runs.
//!
//! [13-evaluator.md § 3]: ../../../specs/13-evaluator.md

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use crate::{
    eval::{
        error::EvalError,
        reduce::{Scope, reduce_expression},
    },
    ir::{Address, Expression, Local, SymbolKind},
};

/// A single participant in a cycle diagnostic.
///
/// Wrapped instead of `Address` directly to keep the public API additive
/// — future variants can carry the file / span of the offending
/// `local.X` reference. Phase 4 ships the address only.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct CycleParticipant {
    /// `local.<name>` address.
    pub address: Address,
}

impl CycleParticipant {
    /// Construct a participant from an address.
    #[must_use]
    pub const fn new(address: Address) -> Self {
        Self { address }
    }
}

/// Resolve the component's locals against `scope`, returning the post-
/// reduction `Local`s plus any local-shadowable bindings.
///
/// `scope.locals` is updated as each local resolves so the next iteration
/// can reach it. Locals that cannot resolve in any iteration keep their
/// expressions (possibly partially-reduced); the binding is left absent
/// from `scope.locals` so downstream `local.X` references stay
/// [`Expression::Unresolved`].
///
/// Cycle detection runs **before** any reduction so we never even start
/// evaluating a tree we know will diverge.
///
/// # Errors
///
/// Returns [`EvalError::Cycle`] if the locals' declared reference graph
/// contains a cycle. All other failures are recorded as diagnostics by the
/// caller (the walker treats unresolved subtrees as data, not errors).
pub fn solve_locals(locals: &[Local], scope: &mut Scope) -> Result<Vec<Local>, EvalError> {
    check_cycle(locals)?;

    let mut out: Vec<Local> = locals.to_vec();
    let mut converged: HashSet<Arc<str>> = HashSet::new();
    // Bounded by `locals.len()` because each fully-resolved local kicks
    // off at most one new wave of resolutions. Cap at len()+1 anyway as a
    // belt-and-braces defence against pathological inputs.
    let max_iters = locals.len().saturating_add(1);
    for _ in 0..max_iters {
        let mut made_progress = false;
        for local in &mut out {
            if converged.contains(&local.name) {
                continue;
            }
            let reduced = reduce_expression(&local.value, scope);
            let progressed = reduced != local.value;
            local.value = reduced;
            if let Expression::Literal(v) = &local.value {
                scope.locals.push((Arc::clone(&local.name), v.clone()));
                converged.insert(Arc::clone(&local.name));
                made_progress = true;
            } else if progressed {
                made_progress = true;
            }
        }
        if !made_progress {
            break;
        }
    }
    Ok(out)
}

/// Tarjan-style cycle check over the locals' declared `local.X` references.
///
/// We do not consult `address_hint` on the contained expressions because
/// the walk wants direct child→parent edges; the lowering pass already
/// classified every `local.X` traversal as
/// [`SymbolKind::Local`](crate::ir::SymbolKind::Local), so a simple
/// `collect_local_refs` pass is enough.
pub(super) fn check_cycle(locals: &[Local]) -> Result<(), EvalError> {
    let name_to_idx: HashMap<Arc<str>, usize> = locals
        .iter()
        .enumerate()
        .map(|(i, l)| (Arc::clone(&l.name), i))
        .collect();

    let mut edges: Vec<Vec<usize>> = (0..locals.len()).map(|_| Vec::new()).collect();
    for (i, local) in locals.iter().enumerate() {
        let Some(edge_list) = edges.get_mut(i) else {
            continue;
        };
        let mut deps: HashSet<Arc<str>> = HashSet::new();
        collect_local_refs(&local.value, &mut deps);
        for dep in deps {
            if let Some(j) = name_to_idx.get(&dep) {
                edge_list.push(*j);
            }
        }
    }

    if let Some(scc) = tarjan_first_cycle(&edges) {
        let mut addrs: BTreeMap<String, Address> = BTreeMap::new();
        for idx in scc {
            if let Some(local) = locals.get(idx) {
                let addr_str = format!("local.{}", local.name);
                if let Ok(addr) = Address::new(&addr_str) {
                    addrs.insert(addr_str, addr);
                }
            }
        }
        return Err(EvalError::Cycle {
            participants: addrs.into_values().collect(),
        });
    }
    Ok(())
}

/// Surface the first non-trivial SCC encountered (Tarjan's algorithm).
///
/// Returns `Some(scc)` for any SCC of size > 1 or a single node with a
/// self-edge, else `None`. The visit order is deterministic across runs
/// (node ids 0..n).
fn tarjan_first_cycle(edges: &[Vec<usize>]) -> Option<Vec<usize>> {
    struct State {
        index: u32,
        stack: Vec<usize>,
        on_stack: Vec<bool>,
        indices: Vec<Option<u32>>,
        lowlinks: Vec<u32>,
        found: Option<Vec<usize>>,
    }

    fn strongconnect(v: usize, edges: &[Vec<usize>], st: &mut State) {
        if st.found.is_some() {
            return;
        }
        if let (Some(slot), Some(low)) = (st.indices.get_mut(v), st.lowlinks.get_mut(v)) {
            *slot = Some(st.index);
            *low = st.index;
        }
        st.index += 1;
        st.stack.push(v);
        if let Some(flag) = st.on_stack.get_mut(v) {
            *flag = true;
        }

        let outgoing = edges.get(v).map_or(&[][..], Vec::as_slice);
        for &w in outgoing {
            let visited = st.indices.get(w).copied().flatten();
            if visited.is_none() {
                strongconnect(w, edges, st);
                let merged = st.lowlinks.get(v).copied().unwrap_or(u32::MAX);
                let from_w = st.lowlinks.get(w).copied().unwrap_or(u32::MAX);
                if let Some(slot) = st.lowlinks.get_mut(v) {
                    *slot = merged.min(from_w);
                }
            } else if st.on_stack.get(w).copied().unwrap_or(false) {
                let merged = st.lowlinks.get(v).copied().unwrap_or(u32::MAX);
                let from_w = visited.unwrap_or(u32::MAX);
                if let Some(slot) = st.lowlinks.get_mut(v) {
                    *slot = merged.min(from_w);
                }
            }
        }

        let low_v = st.lowlinks.get(v).copied();
        let idx_v = st.indices.get(v).copied().flatten();
        if low_v == idx_v {
            let mut scc: Vec<usize> = Vec::new();
            while let Some(w) = st.stack.pop() {
                if let Some(flag) = st.on_stack.get_mut(w) {
                    *flag = false;
                }
                scc.push(w);
                if w == v {
                    break;
                }
            }
            let is_self_cycle = scc.len() == 1 && edges.get(v).is_some_and(|out| out.contains(&v));
            if scc.len() > 1 || is_self_cycle {
                st.found = Some(scc);
            }
        }
    }

    let n = edges.len();
    let mut st = State {
        index: 0,
        stack: Vec::new(),
        on_stack: vec![false; n],
        indices: vec![None; n],
        lowlinks: vec![0; n],
        found: None,
    };

    for v in 0..n {
        let already = st.indices.get(v).copied().flatten();
        if already.is_none() {
            strongconnect(v, edges, &mut st);
            if st.found.is_some() {
                break;
            }
        }
    }
    st.found
}

fn collect_local_refs(expr: &Expression, out: &mut HashSet<Arc<str>>) {
    match expr {
        Expression::Literal(_) => {}
        Expression::Unresolved(s) => {
            if matches!(s.kind, SymbolKind::Local) {
                // Source is `local.<name>[.<rest>]`. We only care about
                // the `<name>` part.
                let rest = s.source.strip_prefix("local.").unwrap_or(&s.source);
                let name = rest.split('.').next().unwrap_or(rest);
                if !name.is_empty() {
                    out.insert(Arc::from(name));
                }
            }
        }
        Expression::BinaryOp { lhs, rhs, .. } => {
            collect_local_refs(lhs, out);
            collect_local_refs(rhs, out);
        }
        Expression::UnaryOp { operand, .. } => collect_local_refs(operand, out),
        Expression::TemplateConcat(parts) | Expression::Array(parts) => {
            for p in parts {
                collect_local_refs(p, out);
            }
        }
        Expression::Object(entries) => {
            for (k, v) in entries {
                collect_local_refs(k, out);
                collect_local_refs(v, out);
            }
        }
        Expression::FuncCall(call) => {
            for a in &call.args {
                collect_local_refs(a, out);
            }
        }
        Expression::Conditional(c) => {
            collect_local_refs(&c.cond, out);
            collect_local_refs(&c.then_branch, out);
            collect_local_refs(&c.else_branch, out);
        }
        Expression::For(f) => {
            collect_local_refs(&f.collection, out);
            collect_local_refs(&f.value, out);
            if let Some(k) = &f.key {
                collect_local_refs(k, out);
            }
            if let Some(c) = &f.cond {
                collect_local_refs(c, out);
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ir::{Expression, Local, Span, SymbolKind, Symbolic, Value};

    fn local(name: &str, expr: Expression) -> Local {
        Local::builder()
            .name(Arc::<str>::from(name))
            .value(expr)
            .span(Span::synthetic())
            .build()
    }

    fn local_ref(name: &str) -> Expression {
        Expression::Unresolved(
            Symbolic::builder()
                .kind(SymbolKind::Local)
                .source(Arc::<str>::from(format!("local.{name}")))
                .span(Span::synthetic())
                .build(),
        )
    }

    #[test]
    fn test_cycle_check_accepts_acyclic_locals() {
        let locals = vec![
            local("a", Expression::Literal(Value::Int(1))),
            local("b", local_ref("a")),
        ];
        check_cycle(&locals).expect("acyclic");
    }

    #[test]
    fn test_cycle_check_rejects_self_cycle() {
        let locals = vec![local("a", local_ref("a"))];
        let err = check_cycle(&locals).unwrap_err();
        let EvalError::Cycle { participants } = err else {
            panic!("expected cycle");
        };
        assert_eq!(participants.len(), 1);
        assert_eq!(participants[0].as_str(), "local.a");
    }

    #[test]
    fn test_cycle_check_rejects_two_node_cycle() {
        let locals = vec![local("a", local_ref("b")), local("b", local_ref("a"))];
        let err = check_cycle(&locals).unwrap_err();
        let EvalError::Cycle { participants } = err else {
            panic!("expected cycle");
        };
        // Sorted alphabetically.
        assert_eq!(participants.len(), 2);
        assert_eq!(participants[0].as_str(), "local.a");
        assert_eq!(participants[1].as_str(), "local.b");
    }

    #[test]
    fn test_cycle_check_rejects_three_node_cycle() {
        let locals = vec![
            local("a", local_ref("b")),
            local("b", local_ref("c")),
            local("c", local_ref("a")),
        ];
        let err = check_cycle(&locals).unwrap_err();
        let EvalError::Cycle { participants } = err else {
            panic!("expected cycle");
        };
        let names: Vec<&str> = participants.iter().map(Address::as_str).collect();
        assert_eq!(names, vec!["local.a", "local.b", "local.c"]);
    }

    #[test]
    fn test_cycle_check_finds_cycle_among_acyclic_neighbours() {
        // Two disjoint groups: one acyclic, one cyclic. The cycle still
        // surfaces.
        let locals = vec![
            local("x", Expression::Literal(Value::Int(0))),
            local("a", local_ref("b")),
            local("b", local_ref("a")),
        ];
        let err = check_cycle(&locals).unwrap_err();
        assert!(matches!(err, EvalError::Cycle { .. }));
    }

    #[test]
    fn test_cycle_participants_are_deterministic_order() {
        // Same cycle declared in a different source order: participants
        // sort lexicographically so the diagnostic is stable.
        let locals_a = vec![local("a", local_ref("b")), local("b", local_ref("a"))];
        let locals_b = vec![local("b", local_ref("a")), local("a", local_ref("b"))];
        let EvalError::Cycle { participants: pa } = check_cycle(&locals_a).unwrap_err() else {
            panic!()
        };
        let EvalError::Cycle { participants: pb } = check_cycle(&locals_b).unwrap_err() else {
            panic!()
        };
        assert_eq!(pa, pb);
    }
}
