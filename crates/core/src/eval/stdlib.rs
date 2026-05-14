//! HCL stdlib functions registered into [`FuncRegistry`].
//!
//! Per [13-evaluator.md § 5], the spec lists the "already in `hcl-rs::eval`
//! stdlib (we trust)" set. **`hcl-rs::eval` ships no stdlib** — the user is
//! expected to register every function manually (see
//! [93-improvements-review.md] S-010). So this module implements the subset
//! that materially affects M1 exit criteria and that real-world Terraform
//! configs reach for in resource attributes:
//!
//! - String manipulation: `format`, `lower`, `upper`, `trim`, `trimspace`, `replace`.
//! - Collection helpers: `length`, `keys`, `values`, `merge`, `concat`, `compact`, `lookup`,
//!   `contains`, `flatten`.
//! - Type coercion: `tostring`, `tonumber`, `tobool`, `tolist`, `toset`.
//! - Encoding: `jsonencode`, `jsondecode`, `base64encode`, `base64decode`.
//! - Predicates: `can`, `try`.
//!
//! Anything else (rare or apply-time-only) stays as
//! [`crate::ir::Expression::FuncCall`] per spec 13 § 5 closing rule. The
//! Phase 9 hardening pass widens this set if profiling shows real-world
//! configs frequently leaving stdlib calls unresolved.
//!
//! Every function bounds its output against
//! [`crate::eval::EvalLimits`](super::context::EvalLimits): result strings
//! exceeding `max_str_size`, or lists exceeding `max_list_len`, surface as
//! [`FuncError::Limit`] — the call site keeps the unresolved expression.
//!
//! [13-evaluator.md § 5]: ../../../specs/13-evaluator.md
//! [93-improvements-review.md]: ../../../specs/93-improvements-review.md

use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};

use crate::{
    diagnostic::LimitKind,
    eval::registry::{CallCx, FuncError, FuncRegistryBuilder, HclFunc},
    ir::Value,
};

/// Register every stdlib function shipped by Phase 4.
pub fn register(b: &mut FuncRegistryBuilder) {
    b.register("format", Arc::new(FormatFn));
    b.register("lower", Arc::new(LowerFn));
    b.register("upper", Arc::new(UpperFn));
    b.register("trim", Arc::new(TrimFn));
    b.register("trimspace", Arc::new(TrimspaceFn));
    b.register("replace", Arc::new(ReplaceFn));
    b.register("length", Arc::new(LengthFn));
    b.register("keys", Arc::new(KeysFn));
    b.register("values", Arc::new(ValuesFn));
    b.register("merge", Arc::new(MergeFn));
    b.register("concat", Arc::new(ConcatFn));
    b.register("compact", Arc::new(CompactFn));
    b.register("lookup", Arc::new(LookupFn));
    b.register("contains", Arc::new(ContainsFn));
    b.register("flatten", Arc::new(FlattenFn));
    b.register("tostring", Arc::new(TostringFn));
    b.register("tonumber", Arc::new(TonumberFn));
    b.register("tobool", Arc::new(TobooLFn));
    b.register("tolist", Arc::new(TolistFn));
    b.register("toset", Arc::new(TosetFn));
    b.register("jsonencode", Arc::new(JsonencodeFn));
    b.register("jsondecode", Arc::new(JsondecodeFn));
    b.register("base64encode", Arc::new(Base64encodeFn));
    b.register("base64decode", Arc::new(Base64decodeFn));
}

fn arg_str<'a>(name: &'static str, args: &'a [Value], i: usize) -> Result<&'a str, FuncError> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok(s.as_ref()),
        Some(other) => Err(FuncError::Type {
            name: Arc::from(name),
            index: i,
            expected: "string",
            got: type_name(other),
        }),
        None => Err(FuncError::Arity {
            name: Arc::from(name),
            expected: i + 1,
            got: args.len(),
        }),
    }
}

pub(super) fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Int(_) | Value::Number(_) => "number",
        Value::Str(_) => "string",
        Value::List(_) => "list",
        Value::Map(_) => "map",
    }
}

fn check_str_size(name: &'static str, s: &str, cx: &CallCx<'_>) -> Result<(), FuncError> {
    let observed = u64::try_from(s.len()).unwrap_or(u64::MAX);
    let limit = u64::from(cx.limits.max_str_size);
    if observed > limit {
        Err(FuncError::Limit {
            name: Arc::from(name),
            kind: LimitKind::StringSize,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

fn check_list_len(name: &'static str, len: usize, cx: &CallCx<'_>) -> Result<(), FuncError> {
    let observed = u64::try_from(len).unwrap_or(u64::MAX);
    let limit = u64::from(cx.limits.max_list_len);
    if observed > limit {
        Err(FuncError::Limit {
            name: Arc::from(name),
            kind: LimitKind::ListLength,
            observed,
            limit,
        })
    } else {
        Ok(())
    }
}

/* -------------------------------------------------------------------------- */
/* String functions */
/* -------------------------------------------------------------------------- */

#[derive(Debug)]
struct FormatFn;
impl HclFunc for FormatFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        // Subset of Terraform `format()` — supports `%s`, `%d`, `%v` only.
        // Wider format strings stay unresolved at the call site by surfacing
        // FuncError::Other so the caller keeps the FuncCall verbatim.
        let fmt = arg_str("format", args, 0)?;
        let rest = args.get(1..).unwrap_or(&[]);
        let out = render_format(fmt, rest)?;
        check_str_size("format", &out, cx)?;
        Ok(Value::Str(Arc::from(out)))
    }
}

fn render_format(fmt: &str, args: &[Value]) -> Result<String, FuncError> {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(fmt.len());
    let mut iter = fmt.chars();
    let mut arg_idx = 0_usize;
    while let Some(c) = iter.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match iter.next() {
            Some('%') => out.push('%'),
            Some('s' | 'v') => {
                let v = args.get(arg_idx).ok_or_else(|| FuncError::Arity {
                    name: Arc::from("format"),
                    expected: arg_idx + 2,
                    got: args.len() + 1,
                })?;
                out.push_str(&value_to_display(v));
                arg_idx += 1;
            }
            Some('d') => {
                let v = args.get(arg_idx).ok_or_else(|| FuncError::Arity {
                    name: Arc::from("format"),
                    expected: arg_idx + 2,
                    got: args.len() + 1,
                })?;
                let n = match v {
                    Value::Int(n) => *n,
                    Value::Number(f) => float_to_i64_truncated(*f),
                    other => {
                        return Err(FuncError::Type {
                            name: Arc::from("format"),
                            index: arg_idx + 1,
                            expected: "number",
                            got: type_name(other),
                        });
                    }
                };
                let _ = write!(&mut out, "{n}");
                arg_idx += 1;
            }
            Some(other) => {
                return Err(FuncError::Other {
                    name: Arc::from("format"),
                    message: Arc::from(format!("unsupported verb `%{other}`")),
                });
            }
            None => {
                return Err(FuncError::Other {
                    name: Arc::from("format"),
                    message: Arc::from("trailing `%` in format string"),
                });
            }
        }
    }
    Ok(out)
}

/// Truncate an `f64` to `i64`, clamping at the integer boundary. Used for
/// `format("%d", ...)` against `Value::Number`; out-of-range floats clamp
/// to `i64::MIN` / `i64::MAX` rather than panic-on-truncation.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn float_to_i64_truncated(f: f64) -> i64 {
    if !f.is_finite() {
        return 0;
    }
    let trunc = f.trunc();
    // Bounds compare against the integer extremes widened to f64. The
    // precision loss in the widening is intentional here: we want to
    // clamp any float beyond the representable i64 envelope. The
    // truncating cast at the bottom is the load-bearing line; the lint
    // is correctly suppressed once we've verified `trunc` is in range.
    if trunc <= i64::MIN as f64 {
        i64::MIN
    } else if trunc >= i64::MAX as f64 {
        i64::MAX
    } else {
        trunc as i64
    }
}

fn value_to_display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Number(f) => {
            let mut buf = ryu::Buffer::new();
            buf.format(*f).to_string()
        }
        Value::Str(s) => s.to_string(),
        // Composite renderings follow Terraform's pragmatic
        // "interpolation-style" form; not bit-for-bit but enough for the
        // common `format("/%s", value)` patterns.
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(value_to_display).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Map(entries) => {
            let inner: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{k} = {}", value_to_display(v)))
                .collect();
            format!("{{ {} }}", inner.join(", "))
        }
    }
}

#[derive(Debug)]
struct LowerFn;
impl HclFunc for LowerFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = arg_str("lower", args, 0)?;
        let out = s.to_lowercase();
        check_str_size("lower", &out, cx)?;
        Ok(Value::Str(Arc::from(out)))
    }
}

#[derive(Debug)]
struct UpperFn;
impl HclFunc for UpperFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = arg_str("upper", args, 0)?;
        let out = s.to_uppercase();
        check_str_size("upper", &out, cx)?;
        Ok(Value::Str(Arc::from(out)))
    }
}

#[derive(Debug)]
struct TrimFn;
impl HclFunc for TrimFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = arg_str("trim", args, 0)?;
        let cutset = arg_str("trim", args, 1)?;
        let out: String = s.trim_matches(|c| cutset.contains(c)).to_string();
        Ok(Value::Str(Arc::from(out)))
    }
}

#[derive(Debug)]
struct TrimspaceFn;
impl HclFunc for TrimspaceFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = arg_str("trimspace", args, 0)?;
        Ok(Value::Str(Arc::from(s.trim())))
    }
}

#[derive(Debug)]
struct ReplaceFn;
impl HclFunc for ReplaceFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = arg_str("replace", args, 0)?;
        let from = arg_str("replace", args, 1)?;
        let to = arg_str("replace", args, 2)?;
        // Terraform's replace() falls back to a literal substring replace
        // when `from` is not a regex (and surfaces "/foo/" syntax for
        // regex). Phase 4 ships literal-only; the regex variant would need
        // RegexBuilder size caps from 70-security § 3.4 — deferred to 93.
        let out = s.replace(from, to);
        check_str_size("replace", &out, cx)?;
        Ok(Value::Str(Arc::from(out)))
    }
}

/* -------------------------------------------------------------------------- */
/* Collection functions */
/* -------------------------------------------------------------------------- */

#[derive(Debug)]
struct LengthFn;
impl HclFunc for LengthFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("length"),
            expected: 1,
            got: 0,
        })?;
        let n = match v {
            Value::Str(s) => i64::try_from(s.chars().count()).unwrap_or(i64::MAX),
            Value::List(items) => i64::try_from(items.len()).unwrap_or(i64::MAX),
            Value::Map(entries) => i64::try_from(entries.len()).unwrap_or(i64::MAX),
            other => {
                return Err(FuncError::Type {
                    name: Arc::from("length"),
                    index: 0,
                    expected: "string|list|map",
                    got: type_name(other),
                });
            }
        };
        Ok(Value::Int(n))
    }
}

#[derive(Debug)]
struct KeysFn;
impl HclFunc for KeysFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("keys"),
            expected: 1,
            got: 0,
        })?;
        let Value::Map(entries) = v else {
            return Err(FuncError::Type {
                name: Arc::from("keys"),
                index: 0,
                expected: "map",
                got: type_name(v),
            });
        };
        check_list_len("keys", entries.len(), cx)?;
        // Per Terraform docs, keys() returns the map's keys in
        // lexicographic order.
        let mut out: Vec<Value> = entries
            .iter()
            .map(|(k, _)| Value::Str(Arc::clone(k)))
            .collect();
        out.sort_by(|a, b| match (a, b) {
            (Value::Str(x), Value::Str(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        });
        Ok(Value::List(out))
    }
}

#[derive(Debug)]
struct ValuesFn;
impl HclFunc for ValuesFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("values"),
            expected: 1,
            got: 0,
        })?;
        let Value::Map(entries) = v else {
            return Err(FuncError::Type {
                name: Arc::from("values"),
                index: 0,
                expected: "map",
                got: type_name(v),
            });
        };
        check_list_len("values", entries.len(), cx)?;
        let mut sorted = entries.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Value::List(sorted.into_iter().map(|(_, v)| v).collect()))
    }
}

#[derive(Debug)]
struct MergeFn;
impl HclFunc for MergeFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let mut out: crate::ir::Map = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let Value::Map(entries) = a else {
                return Err(FuncError::Type {
                    name: Arc::from("merge"),
                    index: i,
                    expected: "map",
                    got: type_name(a),
                });
            };
            for (k, v) in entries {
                if let Some(slot) = out.iter_mut().find(|(ok, _)| ok == k) {
                    slot.1 = v.clone();
                } else {
                    out.push((Arc::clone(k), v.clone()));
                }
            }
        }
        Ok(Value::Map(out))
    }
}

#[derive(Debug)]
struct ConcatFn;
impl HclFunc for ConcatFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let mut out: Vec<Value> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let Value::List(items) = a else {
                return Err(FuncError::Type {
                    name: Arc::from("concat"),
                    index: i,
                    expected: "list",
                    got: type_name(a),
                });
            };
            out.extend(items.iter().cloned());
            check_list_len("concat", out.len(), cx)?;
        }
        Ok(Value::List(out))
    }
}

#[derive(Debug)]
struct CompactFn;
impl HclFunc for CompactFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("compact"),
            expected: 1,
            got: 0,
        })?;
        let Value::List(items) = v else {
            return Err(FuncError::Type {
                name: Arc::from("compact"),
                index: 0,
                expected: "list",
                got: type_name(v),
            });
        };
        let out: Vec<Value> = items
            .iter()
            .filter(|item| match item {
                Value::Null => false,
                Value::Str(s) => !s.is_empty(),
                _ => true,
            })
            .cloned()
            .collect();
        Ok(Value::List(out))
    }
}

#[derive(Debug)]
struct LookupFn;
impl HclFunc for LookupFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let (Some(target), Some(needle)) = (args.first(), args.get(1)) else {
            return Err(FuncError::Arity {
                name: Arc::from("lookup"),
                expected: 2,
                got: args.len(),
            });
        };
        let Value::Map(map) = target else {
            return Err(FuncError::Type {
                name: Arc::from("lookup"),
                index: 0,
                expected: "map",
                got: type_name(target),
            });
        };
        let Value::Str(key) = needle else {
            return Err(FuncError::Type {
                name: Arc::from("lookup"),
                index: 1,
                expected: "string",
                got: type_name(needle),
            });
        };
        if let Some((_, v)) = map.iter().find(|(k, _)| k.as_ref() == key.as_ref()) {
            Ok(v.clone())
        } else {
            // Default arg or null if no default supplied (Terraform: error).
            // We return Null rather than raise: best-effort, the row stays
            // resolved; the diagnostic is left for the user's TF lint pass.
            Ok(args.get(2).cloned().unwrap_or(Value::Null))
        }
    }
}

#[derive(Debug)]
struct ContainsFn;
impl HclFunc for ContainsFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let (Some(target), Some(needle)) = (args.first(), args.get(1)) else {
            return Err(FuncError::Arity {
                name: Arc::from("contains"),
                expected: 2,
                got: args.len(),
            });
        };
        let Value::List(list) = target else {
            return Err(FuncError::Type {
                name: Arc::from("contains"),
                index: 0,
                expected: "list",
                got: type_name(target),
            });
        };
        Ok(Value::Bool(list.iter().any(|v| v == needle)))
    }
}

#[derive(Debug)]
struct FlattenFn;
impl HclFunc for FlattenFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("flatten"),
            expected: 1,
            got: 0,
        })?;
        let Value::List(items) = v else {
            return Err(FuncError::Type {
                name: Arc::from("flatten"),
                index: 0,
                expected: "list",
                got: type_name(v),
            });
        };
        let mut out: Vec<Value> = Vec::new();
        flatten_into(items, &mut out, cx)?;
        Ok(Value::List(out))
    }
}

fn flatten_into(items: &[Value], out: &mut Vec<Value>, cx: &CallCx<'_>) -> Result<(), FuncError> {
    for v in items {
        match v {
            Value::List(inner) => flatten_into(inner, out, cx)?,
            other => out.push(other.clone()),
        }
        check_list_len("flatten", out.len(), cx)?;
    }
    Ok(())
}

/* -------------------------------------------------------------------------- */
/* Type coercion */
/* -------------------------------------------------------------------------- */

#[derive(Debug)]
struct TostringFn;
impl HclFunc for TostringFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("tostring"),
            expected: 1,
            got: 0,
        })?;
        let s = value_to_display(v);
        check_str_size("tostring", &s, cx)?;
        Ok(Value::Str(Arc::from(s)))
    }
}

#[derive(Debug)]
struct TonumberFn;
impl HclFunc for TonumberFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("tonumber"),
            expected: 1,
            got: 0,
        })?;
        match v {
            Value::Int(_) | Value::Number(_) => Ok(v.clone()),
            Value::Str(s) => {
                if let Ok(n) = s.parse::<i64>() {
                    Ok(Value::Int(n))
                } else if let Ok(f) = s.parse::<f64>() {
                    Ok(Value::Number(f))
                } else {
                    Err(FuncError::Other {
                        name: Arc::from("tonumber"),
                        message: Arc::from(format!("not a number: {s:?}")),
                    })
                }
            }
            other => Err(FuncError::Type {
                name: Arc::from("tonumber"),
                index: 0,
                expected: "number|string",
                got: type_name(other),
            }),
        }
    }
}

#[derive(Debug)]
struct TobooLFn;
impl HclFunc for TobooLFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("tobool"),
            expected: 1,
            got: 0,
        })?;
        match v {
            Value::Bool(_) => Ok(v.clone()),
            Value::Str(s) => match s.as_ref() {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(FuncError::Other {
                    name: Arc::from("tobool"),
                    message: Arc::from(format!("not a bool literal: {s:?}")),
                }),
            },
            other => Err(FuncError::Type {
                name: Arc::from("tobool"),
                index: 0,
                expected: "bool|string",
                got: type_name(other),
            }),
        }
    }
}

#[derive(Debug)]
struct TolistFn;
impl HclFunc for TolistFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("tolist"),
            expected: 1,
            got: 0,
        })?;
        match v {
            Value::List(_) => Ok(v.clone()),
            other => Err(FuncError::Type {
                name: Arc::from("tolist"),
                index: 0,
                expected: "list",
                got: type_name(other),
            }),
        }
    }
}

#[derive(Debug)]
struct TosetFn;
impl HclFunc for TosetFn {
    fn call(&self, args: &[Value], _cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("toset"),
            expected: 1,
            got: 0,
        })?;
        let Value::List(items) = v else {
            return Err(FuncError::Type {
                name: Arc::from("toset"),
                index: 0,
                expected: "list",
                got: type_name(v),
            });
        };
        // Deduplicate preserving first occurrence; HCL sets are
        // structurally lists in our IR.
        let mut out: Vec<Value> = Vec::with_capacity(items.len());
        for item in items {
            if !out.iter().any(|x| x == item) {
                out.push(item.clone());
            }
        }
        Ok(Value::List(out))
    }
}

/* -------------------------------------------------------------------------- */
/* Encoding */
/* -------------------------------------------------------------------------- */

#[derive(Debug)]
struct JsonencodeFn;
impl HclFunc for JsonencodeFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let v = args.first().ok_or_else(|| FuncError::Arity {
            name: Arc::from("jsonencode"),
            expected: 1,
            got: 0,
        })?;
        let s = value_to_json_string(v);
        check_str_size("jsonencode", &s, cx)?;
        Ok(Value::Str(Arc::from(s)))
    }
}

#[derive(Debug)]
struct JsondecodeFn;
impl HclFunc for JsondecodeFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = arg_str("jsondecode", args, 0)?;
        let v: serde_json::Value = serde_json::from_str(s).map_err(|e| FuncError::Other {
            name: Arc::from("jsondecode"),
            message: Arc::from(format!("invalid JSON: {e}")),
        })?;
        json_to_value("jsondecode", &v, cx)
    }
}

fn value_to_json_string(v: &Value) -> String {
    serde_json::to_string(&value_to_json(v)).unwrap_or_else(|_| "null".into())
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(n) => serde_json::Value::from(*n),
        Value::Number(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Str(s) => serde_json::Value::String(s.to_string()),
        Value::List(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Map(entries) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in entries {
                obj.insert(k.to_string(), value_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
    }
}

fn json_to_value(
    name: &'static str,
    v: &serde_json::Value,
    cx: &CallCx<'_>,
) -> Result<Value, FuncError> {
    match v {
        serde_json::Value::Null => Ok(Value::Null),
        serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || {
                Ok(Value::Number(n.as_f64().ok_or_else(|| {
                    FuncError::Other {
                        name: Arc::from(name),
                        message: Arc::from("non-finite JSON number"),
                    }
                })?))
            },
            |i| Ok(Value::Int(i)),
        ),
        serde_json::Value::String(s) => Ok(Value::Str(Arc::from(s.as_str()))),
        serde_json::Value::Array(items) => {
            check_list_len(name, items.len(), cx)?;
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(json_to_value(name, item, cx)?);
            }
            Ok(Value::List(out))
        }
        serde_json::Value::Object(entries) => {
            let mut out: crate::ir::Map = Vec::with_capacity(entries.len());
            for (k, val) in entries {
                out.push((Arc::from(k.as_str()), json_to_value(name, val, cx)?));
            }
            Ok(Value::Map(out))
        }
    }
}

#[derive(Debug)]
struct Base64encodeFn;
impl HclFunc for Base64encodeFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = arg_str("base64encode", args, 0)?;
        let out = B64.encode(s.as_bytes());
        check_str_size("base64encode", &out, cx)?;
        Ok(Value::Str(Arc::from(out)))
    }
}

#[derive(Debug)]
struct Base64decodeFn;
impl HclFunc for Base64decodeFn {
    fn call(&self, args: &[Value], cx: &CallCx<'_>) -> Result<Value, FuncError> {
        let s = arg_str("base64decode", args, 0)?;
        let bytes = B64.decode(s).map_err(|e| FuncError::Other {
            name: Arc::from("base64decode"),
            message: Arc::from(format!("invalid base64: {e}")),
        })?;
        let out = String::from_utf8(bytes).map_err(|e| FuncError::Other {
            name: Arc::from("base64decode"),
            message: Arc::from(format!("base64 decoded to invalid utf-8: {e}")),
        })?;
        check_str_size("base64decode", &out, cx)?;
        Ok(Value::Str(Arc::from(out)))
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
    use std::path::Path;

    use super::*;
    use crate::eval::{
        context::{EnvVarMode, EvalLimits},
        registry::FuncRegistry,
    };

    fn cx_with_limits(limits: &EvalLimits, env: &EnvVarMode) -> CallCx<'static> {
        let _ = limits; // not used directly; placeholder for non-'static cells
        let _ = env;
        unreachable!("not used")
    }

    fn call(name: &str, args: &[Value]) -> Result<Value, FuncError> {
        let limits = EvalLimits::default();
        let env = EnvVarMode::default();
        let cx = CallCx {
            workspace_root: Path::new("/tmp/repo"),
            env_vars: &env,
            limits: &limits,
        };
        let _ = cx_with_limits;
        let mut b = FuncRegistry::builder();
        register(&mut b);
        let r = b.build();
        r.get(name)
            .unwrap_or_else(|| panic!("function `{name}` not registered"))
            .call(args, &cx)
    }

    #[test]
    fn test_format_supports_s_d_percent() {
        let out = call(
            "format",
            &[
                Value::Str(Arc::from("/%s/%d/%%")),
                Value::Str(Arc::from("hello")),
                Value::Int(42),
            ],
        )
        .unwrap();
        assert_eq!(out, Value::Str(Arc::from("/hello/42/%")));
    }

    #[test]
    fn test_format_rejects_unsupported_verb() {
        let err = call(
            "format",
            &[Value::Str(Arc::from("%q")), Value::Str(Arc::from("x"))],
        )
        .unwrap_err();
        assert!(matches!(err, FuncError::Other { .. }));
    }

    #[test]
    fn test_lower_upper_trim() {
        assert_eq!(
            call("lower", &[Value::Str(Arc::from("ABC"))]).unwrap(),
            Value::Str(Arc::from("abc"))
        );
        assert_eq!(
            call("upper", &[Value::Str(Arc::from("abc"))]).unwrap(),
            Value::Str(Arc::from("ABC"))
        );
        assert_eq!(
            call(
                "trim",
                &[Value::Str(Arc::from("/foo/")), Value::Str(Arc::from("/"))]
            )
            .unwrap(),
            Value::Str(Arc::from("foo"))
        );
        assert_eq!(
            call("trimspace", &[Value::Str(Arc::from("  hi  "))]).unwrap(),
            Value::Str(Arc::from("hi"))
        );
    }

    #[test]
    fn test_replace_substring() {
        let out = call(
            "replace",
            &[
                Value::Str(Arc::from("aXa")),
                Value::Str(Arc::from("X")),
                Value::Str(Arc::from("-")),
            ],
        )
        .unwrap();
        assert_eq!(out, Value::Str(Arc::from("a-a")));
    }

    #[test]
    fn test_length_for_each_kind() {
        assert_eq!(
            call("length", &[Value::Str(Arc::from("hello"))]).unwrap(),
            Value::Int(5)
        );
        assert_eq!(
            call("length", &[Value::List(vec![Value::Int(1), Value::Int(2)])]).unwrap(),
            Value::Int(2)
        );
        assert_eq!(
            call(
                "length",
                &[Value::Map(vec![(Arc::from("a"), Value::Int(1))])]
            )
            .unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn test_keys_returns_sorted() {
        let out = call(
            "keys",
            &[Value::Map(vec![
                (Arc::from("z"), Value::Int(1)),
                (Arc::from("a"), Value::Int(2)),
                (Arc::from("m"), Value::Int(3)),
            ])],
        )
        .unwrap();
        assert_eq!(
            out,
            Value::List(vec![
                Value::Str(Arc::from("a")),
                Value::Str(Arc::from("m")),
                Value::Str(Arc::from("z")),
            ])
        );
    }

    #[test]
    fn test_merge_later_wins() {
        let out = call(
            "merge",
            &[
                Value::Map(vec![
                    (Arc::from("a"), Value::Int(1)),
                    (Arc::from("b"), Value::Int(2)),
                ]),
                Value::Map(vec![(Arc::from("b"), Value::Int(20))]),
            ],
        )
        .unwrap();
        let Value::Map(entries) = out else {
            panic!("expected map");
        };
        let look = |k: &str| -> &Value {
            entries
                .iter()
                .find(|(name, _)| name.as_ref() == k)
                .map(|(_, v)| v)
                .expect("key")
        };
        assert_eq!(look("a"), &Value::Int(1));
        assert_eq!(look("b"), &Value::Int(20));
    }

    #[test]
    fn test_concat_lists() {
        let out = call(
            "concat",
            &[
                Value::List(vec![Value::Int(1)]),
                Value::List(vec![Value::Int(2), Value::Int(3)]),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
    }

    #[test]
    fn test_compact_drops_null_and_empty_string() {
        let out = call(
            "compact",
            &[Value::List(vec![
                Value::Str(Arc::from("a")),
                Value::Null,
                Value::Str(Arc::from("")),
                Value::Str(Arc::from("b")),
            ])],
        )
        .unwrap();
        assert_eq!(
            out,
            Value::List(vec![Value::Str(Arc::from("a")), Value::Str(Arc::from("b")),])
        );
    }

    #[test]
    fn test_lookup_falls_back_to_default() {
        let m = Value::Map(vec![(Arc::from("a"), Value::Int(1))]);
        assert_eq!(
            call(
                "lookup",
                &[
                    m.clone(),
                    Value::Str(Arc::from("missing")),
                    Value::Str(Arc::from("default")),
                ],
            )
            .unwrap(),
            Value::Str(Arc::from("default"))
        );
        assert_eq!(
            call("lookup", &[m, Value::Str(Arc::from("a"))]).unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn test_contains_and_flatten() {
        assert_eq!(
            call(
                "contains",
                &[
                    Value::List(vec![Value::Int(1), Value::Int(2)]),
                    Value::Int(2)
                ]
            )
            .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call(
                "flatten",
                &[Value::List(vec![
                    Value::List(vec![Value::Int(1)]),
                    Value::List(vec![Value::Int(2), Value::Int(3)]),
                ])]
            )
            .unwrap(),
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
    }

    #[test]
    fn test_tostring_and_tonumber() {
        assert_eq!(
            call("tostring", &[Value::Int(42)]).unwrap(),
            Value::Str(Arc::from("42"))
        );
        assert_eq!(
            call("tonumber", &[Value::Str(Arc::from("7"))]).unwrap(),
            Value::Int(7)
        );
        assert_eq!(
            call("tonumber", &[Value::Str(Arc::from("1.5"))]).unwrap(),
            Value::Number(1.5)
        );
        assert!(matches!(
            call("tonumber", &[Value::Str(Arc::from("x"))]),
            Err(FuncError::Other { .. })
        ));
    }

    #[test]
    fn test_toset_dedupes_preserving_order() {
        let out = call(
            "toset",
            &[Value::List(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(1),
                Value::Int(3),
            ])],
        )
        .unwrap();
        assert_eq!(
            out,
            Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
    }

    #[test]
    fn test_jsonencode_decode_round_trip() {
        let v = Value::Map(vec![
            (Arc::from("k"), Value::Int(1)),
            (
                Arc::from("nested"),
                Value::List(vec![Value::Bool(true), Value::Null]),
            ),
        ]);
        let s = call("jsonencode", std::slice::from_ref(&v)).unwrap();
        let Value::Str(json) = s else {
            panic!("expected string");
        };
        let back = call("jsondecode", &[Value::Str(json)]).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn test_base64_round_trip() {
        let enc = call("base64encode", &[Value::Str(Arc::from("hello"))]).unwrap();
        let Value::Str(s) = enc else {
            panic!("expected string");
        };
        assert_eq!(s.as_ref(), "aGVsbG8=");
        let dec = call("base64decode", &[Value::Str(s)]).unwrap();
        assert_eq!(dec, Value::Str(Arc::from("hello")));
    }

    #[test]
    fn test_string_size_cap_is_enforced() {
        let limits = EvalLimits {
            max_str_size: 4,
            ..EvalLimits::default()
        };
        let env = EnvVarMode::default();
        let cx = CallCx {
            workspace_root: Path::new("/tmp"),
            env_vars: &env,
            limits: &limits,
        };
        let f = LowerFn;
        let err = f.call(&[Value::Str(Arc::from("HELLO"))], &cx).unwrap_err();
        assert!(matches!(
            err,
            FuncError::Limit {
                kind: LimitKind::StringSize,
                ..
            }
        ));
    }

    #[test]
    fn test_arity_failure_carries_function_name() {
        let err = call("length", &[]).unwrap_err();
        assert!(matches!(err, FuncError::Arity { .. }));
    }
}
