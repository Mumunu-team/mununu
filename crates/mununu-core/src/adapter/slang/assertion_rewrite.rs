//! Semantics-preserving rewrites of slang's assertion AST, run before translation so that a few
//! upstream SVA spellings land in the fragment the translator already lowers.
//!
//! - **Named sequence / property instances** ([`inline_assertion_instances`]). slang serializes an
//!   instance of a `sequence` / `property` declaration as `AssertionInstance` with its `body`
//!   already bound to the actual arguments. Replacing the instance by its body is the LRM's own
//!   meaning of an instance (IEEE 1800 16.8 / 16.12) when the declaration has no local variables
//!   and does not recurse; those two stay refused, with a reason. A body that is itself a `##`
//!   chain sitting as one element of an outer chain is spliced, its first gap added to the outer
//!   gap (`##a (x ##b y)` ≡ `##a x ##b y`).
//! - **Elements of an unpacked PORT** ([`rewrite_unpacked_port_selects`]). sv2v flattens an
//!   unpacked port `logic [W-1:0] p [N]` into one `N·W`-bit vector with the range's LEFT bound at
//!   the most significant end (measured 2026-10-21: `logic [3:0] data_i [8]` lifts as a 32-bit
//!   input, `data_i[0]` = bits 31:28, `data_i[7]` = bits 3:0). So `p[k]` with a constant `k` is
//!   exactly that slice, and a comparison over `p[i]` with a variable `i` whose every value is a
//!   valid index is the conjunction over `k` of `i != k || cmp[p[i] := p[k]]` (Shannon expansion
//!   on the selector). An unpacked array that is NOT a port is a memory in the lifted model and
//!   stays refused (mununu#514).
//!
//! Both are exact; neither approximates.

use std::collections::HashMap;

use serde_json::{Value, json};

/// The largest number of elements a variable-index case split expands to.
pub const MAX_PORT_CASE_SPLIT: u64 = 64;

/// Replace every `AssertionInstance` by its bound `body`, recursively.
///
/// `Err` (the reason) for an instance with local variables or a recursive property, and for a
/// repetition applied to an instance whose body is not a plain boolean.
pub fn inline_assertion_instances(node: &Value) -> Result<Value, String> {
    let out = inline(node)?;
    Ok(splice_concats(&out))
}

fn instance_body(inst: &Value) -> Result<&Value, String> {
    let name = inst
        .get("symbol")
        .and_then(Value::as_str)
        .and_then(|s| s.split_whitespace().last())
        .unwrap_or("<instance>");
    if inst
        .get("localVars")
        .and_then(Value::as_array)
        .is_some_and(|v| !v.is_empty())
    {
        return Err(format!(
            "`{name}` declares local variables (a match-item assignment) — local-variable \
             sequences are out of fragment"
        ));
    }
    if inst.get("isRecursiveProperty").and_then(Value::as_bool) == Some(true) {
        return Err(format!(
            "`{name}` is a recursive property — out of fragment"
        ));
    }
    inst.get("body")
        .ok_or_else(|| format!("`{name}`: the instance carries no body"))
}

fn inline(node: &Value) -> Result<Value, String> {
    match node {
        Value::Object(map) => {
            // A sequence instance in sequence position: `Simple { expr: AssertionInstance }`.
            if map.get("kind").and_then(Value::as_str) == Some("Simple")
                && let Some(inst) = map.get("expr")
                && inst.get("kind").and_then(Value::as_str) == Some("AssertionInstance")
            {
                let body = inline(instance_body(inst)?)?;
                return match map.get("repetition") {
                    None => Ok(body),
                    Some(rep) => {
                        // `S[*n]` with S a plain boolean is that boolean repeated.
                        let plain = body.get("kind").and_then(Value::as_str) == Some("Simple")
                            && body.get("repetition").is_none();
                        if !plain {
                            return Err("a repetition of a named sequence that is not a plain \
                                        boolean needs length composition — Tier-3b"
                                .into());
                        }
                        let mut b = body;
                        b["repetition"] = rep.clone();
                        Ok(b)
                    }
                };
            }
            // A property instance in property position.
            if map.get("kind").and_then(Value::as_str) == Some("AssertionInstance") {
                return inline(instance_body(node)?);
            }
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), inline(v)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(a) => Ok(Value::Array(
            a.iter().map(inline).collect::<Result<_, _>>()?,
        )),
        other => Ok(other.clone()),
    }
}

/// `##a (x ##b y)` → `##a x ##b y`: an element of a `SequenceConcat` whose sequence is itself a
/// `SequenceConcat` (a spliced instance body) is flattened when every gap involved is numeric.
fn splice_concats(node: &Value) -> Value {
    match node {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), splice_concats(v));
            }
            if out.get("kind").and_then(Value::as_str) == Some("SequenceConcat")
                && let Some(elements) = out.get("elements").and_then(Value::as_array)
            {
                let mut flat = Vec::new();
                for el in elements {
                    let inner = el.get("sequence").filter(|s| {
                        s.get("kind").and_then(Value::as_str) == Some("SequenceConcat")
                    });
                    let outer_gap = (
                        el.get("min").and_then(Value::as_u64),
                        el.get("max").and_then(Value::as_u64),
                    );
                    match (
                        inner
                            .and_then(|s| s.get("elements"))
                            .and_then(Value::as_array),
                        outer_gap,
                    ) {
                        (Some(inner_els), (Some(omin), Some(omax)))
                            if inner_els.iter().all(|e| {
                                e.get("min").and_then(Value::as_u64).is_some()
                                    && e.get("max").and_then(Value::as_u64).is_some()
                            }) && !inner_els.is_empty() =>
                        {
                            for (i, e) in inner_els.iter().enumerate() {
                                let mut e = e.clone();
                                if i == 0 {
                                    let min = e["min"].as_u64().unwrap_or(0) + omin;
                                    let max = e["max"].as_u64().unwrap_or(0) + omax;
                                    e["min"] = json!(min);
                                    e["max"] = json!(max);
                                }
                                flat.push(e);
                            }
                        }
                        _ => flat.push(el.clone()),
                    }
                }
                out.insert("elements".into(), Value::Array(flat));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(splice_concats).collect()),
        other => other.clone(),
    }
}

/// One unpacked port: element width, element count, and the range's LEFT bound (the element at
/// the most significant end of the flattened vector) and its direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnpackedPort {
    pub elem_width: u64,
    pub left: i64,
    pub right: i64,
}

impl UnpackedPort {
    fn count(&self) -> u64 {
        self.left.abs_diff(self.right) + 1
    }
    fn total(&self) -> u64 {
        self.count() * self.elem_width
    }
    /// The flattened `[hi:lo]` of element `k`, `None` when `k` is out of the range.
    fn slice_of(&self, k: i64) -> Option<(u64, u64)> {
        let (lo_idx, hi_idx) = (self.left.min(self.right), self.left.max(self.right));
        if k < lo_idx || k > hi_idx {
            return None;
        }
        let slot = k.abs_diff(self.left);
        let hi = self.total() - 1 - slot * self.elem_width;
        Some((hi, hi + 1 - self.elem_width))
    }
}

/// Parse slang's type string of a one-dimensional unpacked array of a packed vector:
/// `logic[3:0]$[0:7]`, `logic$[7:0]`.
fn parse_unpacked_type(t: &str) -> Option<UnpackedPort> {
    let (packed, unpacked) = t.split_once('$')?;
    let range = |s: &str| -> Option<(i64, i64)> {
        let s = s.strip_prefix('[')?.strip_suffix(']')?;
        let (a, b) = s.split_once(':')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    };
    let (left, right) = range(unpacked)?;
    let base = packed.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    let elem_width = if base.is_empty() {
        1
    } else {
        let (h, l) = range(base)?;
        h.abs_diff(l) + 1
    };
    Some(UnpackedPort {
        elem_width,
        left,
        right,
    })
}

/// Every `Port` of the document whose type is a one-dimensional unpacked array, keyed by its
/// `internalSymbol` (`"<addr> name"`, the string a `NamedValue` referring to it carries).
pub fn collect_unpacked_ports(root: &Value) -> HashMap<String, UnpackedPort> {
    let mut out = HashMap::new();
    fn walk(n: &Value, out: &mut HashMap<String, UnpackedPort>) {
        match n {
            Value::Object(m) => {
                if m.get("kind").and_then(Value::as_str) == Some("Port")
                    && let (Some(sym), Some(t)) = (
                        m.get("internalSymbol").and_then(Value::as_str),
                        m.get("type").and_then(Value::as_str),
                    )
                    && let Some(p) = parse_unpacked_type(t)
                {
                    out.insert(sym.to_string(), p);
                }
                for v in m.values() {
                    walk(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    walk(root, &mut out);
    out
}

fn int_lit(v: i64) -> Value {
    json!({"kind": "IntegerLiteral", "type": "int", "value": v.to_string(), "constant": v.to_string()})
}

fn constant_of(n: &Value) -> Option<i64> {
    n.get("constant")
        .and_then(Value::as_str)
        .and_then(|c| c.rsplit('\'').next())
        .and_then(|c| {
            c.trim_start_matches(|ch: char| ch.is_ascii_alphabetic())
                .parse()
                .ok()
        })
}

/// The port an `ElementSelect` reads, when its base is an unpacked port.
fn port_select<'a>(
    n: &'a Value,
    ports: &HashMap<String, UnpackedPort>,
) -> Option<(UnpackedPort, &'a Value, &'a Value)> {
    if n.get("kind").and_then(Value::as_str) != Some("ElementSelect") {
        return None;
    }
    let base = n.get("value")?;
    if base.get("kind").and_then(Value::as_str) != Some("NamedValue") {
        return None;
    }
    let p = *ports.get(base.get("symbol").and_then(Value::as_str)?)?;
    Some((p, base, n.get("selector")?))
}

fn slice_node(p: UnpackedPort, base: &Value, k: i64) -> Option<Value> {
    let (hi, lo) = p.slice_of(k)?;
    let mut flat = base.clone();
    flat["type"] = json!(format!("logic[{}:0]", p.total() - 1));
    Some(json!({
        "kind": "RangeSelect",
        "type": format!("logic[{hi}:{lo}]"),
        "selectionKind": "Simple",
        "value": flat,
        "left": int_lit(hi as i64),
        "right": int_lit(lo as i64),
    }))
}

/// The bit width of an expression from its slang type (`logic[2:0]` → 3, `logic` → 1).
fn width_of(n: &Value) -> Option<u64> {
    let t = n.get("type").and_then(Value::as_str)?;
    if t.contains('$') {
        return None;
    }
    let base = t.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    if base.is_empty() {
        return Some(1);
    }
    let s = base.strip_prefix('[')?.strip_suffix(']')?;
    let (a, b) = s.split_once(':')?;
    let (a, b): (i64, i64) = (a.parse().ok()?, b.parse().ok()?);
    Some(a.abs_diff(b) + 1)
}

const COMPARISONS: &[&str] = &[
    "Equality",
    "Inequality",
    "CaseEquality",
    "CaseInequality",
    "LessThan",
    "LessThanEqual",
    "GreaterThan",
    "GreaterThanEqual",
];

/// Rewrite every element select of an unpacked port: a constant index becomes the flattened
/// slice; a comparison with a variable-index operand becomes the case split over the selector's
/// values, when every value is a valid index and there are at most [`MAX_PORT_CASE_SPLIT`]. Any
/// other use is left as it was (and the translator refuses it as before).
pub fn rewrite_unpacked_port_selects(node: &Value, ports: &HashMap<String, UnpackedPort>) -> Value {
    if ports.is_empty() {
        return node.clone();
    }
    rewrite(node, ports)
}

fn rewrite(node: &Value, ports: &HashMap<String, UnpackedPort>) -> Value {
    match node {
        Value::Object(map) => {
            // A constant element select: the slice.
            if let Some((p, base, sel)) = port_select(node, ports)
                && let Some(k) = constant_of(sel)
                && let Some(s) = slice_node(p, base, k)
            {
                return s;
            }
            // A comparison with a variable element select as one operand: the case split.
            if map.get("kind").and_then(Value::as_str) == Some("BinaryOp")
                && map
                    .get("op")
                    .and_then(Value::as_str)
                    .is_some_and(|op| COMPARISONS.contains(&op))
            {
                for side in ["left", "right"] {
                    let Some(operand) = map.get(side) else {
                        continue;
                    };
                    let Some((p, base, sel)) = port_select(operand, ports) else {
                        continue;
                    };
                    if constant_of(sel).is_some() {
                        continue;
                    }
                    let Some(iw) = width_of(sel).filter(|w| *w < 63) else {
                        continue;
                    };
                    let values = 1u64 << iw;
                    let lo_idx = p.left.min(p.right);
                    let hi_idx = p.left.max(p.right);
                    // SOUNDNESS: the split is exact only when EVERY value the selector can take
                    // names an element; an out-of-range read is `x` in SV and is not modelled.
                    if lo_idx != 0 || (hi_idx as u64) + 1 < values || values > MAX_PORT_CASE_SPLIT {
                        continue;
                    }
                    let mut conj: Option<Value> = None;
                    for k in 0..values as i64 {
                        let Some(slice) = slice_node(p, base, k) else {
                            continue;
                        };
                        let mut case = node.clone();
                        case[side] = slice;
                        let case = rewrite(&case, ports);
                        let guard = json!({"kind": "BinaryOp", "type": "logic", "op": "Inequality",
                                           "left": rewrite(sel, ports), "right": int_lit(k)});
                        let arm = json!({"kind": "BinaryOp", "type": "logic", "op": "LogicalOr",
                                         "left": guard, "right": case});
                        conj = Some(match conj {
                            None => arm,
                            Some(acc) => json!({"kind": "BinaryOp", "type": "logic",
                                                "op": "LogicalAnd", "left": acc, "right": arm}),
                        });
                    }
                    if let Some(c) = conj {
                        return c;
                    }
                }
            }
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), rewrite(v, ports));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|v| rewrite(v, ports)).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(sym: &str, ty: &str) -> Value {
        json!({"kind": "NamedValue", "type": ty, "symbol": sym})
    }

    /// The shape slang 11 emits for `sequence S; a == b [*2]; endsequence` instanced as
    /// `##1 S` (captured 2026-10-21 in mununu-sva).
    fn instance(sym: &str, local_vars: Value) -> Value {
        json!({"kind": "AssertionInstance", "type": "sequence", "symbol": sym,
               "body": {"kind": "Simple",
                        "expr": {"kind": "BinaryOp", "type": "logic", "op": "Equality",
                                 "left": named("1 ping_p", "logic"),
                                 "right": named("2 ping_n", "logic")},
                        "repetition": {"kind": "Consecutive", "min": 2, "max": 2}},
               "isRecursiveProperty": false, "localVars": local_vars})
    }

    #[test]
    fn an_instance_is_replaced_by_its_bound_body() {
        let concat = json!({"kind": "SequenceConcat", "elements": [
            {"sequence": {"kind": "Simple", "expr": instance("9 PingSigInt_S", json!([]))},
             "min": 1, "max": 1}]});
        let out = inline_assertion_instances(&concat).expect("inlines");
        let el = &out["elements"][0];
        assert_eq!(el["min"], 1);
        assert_eq!(el["sequence"]["kind"], "Simple");
        assert_eq!(el["sequence"]["repetition"]["min"], 2);
        assert_eq!(el["sequence"]["expr"]["op"], "Equality");
        assert!(!out.to_string().contains("AssertionInstance"));
    }

    #[test]
    fn local_variables_and_recursion_stay_refused() {
        let with_local = json!({"kind": "Simple",
                                "expr": instance("9 Linear_P", json!([{"name": "init_s"}]))});
        let e = inline_assertion_instances(&with_local).unwrap_err();
        assert!(e.contains("local variables"), "{e}");
        let mut rec = instance("9 R", json!([]));
        rec["isRecursiveProperty"] = json!(true);
        let e = inline_assertion_instances(&rec).unwrap_err();
        assert!(e.contains("recursive"), "{e}");
    }

    #[test]
    fn a_chain_body_is_spliced_into_the_outer_chain() {
        // `##2 S` with `S = a ##3 b` is `##2 a ##3 b`.
        let body = json!({"kind": "SequenceConcat", "elements": [
            {"sequence": {"kind": "Simple", "expr": named("1 a", "logic")}, "min": 0, "max": 0},
            {"sequence": {"kind": "Simple", "expr": named("2 b", "logic")}, "min": 3, "max": 3}]});
        let inst = json!({"kind": "AssertionInstance", "type": "sequence", "symbol": "9 S",
                          "body": body, "isRecursiveProperty": false, "localVars": []});
        let outer = json!({"kind": "SequenceConcat", "elements": [
            {"sequence": {"kind": "Simple", "expr": inst}, "min": 2, "max": 2}]});
        let out = inline_assertion_instances(&outer).expect("inlines");
        let els = out["elements"].as_array().expect("elements");
        assert_eq!(els.len(), 2);
        assert_eq!(
            (els[0]["min"].as_u64(), els[0]["max"].as_u64()),
            (Some(2), Some(2))
        );
        assert_eq!(
            (els[1]["min"].as_u64(), els[1]["max"].as_u64()),
            (Some(3), Some(3))
        );
    }

    #[test]
    fn the_flattened_slice_puts_the_left_bound_at_the_top() {
        // Measured on the lift: `logic [3:0] data_i [8]` → data_i[0] = 31:28, data_i[7] = 3:0.
        let p = parse_unpacked_type("logic[3:0]$[0:7]").expect("parses");
        assert_eq!((p.elem_width, p.left, p.right), (4, 0, 7));
        assert_eq!(p.slice_of(0), Some((31, 28)));
        assert_eq!(p.slice_of(3), Some((19, 16)));
        assert_eq!(p.slice_of(7), Some((3, 0)));
        assert_eq!(p.slice_of(8), None);
        // a descending range: its left bound (7) is at the top
        let q = parse_unpacked_type("logic$[7:0]").expect("parses");
        assert_eq!(
            (q.elem_width, q.slice_of(7), q.slice_of(0)),
            (1, Some((7, 7)), Some((0, 0)))
        );
        assert!(parse_unpacked_type("logic[3:0]").is_none());
    }

    fn ports() -> HashMap<String, UnpackedPort> {
        let root = json!({"members": [{"kind": "Port", "name": "data_i", "type": "logic[3:0]$[0:7]",
                                       "direction": "In", "internalSymbol": "5 data_i"}]});
        collect_unpacked_ports(&root)
    }

    #[test]
    fn a_constant_port_element_becomes_its_slice() {
        let cmp = json!({"kind": "BinaryOp", "type": "logic", "op": "Equality",
            "left": named("6 data_o", "logic[3:0]"),
            "right": {"kind": "ElementSelect", "type": "logic[3:0]",
                      "value": named("5 data_i", "logic[3:0]$[0:7]"),
                      "selector": {"kind": "IntegerLiteral", "type": "int", "value": "3", "constant": "3"}}});
        let out = rewrite_unpacked_port_selects(&cmp, &ports());
        let r = &out["right"];
        assert_eq!(r["kind"], "RangeSelect");
        assert_eq!(
            (
                r["left"]["constant"].as_str(),
                r["right"]["constant"].as_str()
            ),
            (Some("19"), Some("16"))
        );
        assert_eq!(r["value"]["type"], "logic[31:0]");
    }

    #[test]
    fn a_total_variable_index_is_split_per_element() {
        let cmp = json!({"kind": "BinaryOp", "type": "logic", "op": "Equality",
            "left": named("6 data_o", "logic[3:0]"),
            "right": {"kind": "ElementSelect", "type": "logic[3:0]",
                      "value": named("5 data_i", "logic[3:0]$[0:7]"),
                      "selector": named("7 idx_o", "logic[2:0]")}});
        let out = rewrite_unpacked_port_selects(&cmp, &ports());
        let s = out.to_string();
        assert!(!s.contains("ElementSelect"), "{s}");
        assert_eq!(
            s.matches("\"Inequality\"").count(),
            8,
            "one guard per index value"
        );
        assert!(s.contains("\"constant\":\"31\"") && s.contains("\"constant\":\"3\""));
        // a selector that can name an element past the array is NOT split (SV reads `x` there)
        let wide = json!({"kind": "BinaryOp", "type": "logic", "op": "Equality",
            "left": named("6 data_o", "logic[3:0]"),
            "right": {"kind": "ElementSelect", "type": "logic[3:0]",
                      "value": named("5 data_i", "logic[3:0]$[0:7]"),
                      "selector": named("8 j", "logic[3:0]")}});
        assert!(
            rewrite_unpacked_port_selects(&wide, &ports())
                .to_string()
                .contains("ElementSelect")
        );
        // a memory (not a port) is untouched — mununu#514
        let mem = json!({"kind": "ElementSelect", "type": "logic[3:0]",
                         "value": named("9 mem", "logic[3:0]$[0:7]"),
                         "selector": {"kind": "IntegerLiteral", "type": "int", "value": "1", "constant": "1"}});
        assert_eq!(rewrite_unpacked_port_selects(&mem, &ports()), mem);
    }
}
