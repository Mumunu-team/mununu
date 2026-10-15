//! Minted expressions — the SVA expression compiler (OpenTitan coverage, Phase 2 + 3).
//!
//! > Source of truth: [`build_expr`] + [`ExprIr::minted_name`] — surface: (CLI+API), through
//! > `sv verify-auto`.
//!
//! The SVA translator lowers a property to mu-calculus over ATOMS: a signal, a signal against a
//! literal, a relational pair. An operand that is an *expression* — `cnt_q[1] + cnt_q[0]`,
//! `{2'b0, step_i}`, `$past(cnt_o) + 1`, `gnt_o[idx_o]`, `min(a, b)` — is not an atom. This module
//! compiles such an expression into an [`ExprIr`], a width-checked expression tree, which
//! `btor2::minted::augment_with_expressions` emits into the lifted model as ONE named combinational
//! node `mununu_x_<hash>`. The formula then reads that node as an atom, exactly as it reads a
//! minted slice (`x__bitsM_L`) or count (`x__popcount`): no abstraction, no new engine.
//!
//! **Typing follows slang, never guesses.** Every slang expression node carries its result `type`
//! after SystemVerilog's context-determined sizing, and every implicit width or sign change is an
//! explicit `Conversion` node. The builder reads widths from those types and turns each
//! `Conversion` into a zero- or sign-extension (by the OPERAND's signedness, SV 11.8.2) or a
//! truncation. It then CHECKS the invariants slang guarantees — both operands of an arithmetic or
//! bitwise operator have the result's width; both operands of a comparison have equal width — and
//! refuses with "unexpected width mismatch" rather than repair one. A comparison is signed iff
//! slang typed both operands signed (SV 11.8.1).
//!
//! **`$past` is pushed to the leaves.** `$past(e, k)` is `e` with every signal `x` read from its
//! depth-`k` shadow `x__pastk` (`$past(a + b) = $past(a) + $past(b)`, exact: an expression is a
//! function of the current values of its leaves). Each leaf records the shadow it needs. A gated
//! or clocked `$past` is refused.
//!
//! **User functions are inlined** when the body is exactly one `return` (OpenTitan's
//! `min` / `max`): formals bind to the (already converted) actual arguments; depth ≤ 8.
//!
//! **Refused, with a reason:** `/ % **`, a multiplication wider than 32 bits, X/Z constants, a
//! non-constant replication count, an unpacked-array element, a dynamic index that can fall
//! outside the declared range (SV reads X there; the model is 2-state), an indexed part-select,
//! a function body other than a single `return`, other system calls.

use std::collections::HashMap;

use serde_json::Value;

use crate::adapter::btor2::minted_expr::mask;
pub use crate::adapter::btor2::minted_expr::{BinOp, CmpOp, ExprIr, RedOp};

use super::translate::{
    is_unpacked_array, packed_dims, packed_sub, past_shadow_name, signal_name, type_width,
    typed_signed,
};

/// Parse a slang constant string (`4'b1111`, `6'sd15`, `-4'sd8`, `32'd1`, `0`, `1'b1`) to its
/// bits at `width`. `None` for X/Z digits, a malformed string or a width above 128.
pub fn parse_const(raw: &str, width: u32) -> Option<u128> {
    if width == 0 || width > 128 {
        return None;
    }
    let s: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '_')
        .collect();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest.to_string()),
        None => (false, s),
    };
    let mag = match s.find('\'') {
        None => s.parse::<u128>().ok()?,
        Some(tick) => {
            let rest = &s[tick + 1..];
            let rest = rest.strip_prefix(['s', 'S']).unwrap_or(rest);
            let mut chars = rest.chars();
            let radix = match chars.next()?.to_ascii_lowercase() {
                'b' => 2,
                'o' => 8,
                'd' => 10,
                'h' => 16,
                _ => return None,
            };
            let digits: String = chars.collect();
            if digits.is_empty() {
                return None;
            }
            u128::from_str_radix(&digits, radix).ok()?
        }
    };
    let v = if neg { mag.wrapping_neg() } else { mag };
    Some(v & mask(width))
}

/// What a successful build needs from the model: the `__past` shadows its leaves read.
#[derive(Debug, Default, Clone)]
pub struct BuildCtx {
    /// User subroutines by slang address (`"93825731134184"`) → their `Subroutine` node.
    pub subroutines: HashMap<String, Value>,
    /// `(base, depth)` shadows the built expression reads.
    pub shadows: Vec<(String, u32, u32)>,
    /// Formal-argument bindings while inlining, by the formal's slang address.
    bindings: HashMap<String, ExprIr>,
    /// The `$past` depth in force (0 = the current cycle).
    past: u32,
    /// Inlining depth.
    inline_depth: u32,
}

impl BuildCtx {
    /// A context that can inline the given subroutines.
    pub fn with_subroutines(subroutines: HashMap<String, Value>) -> Self {
        BuildCtx {
            subroutines,
            ..Default::default()
        }
    }

    fn need_shadow(&mut self, base: &str, width: u32, depth: u32) {
        match self.shadows.iter_mut().find(|(b, _, _)| b == base) {
            Some(s) => s.2 = s.2.max(depth),
            None => self.shadows.push((base.to_string(), width, depth)),
        }
    }

    /// A leaf for signal `name` read in the current `$past` context.
    fn leaf(&mut self, name: &str, width: u32) -> ExprIr {
        if self.past == 0 {
            ExprIr::Leaf {
                name: name.to_string(),
                width,
            }
        } else {
            self.need_shadow(name, width, self.past);
            ExprIr::Leaf {
                name: past_shadow_name(name, self.past),
                width,
            }
        }
    }
}

/// Collect the user subroutines of a slang AST by address.
pub fn collect_subroutines(root: &Value) -> HashMap<String, Value> {
    fn walk(n: &Value, out: &mut HashMap<String, Value>) {
        match n {
            Value::Object(m) => {
                if m.get("kind").and_then(Value::as_str) == Some("Subroutine")
                    && let Some(addr) = m.get("addr").and_then(Value::as_u64)
                {
                    out.insert(addr.to_string(), n.clone());
                }
                for v in m.values() {
                    walk(v, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = HashMap::new();
    walk(root, &mut out);
    out
}

fn node_width(n: &Value) -> Result<u32, String> {
    let ty = n.get("type").and_then(Value::as_str).unwrap_or("");
    type_width(ty).ok_or_else(|| format!("expression of unreadable type `{ty}`"))
}

fn kind(n: &Value) -> &str {
    n.get("kind").and_then(Value::as_str).unwrap_or("?")
}

fn same_width(what: &str, want: u32, parts: &[&ExprIr]) -> Result<(), String> {
    if parts.iter().all(|p| p.width() == want) {
        Ok(())
    } else {
        Err(format!(
            "unexpected width mismatch in `{what}`: result {want} bits, operands {:?} — refusing \
             to guess the conversion",
            parts.iter().map(|p| p.width()).collect::<Vec<_>>()
        ))
    }
}

/// `x != 0` as a 1-bit expression (the truth value SV gives a vector in a logical context).
fn truth(e: ExprIr) -> ExprIr {
    if e.width() == 1 {
        e
    } else {
        ExprIr::Red {
            op: RedOp::Or,
            a: Box::new(e),
        }
    }
}

fn konst(bits: u128, width: u32) -> ExprIr {
    ExprIr::Const {
        bits: bits & mask(width),
        width,
    }
}

/// Bring `e` to `w` bits: zero/sign-extend or truncate.
fn resize(e: ExprIr, w: u32, signed: bool) -> ExprIr {
    let ew = e.width();
    if ew == w {
        e
    } else if ew < w {
        ExprIr::Ext {
            signed,
            a: Box::new(e),
            by: w - ew,
        }
    } else {
        ExprIr::Slice {
            a: Box::new(e),
            hi: w - 1,
            lo: 0,
        }
    }
}

/// Build the expression `node` (a slang expression JSON node, `Conversion` wrappers included).
pub fn build_expr(node: &Value, cx: &mut BuildCtx) -> Result<ExprIr, String> {
    let k = kind(node);
    // A bound formal argument first: its value is the call's argument, never a constant of the
    // definition.
    if k == "NamedValue" {
        let sym = node.get("symbol").and_then(Value::as_str).unwrap_or("");
        if let Some(bound) = cx.bindings.get(sym.split(' ').next().unwrap_or("")) {
            return Ok(bound.clone());
        }
    }
    // A constant slang folded (`{W{1'b1}}`, `W - 1`, a converted literal, a parameter) is a
    // literal of the node's own type. A signal never carries `constant`.
    if let Some(c) = node.get("constant").and_then(Value::as_str) {
        let w = node_width(node)?;
        return parse_const(c, w)
            .map(|bits| konst(bits, w))
            .ok_or_else(|| format!("constant `{c}` is X/Z, malformed or wider than 128 bits"));
    }
    match k {
        "Conversion" => {
            let operand = node.get("operand").ok_or("Conversion without an operand")?;
            let inner = build_expr(operand, cx)?;
            let w = node_width(node)?;
            // SV 11.8.2: inside an expression an operand is extended by the EXPRESSION's
            // signedness — the type slang converts it to — not its own: a signed `s` widened
            // into an unsigned comparison is ZERO-extended. (Measured against yosys-slang's
            // elaboration by `e2e_minted_expressions_agree_with_yosys_elaboration`.) An
            // assignment-like conversion — a function argument — is the exception, handled in
            // `inline_call`.
            Ok(resize(inner, w, typed_signed(node)))
        }
        "IntegerLiteral" | "UnbasedUnsizedIntegerLiteral" => {
            let w = node_width(node)?;
            let raw = node
                .get("value")
                .and_then(Value::as_str)
                .ok_or("literal without a value")?;
            parse_const(raw, w)
                .map(|bits| konst(bits, w))
                .ok_or_else(|| format!("literal `{raw}` is X/Z or malformed"))
        }
        "NamedValue" => {
            if cx.inline_depth > 0 {
                return Err(format!(
                    "function body reads `{}`, which is not one of its arguments",
                    signal_name(node).unwrap_or("?")
                ));
            }
            if is_unpacked_array(node) {
                return Err(format!(
                    "`{}` is an unpacked array; an expression over array content is not supported",
                    signal_name(node).unwrap_or("?")
                ));
            }
            let name = signal_name(node)?.to_string();
            let w = node_width(node)?;
            Ok(cx.leaf(&name, w))
        }
        "ElementSelect" | "RangeSelect" | "MemberAccess" => build_select(node, cx),
        "UnaryOp" => {
            let op = node.get("op").and_then(Value::as_str).unwrap_or("?");
            let a = build_expr(node.get("operand").ok_or("UnaryOp without operand")?, cx)?;
            let w = node_width(node)?;
            let red = |op: RedOp, a: ExprIr| ExprIr::Red { op, a: Box::new(a) };
            let e = match op {
                "BitwiseNot" => ExprIr::Not(Box::new(a)),
                "Minus" => ExprIr::Neg(Box::new(a)),
                "Plus" => a,
                "LogicalNot" => ExprIr::Not(Box::new(truth(a))),
                "BitwiseAnd" => red(RedOp::And, a),
                "BitwiseOr" => red(RedOp::Or, a),
                "BitwiseXor" => red(RedOp::Xor, a),
                "BitwiseNand" => ExprIr::Not(Box::new(red(RedOp::And, a))),
                "BitwiseNor" => ExprIr::Not(Box::new(red(RedOp::Or, a))),
                "BitwiseXnor" => ExprIr::Not(Box::new(red(RedOp::Xor, a))),
                other => return Err(format!("unary operator `{other}` in an expression")),
            };
            same_width(op, w, &[&e])?;
            Ok(e)
        }
        "BinaryOp" => build_binary(node, cx),
        "ConditionalOp" => {
            let conds = node
                .get("conditions")
                .and_then(Value::as_array)
                .ok_or("ConditionalOp without conditions")?;
            let [cond] = conds.as_slice() else {
                return Err("a `?:` with a pattern-matching condition".to_string());
            };
            if cond.get("pattern").is_some() {
                return Err("a `?:` with a pattern-matching condition".to_string());
            }
            let c = truth(build_expr(
                cond.get("expr").ok_or("condition without expr")?,
                cx,
            )?);
            let t = build_expr(node.get("left").ok_or("?: without left")?, cx)?;
            let e = build_expr(node.get("right").ok_or("?: without right")?, cx)?;
            let w = node_width(node)?;
            same_width("?:", w, &[&t, &e])?;
            Ok(ExprIr::Ite {
                c: Box::new(c),
                t: Box::new(t),
                e: Box::new(e),
            })
        }
        "Concatenation" => {
            let ops = node
                .get("operands")
                .and_then(Value::as_array)
                .ok_or("Concatenation without operands")?;
            let mut parts = Vec::with_capacity(ops.len());
            for o in ops {
                parts.push(build_expr(o, cx)?);
            }
            let e = concat_all(parts).ok_or("an empty concatenation")?;
            same_width("{…}", node_width(node)?, &[&e])?;
            Ok(e)
        }
        "Replication" => {
            let count = node
                .get("count")
                .and_then(|c| {
                    c.get("constant")
                        .or_else(|| c.get("value"))
                        .and_then(Value::as_str)
                })
                .and_then(|c| parse_const(c, 32))
                .ok_or("a replication whose count is not a constant")?;
            if count == 0 || count > 1024 {
                return Err(format!("a replication count of {count}"));
            }
            let inner = build_expr(node.get("concat").ok_or("Replication without concat")?, cx)?;
            let e = concat_all(vec![inner; count as usize]).expect("count ≥ 1");
            same_width("{n{…}}", node_width(node)?, &[&e])?;
            Ok(e)
        }
        "Call" => build_call(node, cx),
        other => Err(format!(
            "expression kind `{other}` is not supported in an expression"
        )),
    }
}

fn concat_all(parts: Vec<ExprIr>) -> Option<ExprIr> {
    let mut it = parts.into_iter();
    let first = it.next()?;
    Some(it.fold(first, |hi, lo| ExprIr::Concat {
        hi: Box::new(hi),
        lo: Box::new(lo),
    }))
}

fn build_select(node: &Value, cx: &mut BuildCtx) -> Result<ExprIr, String> {
    // A constant selection chain over one packed signal: the translator's own resolver.
    match packed_sub(node) {
        Some(Ok(sub)) => {
            let base_w = {
                let mut root = node;
                while let Some(v) = root.get("value") {
                    root = v;
                    while kind(root) == "Conversion" {
                        root = root.get("operand").ok_or("Conversion without an operand")?;
                    }
                }
                node_width(root)?
            };
            let leaf = cx.leaf(&sub.base, base_w);
            if sub.lsb == 0 && sub.width == base_w {
                return Ok(leaf);
            }
            return Ok(ExprIr::Slice {
                a: Box::new(leaf),
                hi: sub.lsb + sub.width - 1,
                lo: sub.lsb,
            });
        }
        Some(Err(reason)) => return Err(reason),
        None => {}
    }
    // A dynamic bit-select `v[i]` of a one-dimensional packed vector.
    if kind(node) != "ElementSelect" {
        return Err(format!("a `{}` selection that is not constant", kind(node)));
    }
    let base_node = node.get("value").ok_or("ElementSelect without value")?;
    if is_unpacked_array(base_node) {
        return Err(
            "an element of an unpacked array (array content) is not supported in an expression"
                .to_string(),
        );
    }
    let ty = base_node.get("type").and_then(Value::as_str).unwrap_or("");
    let Some((_, dims)) = packed_dims(ty) else {
        return Err(format!("a dynamic select of a value of type `{ty}`"));
    };
    let [(left, right)] = dims.as_slice() else {
        return Err(
            "a dynamic select of a multi-dimensional packed array (an element, not a bit)"
                .to_string(),
        );
    };
    let (hi, lo) = ((*left).max(*right), (*left).min(*right));
    let base = build_expr(base_node, cx)?;
    let idx_node = node
        .get("selector")
        .ok_or("ElementSelect without selector")?;
    let idx = build_expr(idx_node, cx)?;
    let iw = idx.width();
    if typed_signed(idx_node) {
        return Err("a dynamic select with a signed index".to_string());
    }
    // SOUNDNESS: SV reads X for an index outside the declared range; the model is 2-state and has
    // no X. So the select is minted only when the index CANNOT leave the range: a descending
    // `[N-1:0]` vector whose index cannot exceed N-1. Anything else is refused.
    let max_idx: u128 = mask(iw.min(127));
    if !(lo == 0 && *left >= *right && max_idx <= hi as u128) {
        return Err(format!(
            "a dynamic select whose {iw}-bit index can fall outside [{left}:{right}] (SV reads X \
             there; the model has no X)"
        ));
    }
    let w = base.width();
    let amount = resize(idx, w, false);
    Ok(ExprIr::Slice {
        a: Box::new(ExprIr::Bin {
            op: BinOp::Lshr,
            a: Box::new(base),
            b: Box::new(amount),
        }),
        hi: 0,
        lo: 0,
    })
}

fn build_binary(node: &Value, cx: &mut BuildCtx) -> Result<ExprIr, String> {
    let op = node.get("op").and_then(Value::as_str).unwrap_or("?");
    let (ln, rn) = (
        node.get("left").ok_or("BinaryOp without left")?,
        node.get("right").ok_or("BinaryOp without right")?,
    );
    let w = node_width(node)?;
    let bin = |op: BinOp, a: ExprIr, b: ExprIr| ExprIr::Bin {
        op,
        a: Box::new(a),
        b: Box::new(b),
    };
    let arith = |bop: BinOp, cx: &mut BuildCtx| -> Result<ExprIr, String> {
        let (a, b) = (build_expr(ln, cx)?, build_expr(rn, cx)?);
        same_width(op, w, &[&a, &b])?;
        if bop == BinOp::Mul && w > 32 {
            return Err(format!("a {w}-bit multiplication (the cap is 32 bits)"));
        }
        Ok(bin(bop, a, b))
    };
    let cmp = |cop: CmpOp, cx: &mut BuildCtx| -> Result<ExprIr, String> {
        let (a, b) = (build_expr(ln, cx)?, build_expr(rn, cx)?);
        if a.width() != b.width() {
            return Err(format!(
                "unexpected width mismatch in `{op}`: operands {} and {} bits",
                a.width(),
                b.width()
            ));
        }
        Ok(ExprIr::Cmp {
            op: cop,
            signed: typed_signed(ln) && typed_signed(rn),
            a: Box::new(a),
            b: Box::new(b),
        })
    };
    let logical = |bop: BinOp, cx: &mut BuildCtx| -> Result<ExprIr, String> {
        let a = truth(build_expr(ln, cx)?);
        let b = truth(build_expr(rn, cx)?);
        Ok(bin(bop, a, b))
    };
    match op {
        "Add" => arith(BinOp::Add, cx),
        "Subtract" => arith(BinOp::Sub, cx),
        "Multiply" => arith(BinOp::Mul, cx),
        "BinaryAnd" => arith(BinOp::And, cx),
        "BinaryOr" => arith(BinOp::Or, cx),
        "BinaryXor" => arith(BinOp::Xor, cx),
        "BinaryXnor" => arith(BinOp::Xnor, cx),
        "LogicalShiftLeft" | "ArithmeticShiftLeft" => shift(BinOp::Shl, ln, rn, w, cx),
        "LogicalShiftRight" => shift(BinOp::Lshr, ln, rn, w, cx),
        "ArithmeticShiftRight" => {
            // `$signed(x) >>> k` in an UNSIGNED context: IEEE 1800 11.8.2 makes the operand
            // unsigned, so the fill is 0 (slang's reading); Verilator, iverilog and yosys keep the
            // sign fill (measured 2026-10-15). The design's lift follows the latter, so neither
            // reading is safe to assume here — refused, never guessed.
            if kind(ln) == "Conversion"
                && !typed_signed(ln)
                && ln.get("operand").is_some_and(typed_signed)
            {
                return Err(
                    "`>>>` of a signed operand in an unsigned context: the standard and the \
                     simulators fill differently"
                        .to_string(),
                );
            }
            let bop = if typed_signed(ln) {
                BinOp::Ashr
            } else {
                BinOp::Lshr
            };
            shift(bop, ln, rn, w, cx)
        }
        "Equality" | "CaseEquality" => cmp(CmpOp::Eq, cx),
        "Inequality" | "CaseInequality" => cmp(CmpOp::Ne, cx),
        "LessThan" => cmp(CmpOp::Lt, cx),
        "LessThanEqual" => cmp(CmpOp::Le, cx),
        "GreaterThan" => cmp(CmpOp::Gt, cx),
        "GreaterThanEqual" => cmp(CmpOp::Ge, cx),
        "LogicalAnd" => logical(BinOp::And, cx),
        "LogicalOr" => logical(BinOp::Or, cx),
        "LogicalImplication" => {
            let a = truth(build_expr(ln, cx)?);
            let b = truth(build_expr(rn, cx)?);
            Ok(bin(BinOp::Or, ExprIr::Not(Box::new(a)), b))
        }
        "LogicalEquivalence" => logical(BinOp::Xnor, cx),
        other => Err(format!("binary operator `{other}` in an expression")),
    }
}

/// A shift by a self-determined amount. BTOR2 shifts take the amount at the value's width; an
/// amount ≥ the width fills (0, or the sign for `sra`), which is also SV's result.
fn shift(op: BinOp, ln: &Value, rn: &Value, w: u32, cx: &mut BuildCtx) -> Result<ExprIr, String> {
    let a = build_expr(ln, cx)?;
    same_width("shift", w, &[&a])?;
    let amt = build_expr(rn, cx)?;
    let aw = amt.width();
    let shifted = |amount: ExprIr| ExprIr::Bin {
        op,
        a: Box::new(a.clone()),
        b: Box::new(amount),
    };
    if aw <= w {
        return Ok(shifted(resize(amt, w, false)));
    }
    // A wider amount: ≥ w fills, otherwise its low bits are the amount.
    let fill = if op == BinOp::Ashr {
        ExprIr::Bin {
            op: BinOp::Ashr,
            a: Box::new(a.clone()),
            b: Box::new(konst(u128::from(w - 1), w)),
        }
    } else {
        konst(0, w)
    };
    if aw > 128 {
        return Err(format!("a shift amount of {aw} bits"));
    }
    Ok(ExprIr::Ite {
        c: Box::new(ExprIr::Cmp {
            op: CmpOp::Ge,
            signed: false,
            a: Box::new(amt.clone()),
            b: Box::new(konst(u128::from(w), aw)),
        }),
        t: Box::new(fill),
        e: Box::new(shifted(resize(amt, w, false))),
    })
}

fn call_args(node: &Value) -> Result<&Vec<Value>, String> {
    node.get("arguments")
        .and_then(Value::as_array)
        .ok_or_else(|| "a call without an argument list".to_string())
}

fn build_call(node: &Value, cx: &mut BuildCtx) -> Result<ExprIr, String> {
    let sub = node
        .get("subroutine")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let args = call_args(node)?;
    let w = node_width(node)?;
    match sub {
        "$past" => {
            if args.is_empty() || args.len() > 2 {
                return Err(format!(
                    "$past with {} arguments (a gated or clocked $past is not supported)",
                    args.len()
                ));
            }
            let depth = match args.get(1) {
                None => 1,
                Some(d) => {
                    let dw = node_width(d).unwrap_or(32);
                    let v = d
                        .get("constant")
                        .or_else(|| d.get("value"))
                        .and_then(Value::as_str)
                        .and_then(|c| parse_const(c, dw))
                        .ok_or("a $past depth that is not a constant")?;
                    if v == 0 || v > 64 {
                        return Err(format!("a $past depth of {v}"));
                    }
                    v as u32
                }
            };
            let saved = cx.past;
            cx.past += depth;
            let e = build_expr(&args[0], cx);
            cx.past = saved;
            let e = e?;
            same_width("$past", w, &[&e])?;
            Ok(e)
        }
        "$signed" | "$unsigned" => {
            let [a] = args.as_slice() else {
                return Err(format!("{sub} with {} arguments", args.len()));
            };
            let e = build_expr(a, cx)?;
            same_width(sub, w, &[&e])?;
            Ok(e)
        }
        "$stable" | "$changed" | "$rose" | "$fell" => {
            let [a] = args.as_slice() else {
                return Err(format!("{sub} with {} arguments", args.len()));
            };
            let now = build_expr(a, cx)?;
            let saved = cx.past;
            cx.past += 1;
            let then = build_expr(a, cx);
            cx.past = saved;
            let then = then?;
            let eq = |op| ExprIr::Cmp {
                op,
                signed: false,
                a: Box::new(now.clone()),
                b: Box::new(then.clone()),
            };
            // `$rose` / `$fell` look at the least significant bit (SV 16.9.3).
            let lsb = |e: &ExprIr| ExprIr::Slice {
                a: Box::new(e.clone()),
                hi: 0,
                lo: 0,
            };
            Ok(match sub {
                "$stable" => eq(CmpOp::Eq),
                "$changed" => eq(CmpOp::Ne),
                "$rose" => ExprIr::Bin {
                    op: BinOp::And,
                    a: Box::new(lsb(&now)),
                    b: Box::new(ExprIr::Not(Box::new(lsb(&then)))),
                },
                _ => ExprIr::Bin {
                    op: BinOp::And,
                    a: Box::new(ExprIr::Not(Box::new(lsb(&now)))),
                    b: Box::new(lsb(&then)),
                },
            })
        }
        "$countones" | "$onehot" | "$onehot0" => {
            let [a] = args.as_slice() else {
                return Err(format!("{sub} with {} arguments", args.len()));
            };
            let x = build_expr(a, cx)?;
            let xw = x.width();
            if xw > 64 {
                return Err(format!("{sub} over {xw} bits (the cap is 64)"));
            }
            if sub == "$countones" {
                let bit = |i: u32| {
                    resize(
                        ExprIr::Slice {
                            a: Box::new(x.clone()),
                            hi: i,
                            lo: i,
                        },
                        w,
                        false,
                    )
                };
                let mut sum = bit(0);
                for i in 1..xw {
                    sum = ExprIr::Bin {
                        op: BinOp::Add,
                        a: Box::new(sum),
                        b: Box::new(bit(i)),
                    };
                }
                return Ok(sum);
            }
            // at most one bit set: x & (x - 1) == 0
            let at_most_one = ExprIr::Cmp {
                op: CmpOp::Eq,
                signed: false,
                a: Box::new(ExprIr::Bin {
                    op: BinOp::And,
                    a: Box::new(x.clone()),
                    b: Box::new(ExprIr::Bin {
                        op: BinOp::Sub,
                        a: Box::new(x.clone()),
                        b: Box::new(konst(1, xw)),
                    }),
                }),
                b: Box::new(konst(0, xw)),
            };
            let e = if sub == "$onehot0" {
                at_most_one
            } else {
                ExprIr::Bin {
                    op: BinOp::And,
                    a: Box::new(at_most_one),
                    b: Box::new(truth(x)),
                }
            };
            Ok(resize(e, w, false))
        }
        s if !s.starts_with('$') => inline_call(node, s, args, w, cx),
        other => Err(format!(
            "system call `{other}` is not supported in an expression"
        )),
    }
}

/// Inline a user function whose body is exactly one `return <expr>;`.
fn inline_call(
    node: &Value,
    sub: &str,
    args: &[Value],
    w: u32,
    cx: &mut BuildCtx,
) -> Result<ExprIr, String> {
    let addr = sub.split(' ').next().unwrap_or("");
    let name = sub.rsplit(' ').next().unwrap_or(sub);
    let def = cx
        .subroutines
        .get(addr)
        .cloned()
        .ok_or_else(|| format!("call to `{name}`, whose definition is not in the AST"))?;
    if cx.inline_depth >= 8 {
        return Err(format!("inlining `{name}` exceeds depth 8 (recursion?)"));
    }
    if def.get("subroutineKind").and_then(Value::as_str) != Some("Function") {
        return Err(format!("`{name}` is a task, not a function"));
    }
    let body = def
        .get("body")
        .ok_or_else(|| format!("`{name}` has no body"))?;
    let ret = match kind(body) {
        "Return" => body.get("expr"),
        "Block" | "List" => None,
        _ => None,
    }
    .ok_or_else(|| {
        format!("`{name}`'s body is not a single `return <expr>;`, so it cannot be inlined")
    })?;
    let formals = def
        .get("arguments")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{name}` without an argument list"))?;
    if formals.len() != args.len() {
        return Err(format!(
            "`{name}` takes {} arguments, the call passes {}",
            formals.len(),
            args.len()
        ));
    }
    let mut bindings = HashMap::new();
    for (f, a) in formals.iter().zip(args) {
        if f.get("direction").and_then(Value::as_str) != Some("In") {
            return Err(format!("`{name}` has a non-input argument"));
        }
        let faddr = f
            .get("addr")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("`{name}`'s formal has no address"))?
            .to_string();
        // Argument passing is assignment-like (SV 13.5): the actual keeps ITS signedness when it
        // is widened to the formal's width, so a `Conversion` to the formal type extends by the
        // operand's signedness, not the target's.
        let actual = match (kind(a), a.get("operand")) {
            ("Conversion", Some(op)) if a.get("constant").is_none() => {
                let inner = build_expr(op, cx)?;
                resize(inner, node_width(a)?, typed_signed(op))
            }
            _ => build_expr(a, cx)?,
        };
        let fw = node_width(f)?;
        same_width(name, fw, &[&actual])?;
        bindings.insert(faddr, actual);
    }
    let saved_bindings = std::mem::replace(&mut cx.bindings, bindings);
    let saved_past = std::mem::replace(&mut cx.past, 0);
    cx.inline_depth += 1;
    let e = build_expr(ret, cx);
    cx.inline_depth -= 1;
    cx.past = saved_past;
    cx.bindings = saved_bindings;
    let e = e?;
    same_width(name, w, &[&e])?;
    let _ = node;
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_parse_at_their_width_including_negative_signed() {
        assert_eq!(parse_const("4'b1111", 4), Some(15));
        assert_eq!(parse_const("6'sd15", 6), Some(15));
        assert_eq!(parse_const("-4'sd8", 4), Some(8)); // -8 in 4 bits is 1000
        assert_eq!(parse_const("-1", 8), Some(255));
        assert_eq!(parse_const("32'd1", 32), Some(1));
        assert_eq!(parse_const("1'bx", 1), None);
        assert_eq!(parse_const("4'hz", 4), None);
        assert_eq!(parse_const("0", 129), None);
    }
}
