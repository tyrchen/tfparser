//! Canonical-JSON renderer for the Parquet `attributes_json` column.
//!
//! Per [10-data-model.md § 4] and [20-parquet-exporter.md § 3.3]:
//!
//! - keys sorted alphabetically at every object level;
//! - numbers rendered with `ryu` (shortest exact `f64` representation);
//! - HCL `null`/bool/string/list/map → JSON equivalents;
//! - [`Expression::Unresolved`] → `{"__unresolved__": "<source>", "__kind__": "Var|Local|..."}`;
//! - [`Expression::FuncCall`] → `{"__unresolved_func__": "<name>", "args": [...]}`;
//! - the renderer never panics: any non-finite `f64` (NaN, ±∞) is rendered as JSON `null` to keep
//!   the artefact strict-JSON valid.
//!
//! The renderer is deterministic: same input → byte-identical output.
//!
//! [10-data-model.md § 4]: ../../../specs/10-data-model.md
//! [20-parquet-exporter.md § 3.3]: ../../../specs/20-parquet-exporter.md

use std::{cmp::Ordering, fmt::Write as _};

use crate::ir::{AttributeMap, Expression, SymbolKind, Value};

/// Render an [`AttributeMap`] as canonical JSON object into `out`.
///
/// `out` is appended to (not cleared) so callers can pool the buffer
/// across rows.
pub fn render_attribute_map(map: &AttributeMap, out: &mut String) {
    let mut entries: Vec<(&str, &Expression)> = map.iter().map(|(k, v)| (k.as_ref(), v)).collect();
    entries.sort_by(|(a, _), (b, _)| str::cmp(a, b));
    out.push('{');
    for (idx, (key, expr)) in entries.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        write_json_string(key, out);
        out.push(':');
        write_expression(expr, out);
    }
    out.push('}');
}

/// Convenience: produce the canonical JSON of an [`AttributeMap`] as a
/// freshly-allocated [`String`].
#[must_use]
pub fn attribute_map_to_string(map: &AttributeMap) -> String {
    let mut s = String::with_capacity(64);
    render_attribute_map(map, &mut s);
    s
}

fn write_expression(expr: &Expression, out: &mut String) {
    match expr {
        Expression::Literal(v) => write_value(v, out),
        Expression::Unresolved(s) => write_unresolved(s, out),
        Expression::FuncCall(call) => {
            out.push_str(r#"{"__unresolved_func__":"#);
            write_json_string(call.name.as_ref(), out);
            out.push_str(r#","args":"#);
            write_expression_list(&call.args, out);
            out.push('}');
        }
        Expression::BinaryOp { op, lhs, rhs, .. } => {
            out.push_str(r#"{"__binary_op__":"#);
            write_json_string(&format!("{op:?}"), out);
            out.push_str(r#","lhs":"#);
            write_expression(lhs, out);
            out.push_str(r#","rhs":"#);
            write_expression(rhs, out);
            out.push('}');
        }
        Expression::UnaryOp { op, operand, .. } => {
            out.push_str(r#"{"__unary_op__":"#);
            write_json_string(&format!("{op:?}"), out);
            out.push_str(r#","operand":"#);
            write_expression(operand, out);
            out.push('}');
        }
        Expression::TemplateConcat(parts) => {
            out.push_str(r#"{"__template_concat__":"#);
            write_expression_list(parts, out);
            out.push('}');
        }
        Expression::Array(items) => write_expression_list(items, out),
        Expression::Object(entries) => write_object(entries, out),
        Expression::Conditional(c) => {
            out.push_str(r#"{"__conditional__":{"cond":"#);
            write_expression(&c.cond, out);
            out.push_str(r#","else":"#);
            write_expression(&c.else_branch, out);
            out.push_str(r#","then":"#);
            write_expression(&c.then_branch, out);
            out.push_str("}}");
        }
        Expression::For(f) => write_for(f, out),
    }
}

fn write_unresolved(s: &crate::ir::Symbolic, out: &mut String) {
    out.push_str(r#"{"__kind__":"#);
    write_json_string(symbol_kind_str(s.kind), out);
    out.push_str(r#","__unresolved__":"#);
    write_json_string(s.source.as_ref(), out);
    out.push('}');
}

fn write_expression_list(items: &[Expression], out: &mut String) {
    out.push('[');
    for (idx, item) in items.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        write_expression(item, out);
    }
    out.push(']');
}

#[allow(clippy::indexing_slicing)]
fn write_object(entries: &[(Expression, Expression)], out: &mut String) {
    // Keys may themselves be expressions; render the alpha-sorted
    // string projection of each key for stability.
    let mut indexed: Vec<(usize, String)> = entries
        .iter()
        .enumerate()
        .map(|(i, (k, _))| (i, expression_to_canonical(k)))
        .collect();
    indexed.sort_by(|(_, a), (_, b)| Ord::cmp(a, b));
    out.push('{');
    for (n, (orig_idx, key_str)) in indexed.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        write_json_string(key_str, out);
        out.push(':');
        write_expression(&entries[*orig_idx].1, out);
    }
    out.push('}');
}

fn write_for(f: &crate::ir::ForExpr, out: &mut String) {
    out.push_str(r#"{"__for__":{"binders":["#);
    for (idx, b) in f.binders.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        write_json_string(b.as_ref(), out);
    }
    out.push_str(r#"],"collection":"#);
    write_expression(&f.collection, out);
    out.push_str(r#","object_form":"#);
    out.push_str(if f.object_form { "true" } else { "false" });
    out.push_str(r#","value":"#);
    write_expression(&f.value, out);
    out.push_str("}}");
}

fn write_value(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => {
            // i64::MIN..=i64::MAX renders as ASCII digits — never errors.
            let _ = write!(out, "{n}");
        }
        Value::Number(f) => {
            if f.is_finite() {
                let mut buf = ryu::Buffer::new();
                out.push_str(buf.format(*f));
            } else {
                // Strict JSON has no NaN / Inf; render as null.
                out.push_str("null");
            }
        }
        Value::Str(s) => write_json_string(s.as_ref(), out),
        Value::List(items) => {
            out.push('[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Map(entries) => {
            // Stable alpha order over UTF-8 string keys (insertion order is
            // preserved in the IR, but canonical JSON commits to alpha).
            let mut sorted: Vec<(&str, &Value)> =
                entries.iter().map(|(k, v)| (k.as_ref(), v)).collect();
            sorted.sort_by(|(a, _), (b, _)| match (a, b) {
                (a, b) if a == b => Ordering::Equal,
                (a, b) => str::cmp(a, b),
            });
            out.push('{');
            for (n, (k, val)) in sorted.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_value(val, out);
            }
            out.push('}');
        }
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

const fn symbol_kind_str(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Var => "Var",
        SymbolKind::Local => "Local",
        SymbolKind::Resource => "Resource",
        SymbolKind::Data => "Data",
        SymbolKind::Module => "Module",
        SymbolKind::Path => "Path",
        SymbolKind::Iteration => "Iteration",
        SymbolKind::Terraform => "Terraform",
        SymbolKind::TerragruntDependency => "TerragruntDependency",
        SymbolKind::Other => "Other",
    }
}

/// Compact canonical projection of an arbitrary [`Expression`] used purely
/// for sort-key generation when an [`Expression::Object`] has expression keys.
fn expression_to_canonical(expr: &Expression) -> String {
    let mut s = String::new();
    write_expression(expr, &mut s);
    s
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
    use crate::ir::{Span, Symbolic};

    fn s(src: &str, kind: SymbolKind) -> Expression {
        Expression::Unresolved(
            Symbolic::builder()
                .kind(kind)
                .source(Arc::<str>::from(src))
                .span(Span::synthetic())
                .build(),
        )
    }

    #[test]
    fn test_should_render_literal_attribute_map() {
        let map: AttributeMap = vec![
            (
                Arc::<str>::from("name"),
                Expression::Literal(Value::Str(Arc::from("svc"))),
            ),
            (
                Arc::<str>::from("count"),
                Expression::Literal(Value::Int(3)),
            ),
            (
                Arc::<str>::from("enabled"),
                Expression::Literal(Value::Bool(true)),
            ),
        ];
        let out = attribute_map_to_string(&map);
        // Keys are alpha-sorted: count, enabled, name.
        assert_eq!(out, r#"{"count":3,"enabled":true,"name":"svc"}"#);
    }

    #[test]
    fn test_should_render_unresolved_with_sentinel() {
        let map: AttributeMap =
            vec![(Arc::<str>::from("region"), s("var.region", SymbolKind::Var))];
        let out = attribute_map_to_string(&map);
        assert_eq!(
            out,
            r#"{"region":{"__kind__":"Var","__unresolved__":"var.region"}}"#
        );
    }

    #[test]
    fn test_should_render_func_call_with_unresolved_func_sentinel() {
        let map: AttributeMap = vec![(
            Arc::<str>::from("payload"),
            Expression::FuncCall(Box::new(crate::ir::FuncCall {
                name: Arc::from("jsonencode"),
                args: vec![Expression::Literal(Value::Str(Arc::from("hi")))],
                span: Span::synthetic(),
            })),
        )];
        let out = attribute_map_to_string(&map);
        assert_eq!(
            out,
            r#"{"payload":{"__unresolved_func__":"jsonencode","args":["hi"]}}"#
        );
    }

    #[test]
    fn test_should_be_deterministic_for_same_input() {
        let map: AttributeMap = vec![
            (Arc::<str>::from("z"), Expression::Literal(Value::Int(1))),
            (Arc::<str>::from("a"), Expression::Literal(Value::Int(2))),
            (Arc::<str>::from("m"), Expression::Literal(Value::Int(3))),
        ];
        let a = attribute_map_to_string(&map);
        let b = attribute_map_to_string(&map);
        assert_eq!(a, b);
        assert_eq!(a, r#"{"a":2,"m":3,"z":1}"#);
    }

    #[test]
    fn test_should_render_nested_value_map_alpha_sorted() {
        let map: AttributeMap = vec![(
            Arc::<str>::from("tags"),
            Expression::Literal(Value::Map(vec![
                (Arc::from("Owner"), Value::Str(Arc::from("y"))),
                (Arc::from("Service"), Value::Str(Arc::from("x"))),
            ])),
        )];
        let out = attribute_map_to_string(&map);
        assert_eq!(out, r#"{"tags":{"Owner":"y","Service":"x"}}"#);
    }

    #[test]
    fn test_should_escape_control_characters_in_strings() {
        let map: AttributeMap = vec![(
            Arc::<str>::from("msg"),
            Expression::Literal(Value::Str(Arc::from("hi\n\"world"))),
        )];
        let out = attribute_map_to_string(&map);
        assert!(out.contains(r"\n"));
        assert!(out.contains(r#"\""#));
    }

    #[test]
    fn test_should_render_array_of_mixed_resolved_and_unresolved() {
        let map: AttributeMap = vec![(
            Arc::<str>::from("cidrs"),
            Expression::Array(vec![
                Expression::Literal(Value::Str(Arc::from("10.0.0.0/8"))),
                s("var.extra", SymbolKind::Var),
            ]),
        )];
        let out = attribute_map_to_string(&map);
        assert_eq!(
            out,
            r#"{"cidrs":["10.0.0.0/8",{"__kind__":"Var","__unresolved__":"var.extra"}]}"#
        );
    }

    #[test]
    fn test_should_render_finite_floats_via_ryu_and_nan_as_null() {
        let map: AttributeMap = vec![
            (
                Arc::<str>::from("ratio"),
                Expression::Literal(Value::Number(1.5)),
            ),
            (
                Arc::<str>::from("nan"),
                Expression::Literal(Value::Number(f64::NAN)),
            ),
        ];
        let out = attribute_map_to_string(&map);
        assert!(out.contains("1.5"), "{out}");
        assert!(out.contains(r#""nan":null"#), "{out}");
    }

    #[test]
    fn test_should_render_unresolved_keys_in_alpha_byte_order() {
        // `__kind__` < `__unresolved__` under ASCII byte order — both start
        // with `__` so the next char decides. The byte string must be
        // exactly this; future refactors that flip the order would silently
        // break downstream consumers that rely on byte-determinism.
        let map: AttributeMap = vec![(Arc::<str>::from("r"), s("var.x", SymbolKind::Var))];
        let out = attribute_map_to_string(&map);
        assert_eq!(out, r#"{"r":{"__kind__":"Var","__unresolved__":"var.x"}}"#);
    }

    #[test]
    fn test_should_render_func_call_keys_in_alpha_byte_order() {
        // `_` (0x5F) < `a` (0x61), so `__unresolved_func__` < `args`.
        let map: AttributeMap = vec![(
            Arc::<str>::from("p"),
            Expression::FuncCall(Box::new(crate::ir::FuncCall {
                name: Arc::from("base64encode"),
                args: vec![],
                span: Span::synthetic(),
            })),
        )];
        let out = attribute_map_to_string(&map);
        assert_eq!(
            out,
            r#"{"p":{"__unresolved_func__":"base64encode","args":[]}}"#
        );
    }

    #[test]
    fn test_should_render_binary_op_keys_in_alpha_byte_order() {
        let map: AttributeMap = vec![(
            Arc::<str>::from("flag"),
            Expression::BinaryOp {
                op: crate::ir::BinaryOp::Eq,
                lhs: Box::new(Expression::Literal(crate::ir::Value::Int(1))),
                rhs: Box::new(Expression::Literal(crate::ir::Value::Int(2))),
                span: Span::synthetic(),
            },
        )];
        let out = attribute_map_to_string(&map);
        // `__binary_op__` < `lhs` < `rhs` byte-lexically.
        assert_eq!(out, r#"{"flag":{"__binary_op__":"Eq","lhs":1,"rhs":2}}"#);
    }

    #[test]
    fn test_should_parse_back_through_serde_json() {
        let map: AttributeMap = vec![
            (
                Arc::<str>::from("name"),
                Expression::Literal(Value::Str(Arc::from("svc"))),
            ),
            (Arc::<str>::from("region"), s("var.region", SymbolKind::Var)),
        ];
        let out = attribute_map_to_string(&map);
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["name"], "svc");
        assert_eq!(v["region"]["__unresolved__"], "var.region");
        assert_eq!(v["region"]["__kind__"], "Var");
    }
}
