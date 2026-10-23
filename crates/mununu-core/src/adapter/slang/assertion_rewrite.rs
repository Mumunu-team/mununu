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
//! - **Local variables of a property instance** ([`expand_local_vars`], 2026-10-23). The
//!   `ASSERT_FPV_LINEAR_FSM` shape — `(ante, v = E) |-> C(v)` with `v` assigned exactly once, in
//!   the antecedent's one match item, from an expression `E` free of local variables, and read
//!   only in the consequent — is the finite conjunction over the variable's DOMAIN `D`:
//!   `∧_{d ∈ D} [ (ante && E == d) |-> C(d) ]`, plus, for an enum domain, the coverage conjunct
//!   `ante |-> ∨_{d ∈ D} E == d` so a HOLDS never rests on `E` leaving its encoding (IEEE 1800
//!   16.10: the match item samples `E` at the antecedent's match; the conjunction enumerates that
//!   sample). `D` is the members of the variable's enum type (through any `typedef` chain), or
//!   `0..2^W` for a packed `logic [W-1:0]`; both capped at [`MAX_PORT_CASE_SPLIT`] values. A
//!   variable assigned twice, assigned from another local variable, read in the antecedent, or
//!   assigned inside a `##` chain antecedent stays refused, each with its reason.
//!
//! All three are exact; none approximates.

use std::collections::HashMap;

use serde_json::{Value, json};

/// The largest number of elements a variable-index case split expands to — and the largest
/// local-variable domain [`expand_local_vars`] enumerates.
pub const MAX_PORT_CASE_SPLIT: u64 = 64;

/// What a local variable's type ranges over, read off the slang AST once per translation.
#[derive(Debug, Default, Clone)]
pub struct LocalVarDomains {
    /// Every `TypeAlias` addr → its target type text (`"logic[7:0]"`, `"<addr> pkg::sts_e"`), and
    /// every `EnumType` addr → its packed base type (`collect_type_aliases` in `translate`).
    pub aliases: HashMap<String, String>,
    /// Every `EnumType` addr → its members `(name, value)` (`collect_enum_members`).
    pub enums: HashMap<String, Vec<(String, i64)>>,
}

/// A resolved local-variable domain: the values, and whether they are an enum's members (which
/// gets the coverage conjunct) or the full range of a packed vector (which needs none).
struct Domain {
    values: Vec<i64>,
    is_enum: bool,
    /// The packed type text the substituted literals carry (`"logic[9:0]"`).
    packed: String,
}

impl LocalVarDomains {
    /// The values a local variable of slang type text `ty` ranges over.
    fn domain(&self, ty: &str) -> Result<Domain, String> {
        let mut ty = ty.to_string();
        for _ in 0..8 {
            // `"<addr> pkg::name"` — an alias or an enum, by address
            if let Some((addr, _)) = ty.split_once(' ')
                && addr.chars().all(|c| c.is_ascii_digit())
            {
                if let Some(members) = self.enums.get(addr) {
                    let packed = self.aliases.get(addr).cloned().unwrap_or_default();
                    return Ok(Domain {
                        values: members.iter().map(|(_, v)| *v).collect(),
                        is_enum: true,
                        packed,
                    });
                }
                match self.aliases.get(addr) {
                    Some(target) => {
                        ty = target.clone();
                        continue;
                    }
                    None => {
                        return Err(format!("its type `{ty}` is not an enum or a packed vector"));
                    }
                }
            }
            // `logic[hi:lo]` / `bit[hi:lo]` / `logic` / `bit`
            let width = packed_width(&ty)
                .ok_or_else(|| format!("its type `{ty}` is not an enum or a packed vector"))?;
            if width > MAX_PORT_CASE_SPLIT.trailing_zeros() as u64 {
                return Err(format!(
                    "its type `{ty}` has 2^{width} values, over the {MAX_PORT_CASE_SPLIT}-value \
                     expansion cap"
                ));
            }
            return Ok(Domain {
                values: (0..(1i64 << width)).collect(),
                is_enum: false,
                packed: ty,
            });
        }
        Err(format!("its type `{ty}` aliases too deeply"))
    }
}

/// The bit width of a packed scalar/vector type text: `logic[9:0]` → 10, `bit` → 1; `None` for
/// anything else (unpacked, struct, real, …).
fn packed_width(ty: &str) -> Option<u64> {
    let ty = ty.trim();
    let (base, range) = match ty.find('[') {
        Some(i) => (&ty[..i], Some(&ty[i..])),
        None => (ty, None),
    };
    if !matches!(base.trim(), "logic" | "bit" | "reg") {
        return None;
    }
    let Some(range) = range else {
        return Some(1);
    };
    let inner = range.strip_prefix('[')?.strip_suffix(']')?;
    let (hi, lo) = inner.split_once(':')?;
    let (hi, lo): (i64, i64) = (hi.trim().parse().ok()?, lo.trim().parse().ok()?);
    Some((hi - lo).unsigned_abs() + 1)
}

/// Replace every `AssertionInstance` by its bound `body`, recursively; a property instance with
/// local variables is expanded over their domains ([`expand_local_vars`]).
///
/// `Err` (the reason) for a sequence instance with local variables, a recursive property, a
/// local variable outside the expandable shape, and for a repetition applied to an instance
/// whose body is not a plain boolean.
pub fn inline_assertion_instances(
    node: &Value,
    domains: &LocalVarDomains,
) -> Result<Value, String> {
    let out = inline(node, domains)?;
    Ok(splice_concats(&out))
}

fn instance_name(inst: &Value) -> &str {
    inst.get("symbol")
        .and_then(Value::as_str)
        .and_then(|s| s.split_whitespace().last())
        .unwrap_or("<instance>")
}

fn has_local_vars(inst: &Value) -> bool {
    inst.get("localVars")
        .and_then(Value::as_array)
        .is_some_and(|v| !v.is_empty())
}

fn instance_body(inst: &Value) -> Result<&Value, String> {
    let name = instance_name(inst);
    if has_local_vars(inst) {
        return Err(format!(
            "`{name}` declares local variables (a match-item assignment) in sequence position — \
             only a property instance's local variables are expanded"
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

fn inline(node: &Value, domains: &LocalVarDomains) -> Result<Value, String> {
    match node {
        Value::Object(map) => {
            // A sequence instance in sequence position: `Simple { expr: AssertionInstance }`.
            if map.get("kind").and_then(Value::as_str) == Some("Simple")
                && let Some(inst) = map.get("expr")
                && inst.get("kind").and_then(Value::as_str) == Some("AssertionInstance")
            {
                // slang wraps a PROPERTY instance in `Simple` too (`type: "property"`, no
                // repetition); with local variables it is expanded, the wrapper dropped.
                if inst.get("type").and_then(Value::as_str) == Some("property")
                    && has_local_vars(inst)
                    && map.get("repetition").is_none()
                {
                    return inline(&expand_local_vars(inst, domains)?, domains);
                }
                let body = inline(instance_body(inst)?, domains)?;
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
            // A property instance in property position; with local variables, expanded over
            // their domains first (the expansion's body may hold further instances).
            if map.get("kind").and_then(Value::as_str) == Some("AssertionInstance") {
                if has_local_vars(node) {
                    return inline(&expand_local_vars(node, domains)?, domains);
                }
                return inline(instance_body(node)?, domains);
            }
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), inline(v, domains)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(a) => Ok(Value::Array(
            a.iter()
                .map(|v| inline(v, domains))
                .collect::<Result<_, _>>()?,
        )),
        other => Ok(other.clone()),
    }
}

/// One local variable of a property declaration, as slang lists it under `localVars`.
struct LocalVar {
    name: String,
    /// The address every `NamedValue` of the variable carries in its `symbol` (`"<addr> <name>"`).
    addr: String,
    ty: String,
}

/// Does `node` reference (as a `NamedValue`) any local variable whose addr is in `vars`?
/// Returns the first such variable's name.
fn references_local(node: &Value, vars: &[LocalVar]) -> Option<String> {
    match node {
        Value::Object(m) => {
            if m.get("kind").and_then(Value::as_str) == Some("NamedValue")
                && let Some(sym) = m.get("symbol").and_then(Value::as_str)
                && let Some(addr) = sym.split_whitespace().next()
                && let Some(v) = vars.iter().find(|v| v.addr == addr)
            {
                return Some(v.name.clone());
            }
            m.values().find_map(|v| references_local(v, vars))
        }
        Value::Array(a) => a.iter().find_map(|v| references_local(v, vars)),
        _ => None,
    }
}

/// `node` with every `NamedValue` of the local variable `addr` replaced by the integer literal
/// `value` (typed `packed`, as `resolve_enum_refs` types an enum member: the expression compiler
/// sizes the literal by it).
fn substitute_local(node: &Value, addr: &str, value: i64, packed: &str) -> Value {
    match node {
        Value::Object(m) => {
            if m.get("kind").and_then(Value::as_str) == Some("NamedValue")
                && m.get("symbol")
                    .and_then(Value::as_str)
                    .and_then(|s| s.split_whitespace().next())
                    == Some(addr)
            {
                let mut lit = json!({
                    "kind": "IntegerLiteral",
                    "constant": value.to_string(),
                    "value": value.to_string(),
                });
                if !packed.is_empty() {
                    lit["type"] = json!(packed);
                } else if let Some(ty) = m.get("type") {
                    lit["type"] = ty.clone();
                }
                return lit;
            }
            Value::Object(
                m.iter()
                    .map(|(k, v)| (k.clone(), substitute_local(v, addr, value, packed)))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|v| substitute_local(v, addr, value, packed))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn eq_literal(expr: &Value, value: i64, packed: &str) -> Value {
    let mut lit = json!({"kind": "IntegerLiteral", "constant": value.to_string(),
                         "value": value.to_string()});
    if !packed.is_empty() {
        lit["type"] = json!(packed);
    } else if let Some(ty) = expr.get("type") {
        lit["type"] = ty.clone();
    }
    json!({"kind": "BinaryOp", "op": "Equality", "type": "logic", "left": expr.clone(), "right": lit})
}

fn logical(op: &str, a: Value, b: Value) -> Value {
    json!({"kind": "BinaryOp", "op": op, "type": "logic", "left": a, "right": b})
}

/// A property instance whose declaration has local variables, expanded over their domains — the
/// module doc's "Local variables of a property instance". The result is a property `Binary{And}`
/// chain of implications (plus the coverage implication for each enum-typed variable); `Err`
/// names the variable and why its shape is not the expandable one.
pub fn expand_local_vars(inst: &Value, domains: &LocalVarDomains) -> Result<Value, String> {
    let name = instance_name(inst);
    if inst.get("isRecursiveProperty").and_then(Value::as_bool) == Some(true) {
        return Err(format!(
            "`{name}` is a recursive property — out of fragment"
        ));
    }
    let vars: Vec<LocalVar> = inst
        .get("localVars")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| LocalVar {
                    name: v
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                    addr: v.get("addr").map(|a| a.to_string()).unwrap_or_default(),
                    ty: v
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let body = inst
        .get("body")
        .ok_or_else(|| format!("`{name}`: the instance carries no body"))?;
    let refuse = |what: &str| -> String {
        format!(
            "`{name}` declares local variables and {what} — only `(ante, v = E) |-> C(v)` \
                 with `v` assigned once, from an expression free of local variables, and read \
                 only in the consequent, expands over `v`'s domain"
        )
    };
    // The shape: an implication whose antecedent is one match-item block over a boolean.
    let op = body.get("op").and_then(Value::as_str).unwrap_or("");
    if body.get("kind").and_then(Value::as_str) != Some("Binary")
        || !matches!(op, "OverlappedImplication" | "NonOverlappedImplication")
    {
        return Err(refuse("its body is not an implication"));
    }
    let (left, right) = (
        body.get("left")
            .ok_or_else(|| refuse("the implication has no antecedent"))?,
        body.get("right")
            .ok_or_else(|| refuse("the implication has no consequent"))?,
    );
    // Every assignment anywhere in the antecedent, per variable.
    let mut assigned: HashMap<String, Vec<Value>> = HashMap::new();
    fn collect_assignments(node: &Value, out: &mut HashMap<String, Vec<Value>>) {
        match node {
            Value::Object(m) => {
                if m.get("kind").and_then(Value::as_str) == Some("SequenceWithMatch")
                    && let Some(items) = m.get("matchItems").and_then(Value::as_array)
                {
                    for it in items {
                        if it.get("kind").and_then(Value::as_str) == Some("Assignment")
                            && let Some(addr) = it
                                .get("left")
                                .and_then(|l| l.get("symbol"))
                                .and_then(Value::as_str)
                                .and_then(|s| s.split_whitespace().next())
                            && let Some(rhs) = it.get("right")
                        {
                            out.entry(addr.to_string()).or_default().push(rhs.clone());
                        }
                    }
                }
                m.values().for_each(|v| collect_assignments(v, out));
            }
            Value::Array(a) => a.iter().for_each(|v| collect_assignments(v, out)),
            _ => {}
        }
    }
    collect_assignments(left, &mut assigned);
    for v in &vars {
        match assigned.get(&v.addr).map(Vec::len).unwrap_or(0) {
            0 => {
                return Err(refuse(&format!(
                    "`{}` is never assigned in the antecedent",
                    v.name
                )));
            }
            1 => {}
            _ => return Err(refuse(&format!("`{}` is assigned twice", v.name))),
        }
        if let Some(other) = references_local(&assigned[&v.addr][0], &vars) {
            return Err(refuse(&format!(
                "`{}` is assigned from another local variable (`{other}`)",
                v.name
            )));
        }
    }
    // A local variable read anywhere in the antecedent outside the match items (its own
    // assignments) is a value the expansion cannot pin — refused before the shape check so the
    // reason names it.
    fn reads_outside_assignments(node: &Value, vars: &[LocalVar]) -> Option<String> {
        match node {
            Value::Object(m) => {
                if m.get("kind").and_then(Value::as_str) == Some("NamedValue") {
                    return references_local(node, vars);
                }
                m.iter()
                    .filter(|(k, _)| k.as_str() != "matchItems")
                    .find_map(|(_, v)| reads_outside_assignments(v, vars))
            }
            Value::Array(a) => a.iter().find_map(|v| reads_outside_assignments(v, vars)),
            _ => None,
        }
    }
    if let Some(v) = reads_outside_assignments(left, &vars) {
        return Err(refuse(&format!("`{v}` is read in the antecedent")));
    }
    // The antecedent must be exactly one match-item block over a boolean …
    let (ante_expr, items) = match (
        left.get("kind").and_then(Value::as_str),
        left.get("expr"),
        left.get("matchItems").and_then(Value::as_array),
    ) {
        (Some("SequenceWithMatch"), Some(e), Some(items))
            if e.get("kind").and_then(Value::as_str) == Some("Simple")
                && e.get("repetition").is_none() =>
        {
            (
                e.get("expr")
                    .ok_or_else(|| refuse("the antecedent has no expression"))?,
                items,
            )
        }
        _ => {
            return Err(refuse(
                "a local variable is assigned inside a sequence antecedent",
            ));
        }
    };
    if items.len() != vars.len() {
        return Err(refuse("a match item is not a local-variable assignment"));
    }
    // The domains, and the expansion size.
    let doms: Vec<Domain> = vars
        .iter()
        .map(|v| {
            domains
                .domain(&v.ty)
                .map_err(|why| refuse(&format!("`{}`: {why}", v.name)))
        })
        .collect::<Result<_, _>>()?;
    let total = doms
        .iter()
        .try_fold(1u64, |acc, d| acc.checked_mul(d.values.len() as u64))
        .unwrap_or(u64::MAX);
    if total == 0 || total > MAX_PORT_CASE_SPLIT {
        return Err(refuse(&format!(
            "the expansion would have {total} cases, over the {MAX_PORT_CASE_SPLIT}-case cap"
        )));
    }
    // Every assignment of values to variables (the product of the domains).
    let mut tuples: Vec<Vec<i64>> = vec![Vec::new()];
    for d in &doms {
        tuples = tuples
            .iter()
            .flat_map(|t| {
                d.values.iter().map(move |v| {
                    let mut t2 = t.clone();
                    t2.push(*v);
                    t2
                })
            })
            .collect();
    }
    let mut conjuncts: Vec<Value> = Vec::with_capacity(tuples.len() + vars.len());
    for tuple in &tuples {
        let mut ante = ante_expr.clone();
        let mut cons = right.clone();
        for ((v, d), value) in vars.iter().zip(&doms).zip(tuple) {
            ante = logical(
                "LogicalAnd",
                ante,
                eq_literal(&assigned[&v.addr][0], *value, &d.packed),
            );
            cons = substitute_local(&cons, &v.addr, *value, &d.packed);
        }
        conjuncts.push(json!({"kind": "Binary", "op": op,
                              "left": {"kind": "Simple", "expr": ante}, "right": cons}));
    }
    // Coverage: for an enum-typed variable, `E` is one of the members at the match — a HOLDS
    // never rests on `E` leaving its encoding (a violation here is a domain escape).
    for (v, d) in vars.iter().zip(&doms) {
        if !d.is_enum {
            continue;
        }
        let e = &assigned[&v.addr][0];
        let mut any = eq_literal(e, d.values[0], &d.packed);
        for value in &d.values[1..] {
            any = logical("LogicalOr", any, eq_literal(e, *value, &d.packed));
        }
        conjuncts.push(json!({"kind": "Binary", "op": op,
                              "left": {"kind": "Simple", "expr": ante_expr.clone()},
                              "right": {"kind": "Simple", "expr": any}}));
    }
    let mut out = conjuncts.remove(0);
    for c in conjuncts {
        out = json!({"kind": "Binary", "op": "And", "left": out, "right": c});
    }
    Ok(out)
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
        let out =
            inline_assertion_instances(&concat, &LocalVarDomains::default()).expect("inlines");
        let el = &out["elements"][0];
        assert_eq!(el["min"], 1);
        assert_eq!(el["sequence"]["kind"], "Simple");
        assert_eq!(el["sequence"]["repetition"]["min"], 2);
        assert_eq!(el["sequence"]["expr"]["op"], "Equality");
        assert!(!out.to_string().contains("AssertionInstance"));
    }

    #[test]
    fn sequence_position_local_variables_and_recursion_stay_refused() {
        let d = LocalVarDomains::default();
        let with_local = json!({"kind": "Simple",
                                "expr": instance("9 Linear_P", json!([{"name": "init_s"}]))});
        let e = inline_assertion_instances(&with_local, &d).unwrap_err();
        assert!(e.contains("sequence position"), "{e}");
        let mut rec = instance("9 R", json!([]));
        rec["isRecursiveProperty"] = json!(true);
        let e = inline_assertion_instances(&rec, &d).unwrap_err();
        assert!(e.contains("recursive"), "{e}");
    }

    /// The `ASSERT_FPV_LINEAR_FSM` property as slang 11 serialises it (captured 2026-10-23 in
    /// `testdata/sva_local_var_forms.ast.json`): `(!$stable(s) & cond, init = $past(s)) |->
    /// (s != init) until !cond`, `init` of the state's enum type.
    fn linear(local_vars: Value, ante_extra: Option<Value>, cons_reads: &str) -> Value {
        let s = named("50 state_q", "60 pkg::fsm_state_e");
        let past = json!({"kind": "Call", "subroutine": "$past", "type": "60 pkg::fsm_state_e",
                          "arguments": [s.clone()]});
        let mut ante = json!({"kind": "BinaryOp", "op": "BinaryAnd", "type": "logic",
                              "left": {"kind": "UnaryOp", "op": "LogicalNot", "type": "bit",
                                       "operand": {"kind": "Call", "subroutine": "$stable",
                                                   "type": "bit", "arguments": [s.clone()]}},
                              "right": named("51 cond", "bit")});
        if let Some(x) = ante_extra {
            ante = json!({"kind": "BinaryOp", "op": "LogicalAnd", "type": "logic", "left": ante,
                          "right": x});
        }
        json!({"kind": "AssertionInstance", "type": "property", "symbol": "70 Linear_P",
               "isRecursiveProperty": false, "localVars": local_vars,
               "body": {"kind": "Binary", "op": "OverlappedImplication",
                        "left": {"kind": "SequenceWithMatch",
                                 "expr": {"kind": "Simple", "expr": ante},
                                 "matchItems": [{"kind": "Assignment", "type": "60 pkg::fsm_state_e",
                                                 "left": named("80 initial_state", "60 pkg::fsm_state_e"),
                                                 "right": past}]},
                        "right": {"kind": "Binary", "op": "Until",
                                  "left": {"kind": "Simple", "expr": {"kind": "BinaryOp",
                                           "op": "Inequality", "type": "logic", "left": s,
                                           "right": named(cons_reads, "60 pkg::fsm_state_e")}},
                                  "right": {"kind": "Simple", "expr": {"kind": "UnaryOp",
                                            "op": "LogicalNot", "type": "logic",
                                            "operand": named("51 cond", "bit")}}}}})
    }

    fn enum_domains() -> LocalVarDomains {
        let mut d = LocalVarDomains::default();
        d.aliases.insert("60".into(), "logic[9:0]".into());
        d.enums.insert(
            "60".into(),
            vec![("A".into(), 201), ("B".into(), 185), ("C".into(), 921)],
        );
        d
    }

    #[test]
    fn a_local_variable_over_an_enum_expands_to_one_implication_per_member_plus_coverage() {
        let inst = linear(
            json!([{"name": "initial_state", "addr": 80, "type": "60 pkg::fsm_state_e"}]),
            None,
            "80 initial_state",
        );
        let out = inline_assertion_instances(&inst, &enum_domains()).expect("expands");
        let s = out.to_string();
        assert!(
            !s.contains("AssertionInstance") && !s.contains("initial_state"),
            "{s}"
        );
        // three member cases + the coverage conjunct = four implications under three `And`s
        assert_eq!(s.matches("\"op\":\"And\"").count(), 3, "{s}");
        assert_eq!(s.matches("OverlappedImplication").count(), 4, "{s}");
        // each case pins `$past(state_q) == d` in the antecedent and substitutes `d` in the body
        for d in ["201", "185", "921"] {
            assert!(
                s.contains(&format!("\"constant\":\"{d}\"")),
                "{d} missing in {s}"
            );
        }
        assert!(
            s.contains("\"type\":\"logic[9:0]\""),
            "the literal carries the packed type: {s}"
        );
        assert!(
            s.contains("LogicalOr"),
            "the coverage disjunction is present: {s}"
        );
    }

    #[test]
    fn a_packed_local_variable_expands_over_its_range_without_coverage() {
        let mut inst = linear(
            json!([{"name": "init_s", "addr": 80, "type": "logic[1:0]"}]),
            None,
            "80 init_s",
        );
        inst["body"]["left"]["matchItems"][0]["left"] = named("80 init_s", "logic[1:0]");
        let out = inline_assertion_instances(&inst, &LocalVarDomains::default()).expect("expands");
        let s = out.to_string();
        assert_eq!(
            s.matches("OverlappedImplication").count(),
            4,
            "2^2 cases, no coverage: {s}"
        );
        assert!(!s.contains("LogicalOr"), "{s}");
    }

    #[test]
    fn local_variable_shapes_outside_the_expansion_are_refused_with_their_reason() {
        let d = enum_domains();
        // read in the antecedent
        let inst = linear(
            json!([{"name": "initial_state", "addr": 80, "type": "60 pkg::fsm_state_e"}]),
            Some(named("80 initial_state", "60 pkg::fsm_state_e")),
            "80 initial_state",
        );
        let e = inline_assertion_instances(&inst, &d).unwrap_err();
        assert!(e.contains("read in the antecedent"), "{e}");
        // assigned twice (a second match block in a chain antecedent)
        let mut inst = linear(
            json!([{"name": "initial_state", "addr": 80, "type": "60 pkg::fsm_state_e"}]),
            None,
            "80 initial_state",
        );
        let first = inst["body"]["left"].clone();
        inst["body"]["left"] = json!({"kind": "SequenceConcat", "elements": [
            {"sequence": first.clone(), "min": 0, "max": 0},
            {"sequence": first, "min": 1, "max": 1}]});
        let e = inline_assertion_instances(&inst, &d).unwrap_err();
        assert!(e.contains("assigned twice"), "{e}");
        // assigned from another local variable
        let mut inst = linear(
            json!([{"name": "a", "addr": 80, "type": "logic[1:0]"},
                                     {"name": "b", "addr": 81, "type": "logic[1:0]"}]),
            None,
            "81 b",
        );
        inst["body"]["left"]["matchItems"] = json!([
            {"kind": "Assignment", "left": named("80 a", "logic[1:0]"), "right": named("1 x", "logic[1:0]")},
            {"kind": "Assignment", "left": named("81 b", "logic[1:0]"), "right": named("80 a", "logic[1:0]")}]);
        let e = inline_assertion_instances(&inst, &d).unwrap_err();
        assert!(e.contains("another local variable"), "{e}");
        // a domain over the cap
        let inst = linear(
            json!([{"name": "init_s", "addr": 80, "type": "logic[9:0]"}]),
            None,
            "80 init_s",
        );
        let e = inline_assertion_instances(&inst, &d).unwrap_err();
        assert!(e.contains("2^10") || e.contains("cap"), "{e}");
        // an unknown type
        let inst = linear(
            json!([{"name": "init_s", "addr": 80, "type": "real"}]),
            None,
            "80 init_s",
        );
        let e = inline_assertion_instances(&inst, &d).unwrap_err();
        assert!(e.contains("not an enum or a packed vector"), "{e}");
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
        let out = inline_assertion_instances(&outer, &LocalVarDomains::default()).expect("inlines");
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
