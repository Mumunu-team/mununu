//! The minted-expression IR and its BTOR2 emission (OpenTitan coverage, Phase 2).
//!
//! > Source of truth: [`augment_with_expressions`] + [`ExprIr::minted_name`] — surface: (CLI+API),
//! > through `sv verify-auto`.
//!
//! An [`ExprIr`] is a width-checked combinational expression over named model signals; the SVA
//! front end builds one per expression operand it cannot express as an atom
//! (`adapter::slang::expr_ir::build_expr`). [`augment_with_expressions`] appends it to a BTOR2
//! model as ordinary operator lines, the root named [`ExprIr::minted_name`], so the formula reads
//! it as one atom. Exact: combinational arithmetic, no state, no abstraction. A leaf that does not
//! resolve to a signal of its width leaves the expression unminted — its atom stays unbound and
//! the property is skipped with that reason, never mis-decided.

use std::collections::HashMap;

use crate::adapter::btor2::ast::{Nid, Node};
use crate::adapter::btor2::parser;
use crate::adapter::{AdapterError, AdapterErrorKind};

/// Bitwise / arithmetic binary operators (both operands and the result share one width).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    And,
    Or,
    Xor,
    Xnor,
    /// Shift left; the amount is the second operand, already at the value's width.
    Shl,
    /// Logical shift right.
    Lshr,
    /// Arithmetic shift right.
    Ashr,
}

/// Comparison operators; the result is 1 bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Reductions to 1 bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RedOp {
    And,
    Or,
    Xor,
}

/// A width-checked combinational expression over named model signals.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExprIr {
    /// A named signal of the lifted model (a state, input, output, named net or `__past` shadow),
    /// read at its full width.
    Leaf {
        name: String,
        width: u32,
    },
    /// A constant; `width ≤ 128`.
    Const {
        bits: u128,
        width: u32,
    },
    /// Bitwise complement.
    Not(Box<ExprIr>),
    /// Two's-complement negation.
    Neg(Box<ExprIr>),
    Bin {
        op: BinOp,
        a: Box<ExprIr>,
        b: Box<ExprIr>,
    },
    Cmp {
        op: CmpOp,
        signed: bool,
        a: Box<ExprIr>,
        b: Box<ExprIr>,
    },
    /// `c ? t : e` with a 1-bit `c`.
    Ite {
        c: Box<ExprIr>,
        t: Box<ExprIr>,
        e: Box<ExprIr>,
    },
    /// Bits `hi..=lo` (BTOR2 positions).
    Slice {
        a: Box<ExprIr>,
        hi: u32,
        lo: u32,
    },
    /// Zero- or sign-extension by `by` bits.
    Ext {
        signed: bool,
        a: Box<ExprIr>,
        by: u32,
    },
    /// `{hi, lo}`.
    Concat {
        hi: Box<ExprIr>,
        lo: Box<ExprIr>,
    },
    Red {
        op: RedOp,
        a: Box<ExprIr>,
    },
}

impl ExprIr {
    /// Result width in bits.
    pub fn width(&self) -> u32 {
        match self {
            ExprIr::Leaf { width, .. } | ExprIr::Const { width, .. } => *width,
            ExprIr::Not(a) | ExprIr::Neg(a) => a.width(),
            ExprIr::Bin { a, .. } => a.width(),
            ExprIr::Cmp { .. } | ExprIr::Red { .. } => 1,
            ExprIr::Ite { t, .. } => t.width(),
            ExprIr::Slice { hi, lo, .. } => hi - lo + 1,
            ExprIr::Ext { a, by, .. } => a.width() + by,
            ExprIr::Concat { hi, lo } => hi.width() + lo.width(),
        }
    }

    /// A canonical s-expression; equal expressions print equal. The minted name hashes this.
    pub fn canonical(&self) -> String {
        match self {
            ExprIr::Leaf { name, width } => format!("(leaf {name} {width})"),
            ExprIr::Const { bits, width } => format!("(const {width} {bits})"),
            ExprIr::Not(a) => format!("(not {})", a.canonical()),
            ExprIr::Neg(a) => format!("(neg {})", a.canonical()),
            ExprIr::Bin { op, a, b } => {
                format!("({} {} {})", bin_keyword(*op), a.canonical(), b.canonical())
            }
            ExprIr::Cmp { op, signed, a, b } => format!(
                "({} {} {})",
                cmp_keyword(*op, *signed),
                a.canonical(),
                b.canonical()
            ),
            ExprIr::Ite { c, t, e } => {
                format!(
                    "(ite {} {} {})",
                    c.canonical(),
                    t.canonical(),
                    e.canonical()
                )
            }
            ExprIr::Slice { a, hi, lo } => format!("(slice {} {hi} {lo})", a.canonical()),
            ExprIr::Ext { signed, a, by } => format!(
                "({} {} {by})",
                if *signed { "sext" } else { "uext" },
                a.canonical()
            ),
            ExprIr::Concat { hi, lo } => format!("(concat {} {})", hi.canonical(), lo.canonical()),
            ExprIr::Red { op, a } => format!("({} {})", red_keyword(*op), a.canonical()),
        }
    }

    /// `mununu_x_<FNV-1a-64 of the canonical form, 16 hex digits>` — stable across Rust versions
    /// and runs (no `std` hasher), lexable by the mu-calculus parser (no `[`, `.` or `-`).
    pub fn minted_name(&self) -> String {
        format!("mununu_x_{:016x}", fnv1a64(self.canonical().as_bytes()))
    }

    /// Every leaf name, in first-visit order, deduplicated.
    pub fn leaves(&self) -> Vec<(String, u32)> {
        fn walk(e: &ExprIr, out: &mut Vec<(String, u32)>) {
            match e {
                ExprIr::Leaf { name, width } => {
                    if !out.iter().any(|(n, _)| n == name) {
                        out.push((name.clone(), *width));
                    }
                }
                ExprIr::Const { .. } => {}
                ExprIr::Not(a)
                | ExprIr::Neg(a)
                | ExprIr::Slice { a, .. }
                | ExprIr::Ext { a, .. }
                | ExprIr::Red { a, .. } => walk(a, out),
                ExprIr::Bin { a, b, .. } | ExprIr::Cmp { a, b, .. } => {
                    walk(a, out);
                    walk(b, out);
                }
                ExprIr::Ite { c, t, e } => {
                    walk(c, out);
                    walk(t, out);
                    walk(e, out);
                }
                ExprIr::Concat { hi, lo } => {
                    walk(hi, out);
                    walk(lo, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self, &mut out);
        out
    }

    /// Reference evaluator over `≤ 128`-bit values (the tests' oracle for the BTOR2 emission).
    /// `env` gives each leaf's value; the result is masked to [`ExprIr::width`].
    pub fn eval(&self, env: &dyn Fn(&str) -> u128) -> u128 {
        let w = self.width();
        let v = match self {
            ExprIr::Leaf { name, .. } => env(name),
            ExprIr::Const { bits, .. } => *bits,
            ExprIr::Not(a) => !a.eval(env),
            ExprIr::Neg(a) => a.eval(env).wrapping_neg(),
            ExprIr::Bin { op, a, b } => {
                let (x, y) = (a.eval(env), b.eval(env));
                match op {
                    BinOp::Add => x.wrapping_add(y),
                    BinOp::Sub => x.wrapping_sub(y),
                    BinOp::Mul => x.wrapping_mul(y),
                    BinOp::And => x & y,
                    BinOp::Or => x | y,
                    BinOp::Xor => x ^ y,
                    BinOp::Xnor => !(x ^ y),
                    BinOp::Shl => {
                        if y >= u128::from(w) {
                            0
                        } else {
                            x << y
                        }
                    }
                    BinOp::Lshr => {
                        if y >= u128::from(w) {
                            0
                        } else {
                            x >> y
                        }
                    }
                    BinOp::Ashr => {
                        let s = sign_extend(x, w);
                        let amt = y.min(u128::from(w) - 1) as u32;
                        (s >> amt) as u128
                    }
                }
            }
            ExprIr::Cmp { op, signed, a, b } => {
                let aw = a.width();
                let (x, y) = (a.eval(env), b.eval(env));
                let ord = if *signed {
                    sign_extend(x, aw).cmp(&sign_extend(y, aw))
                } else {
                    x.cmp(&y)
                };
                u128::from(match op {
                    CmpOp::Eq => ord.is_eq(),
                    CmpOp::Ne => !ord.is_eq(),
                    CmpOp::Lt => ord.is_lt(),
                    CmpOp::Le => ord.is_le(),
                    CmpOp::Gt => ord.is_gt(),
                    CmpOp::Ge => ord.is_ge(),
                })
            }
            ExprIr::Ite { c, t, e } => {
                if c.eval(env) & 1 == 1 {
                    t.eval(env)
                } else {
                    e.eval(env)
                }
            }
            ExprIr::Slice { a, lo, .. } => a.eval(env) >> lo,
            ExprIr::Ext { signed, a, .. } => {
                if *signed {
                    sign_extend(a.eval(env), a.width()) as u128
                } else {
                    a.eval(env)
                }
            }
            ExprIr::Concat { hi, lo } => {
                let lw = lo.width();
                (hi.eval(env) << lw) | lo.eval(env)
            }
            ExprIr::Red { op, a } => {
                let aw = a.width();
                let x = a.eval(env) & mask(aw);
                u128::from(match op {
                    RedOp::And => x == mask(aw),
                    RedOp::Or => x != 0,
                    RedOp::Xor => x.count_ones() % 2 == 1,
                })
            }
        };
        v & mask(w)
    }
}

/// The BTOR2 keyword of a binary operator (`Ashr` → `sra`).
pub fn bin_keyword(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::Xor => "xor",
        BinOp::Xnor => "xnor",
        BinOp::Shl => "sll",
        BinOp::Lshr => "srl",
        BinOp::Ashr => "sra",
    }
}

/// The BTOR2 keyword of a comparison.
pub fn cmp_keyword(op: CmpOp, signed: bool) -> &'static str {
    match (op, signed) {
        (CmpOp::Eq, _) => "eq",
        (CmpOp::Ne, _) => "neq",
        (CmpOp::Lt, false) => "ult",
        (CmpOp::Le, false) => "ulte",
        (CmpOp::Gt, false) => "ugt",
        (CmpOp::Ge, false) => "ugte",
        (CmpOp::Lt, true) => "slt",
        (CmpOp::Le, true) => "slte",
        (CmpOp::Gt, true) => "sgt",
        (CmpOp::Ge, true) => "sgte",
    }
}

/// The BTOR2 keyword of a reduction.
pub fn red_keyword(op: RedOp) -> &'static str {
    match op {
        RedOp::And => "redand",
        RedOp::Or => "redor",
        RedOp::Xor => "redxor",
    }
}

/// All-ones at `w` bits (`w ≥ 128` → `u128::MAX`).
pub fn mask(w: u32) -> u128 {
    if w >= 128 {
        u128::MAX
    } else {
        (1u128 << w) - 1
    }
}

fn sign_extend(x: u128, w: u32) -> i128 {
    if w == 0 || w >= 128 {
        return x as i128;
    }
    let shift = 128 - w;
    ((x << shift) as i128) >> shift
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Append each `(name, expr)` to the BTOR2 `content` as a named combinational node. `name` must be
/// `expr.minted_name()` (checked — the translator and the model agree on names by construction).
/// Leaves resolve like a minted slice's base: a state, input or named net by symbol, or an output
/// port's driver; a leaf that does not resolve, or resolves at another width, skips that
/// expression. Idempotent: a name already in the model is not re-minted.
pub fn augment_with_expressions(
    content: &str,
    exprs: &[(&str, &ExprIr)],
) -> Result<String, AdapterError> {
    if exprs.is_empty() {
        return Ok(content.to_string());
    }
    let file = parser::parse(content).map_err(|mut e| {
        e.message = format!("adapter/btor2/minted_expr: {}", e.message);
        e
    })?;
    let symbols = parser::collect_symbols(&file);
    let mut by_symbol: HashMap<&str, Nid> =
        symbols.iter().map(|(nid, s)| (s.as_str(), *nid)).collect();
    for l in &file.lines {
        if let Node::Op {
            symbol: Some(sym), ..
        } = &l.node
        {
            by_symbol.entry(sym.as_str()).or_insert(l.nid);
        }
    }
    let mut sort_of_width: HashMap<u32, Nid> = file
        .lines
        .iter()
        .filter_map(|l| match &l.node {
            Node::Sort {
                sort: crate::adapter::btor2::ast::Sort::BitVec { width },
            } => Some((*width, l.nid)),
            _ => None,
        })
        .collect();
    let width_of = |nid: Nid| -> Option<u32> {
        let line = file.lines.iter().find(|l| l.nid == nid)?;
        let sort = match &line.node {
            Node::Input { sort, .. }
            | Node::State { sort, .. }
            | Node::Const { sort, .. }
            | Node::Op { sort, .. } => *sort,
            _ => return None,
        };
        parser::bv_width(&file, sort)
    };
    let mut em = Emitter {
        next_nid: file.lines.iter().map(|l| l.nid).max().unwrap_or(0) + 1,
        lines: Vec::new(),
        sort_of_width: &mut sort_of_width,
        memo: HashMap::new(),
    };
    let mut done = std::collections::HashSet::new();
    for &(name, expr) in exprs {
        if expr.minted_name() != name {
            return Err(AdapterError {
                kind: AdapterErrorKind::ParseError,
                message: format!(
                    "adapter/btor2/minted_expr: `{name}` is not the minted name of its expression \
                     (`{}`) — the translator and the model disagree",
                    expr.minted_name()
                ),
                location: None,
            });
        }
        if !done.insert(name) || by_symbol.contains_key(name) {
            continue;
        }
        let mut leaves = HashMap::new();
        let mut resolved = true;
        for (leaf, w) in expr.leaves() {
            let nid = by_symbol.get(leaf.as_str()).copied().or_else(|| {
                crate::adapter::btor2::bad_monitor::output_nid_and_sort(&file, &leaf)
                    .map(|(n, _)| n)
            });
            match nid {
                Some(n) if width_of(n) == Some(w) => {
                    leaves.insert(leaf, n);
                }
                _ => {
                    resolved = false;
                    break;
                }
            }
        }
        if !resolved {
            continue;
        }
        let checkpoint = (em.next_nid, em.lines.len(), em.memo.clone());
        match em.emit(expr, &leaves) {
            Some(root) => {
                let sort = em.sort(expr.width());
                let n = em.fresh();
                em.lines.push(format!("{n} uext {sort} {root} 0 {name}"));
            }
            None => {
                // Unreachable for a well-formed IR; roll back rather than leave half an expression.
                em.next_nid = checkpoint.0;
                em.lines.truncate(checkpoint.1);
                em.memo = checkpoint.2;
            }
        }
    }
    if em.lines.is_empty() {
        return Ok(content.to_string());
    }
    let mut out = content.trim_end().to_string();
    out.push('\n');
    for line in &em.lines {
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

struct Emitter<'a> {
    next_nid: Nid,
    lines: Vec<String>,
    sort_of_width: &'a mut HashMap<u32, Nid>,
    /// Canonical form → emitted nid, shared across expressions (common sub-expressions once).
    memo: HashMap<String, Nid>,
}

impl Emitter<'_> {
    fn fresh(&mut self) -> Nid {
        let n = self.next_nid;
        self.next_nid += 1;
        n
    }

    fn sort(&mut self, width: u32) -> Nid {
        if let Some(&s) = self.sort_of_width.get(&width) {
            return s;
        }
        let n = self.fresh();
        self.lines.push(format!("{n} sort bitvec {width}"));
        self.sort_of_width.insert(width, n);
        n
    }

    fn emit(&mut self, e: &ExprIr, leaves: &HashMap<String, Nid>) -> Option<Nid> {
        if let ExprIr::Leaf { name, .. } = e {
            return leaves.get(name).copied();
        }
        let key = e.canonical();
        if let Some(&n) = self.memo.get(&key) {
            return Some(n);
        }
        let w = e.width();
        let line_body = match e {
            ExprIr::Leaf { .. } => unreachable!("handled above"),
            ExprIr::Const { bits, width } => {
                let s = self.sort(*width);
                let bin: String = (0..*width)
                    .rev()
                    .map(|i| if (bits >> i) & 1 == 1 { '1' } else { '0' })
                    .collect();
                format!("const {s} {bin}")
            }
            ExprIr::Not(a) => {
                let a = self.emit(a, leaves)?;
                format!("not {} {a}", self.sort(w))
            }
            ExprIr::Neg(a) => {
                let a = self.emit(a, leaves)?;
                format!("neg {} {a}", self.sort(w))
            }
            ExprIr::Bin { op, a, b } => {
                let (a, b) = (self.emit(a, leaves)?, self.emit(b, leaves)?);
                format!("{} {} {a} {b}", bin_keyword(*op), self.sort(w))
            }
            ExprIr::Cmp { op, signed, a, b } => {
                let (a, b) = (self.emit(a, leaves)?, self.emit(b, leaves)?);
                format!("{} {} {a} {b}", cmp_keyword(*op, *signed), self.sort(1))
            }
            ExprIr::Ite { c, t, e: el } => {
                let (c, t, el) = (
                    self.emit(c, leaves)?,
                    self.emit(t, leaves)?,
                    self.emit(el, leaves)?,
                );
                format!("ite {} {c} {t} {el}", self.sort(w))
            }
            ExprIr::Slice { a, hi, lo } => {
                let a = self.emit(a, leaves)?;
                format!("slice {} {a} {hi} {lo}", self.sort(w))
            }
            ExprIr::Ext { signed, a, by } => {
                let a = self.emit(a, leaves)?;
                let kw = if *signed { "sext" } else { "uext" };
                format!("{kw} {} {a} {by}", self.sort(w))
            }
            ExprIr::Concat { hi, lo } => {
                let (hi, lo) = (self.emit(hi, leaves)?, self.emit(lo, leaves)?);
                format!("concat {} {hi} {lo}", self.sort(w))
            }
            ExprIr::Red { op, a } => {
                let a = self.emit(a, leaves)?;
                format!("{} {} {a}", red_keyword(*op), self.sort(1))
            }
        };
        let n = self.fresh();
        self.lines.push(format!("{n} {line_body}"));
        self.memo.insert(key, n);
        Some(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(n: &str, w: u32) -> ExprIr {
        ExprIr::Leaf {
            name: n.into(),
            width: w,
        }
    }

    #[test]
    fn the_minted_name_is_stable_and_lexable() {
        let e = ExprIr::Bin {
            op: BinOp::Add,
            a: Box::new(leaf("a", 4)),
            b: Box::new(ExprIr::Const { bits: 1, width: 4 }),
        };
        // Pinned: a change here renames every minted node and must be deliberate.
        assert_eq!(e.canonical(), "(add (leaf a 4) (const 4 1))");
        let n = e.minted_name();
        assert!(
            n.starts_with("mununu_x_") && n.len() == "mununu_x_".len() + 16,
            "{n}"
        );
        assert!(n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        assert_eq!(n, e.clone().minted_name());
    }

    #[test]
    fn the_reference_evaluator_follows_btor2_semantics() {
        let env = |n: &str| match n {
            "a" => 0b1010,
            "s" => 0b1000, // -8 as 4-bit signed
            _ => 0,
        };
        let sh = |op, amt| ExprIr::Bin {
            op,
            a: Box::new(leaf("s", 4)),
            b: Box::new(ExprIr::Const {
                bits: amt,
                width: 4,
            }),
        };
        assert_eq!(sh(BinOp::Shl, 1).eval(&env), 0);
        assert_eq!(sh(BinOp::Lshr, 3).eval(&env), 1);
        assert_eq!(sh(BinOp::Ashr, 2).eval(&env), 0b1110);
        assert_eq!(sh(BinOp::Ashr, 9).eval(&env), 0b1111);
        let lt = |signed| ExprIr::Cmp {
            op: CmpOp::Lt,
            signed,
            a: Box::new(leaf("s", 4)),
            b: Box::new(ExprIr::Const { bits: 0, width: 4 }),
        };
        assert_eq!(lt(true).eval(&env), 1);
        assert_eq!(lt(false).eval(&env), 0);
        let rx = ExprIr::Red {
            op: RedOp::Xor,
            a: Box::new(leaf("a", 4)),
        };
        assert_eq!(rx.eval(&env), 0);
        let ext = ExprIr::Ext {
            signed: true,
            a: Box::new(leaf("s", 4)),
            by: 4,
        };
        assert_eq!(ext.eval(&env), 0b1111_1000);
    }

    /// A deterministic generator of random well-formed IR over the leaves `a` (3 bits), `b`
    /// (3 bits) and `c` (2 bits) — every operator, every width rule the builder produces.
    struct Gen(u64);
    impl Gen {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn pick(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        /// An expression of exactly `w` bits, `depth` levels at most.
        fn expr(&mut self, w: u32, depth: u32) -> ExprIr {
            let fit = |e: ExprIr, w: u32, g: &mut Gen| -> ExprIr {
                let ew = e.width();
                if ew == w {
                    e
                } else if ew < w {
                    ExprIr::Ext {
                        signed: g.pick(2) == 0,
                        a: Box::new(e),
                        by: w - ew,
                    }
                } else {
                    let lo = g.pick(u64::from(ew - w + 1)) as u32;
                    ExprIr::Slice {
                        a: Box::new(e),
                        hi: lo + w - 1,
                        lo,
                    }
                }
            };
            if depth == 0 || self.pick(5) == 0 {
                return match self.pick(4) {
                    0 => ExprIr::Const {
                        bits: u128::from(self.next()) & mask(w),
                        width: w,
                    },
                    1 => fit(leaf("a", 3), w, self),
                    2 => fit(leaf("b", 3), w, self),
                    _ => fit(leaf("c", 2), w, self),
                };
            }
            let d = depth - 1;
            let bx = |e: ExprIr| Box::new(e);
            match self.pick(10) {
                0 => ExprIr::Not(bx(self.expr(w, d))),
                1 => ExprIr::Neg(bx(self.expr(w, d))),
                2 | 3 => {
                    let op = [
                        BinOp::Add,
                        BinOp::Sub,
                        BinOp::Mul,
                        BinOp::And,
                        BinOp::Or,
                        BinOp::Xor,
                        BinOp::Xnor,
                        BinOp::Shl,
                        BinOp::Lshr,
                        BinOp::Ashr,
                    ][self.pick(10) as usize];
                    ExprIr::Bin {
                        op,
                        a: bx(self.expr(w, d)),
                        b: bx(self.expr(w, d)),
                    }
                }
                4 if w == 1 => {
                    let ow = 1 + self.pick(4) as u32;
                    let op = [
                        CmpOp::Eq,
                        CmpOp::Ne,
                        CmpOp::Lt,
                        CmpOp::Le,
                        CmpOp::Gt,
                        CmpOp::Ge,
                    ][self.pick(6) as usize];
                    ExprIr::Cmp {
                        op,
                        signed: self.pick(2) == 0,
                        a: bx(self.expr(ow, d)),
                        b: bx(self.expr(ow, d)),
                    }
                }
                5 if w == 1 => {
                    let op = [RedOp::And, RedOp::Or, RedOp::Xor][self.pick(3) as usize];
                    let aw = 1 + self.pick(4) as u32;
                    ExprIr::Red {
                        op,
                        a: bx(self.expr(aw, d)),
                    }
                }
                6 => ExprIr::Ite {
                    c: bx(self.expr(1, d)),
                    t: bx(self.expr(w, d)),
                    e: bx(self.expr(w, d)),
                },
                7 if w >= 2 => {
                    let hw = 1 + self.pick(u64::from(w - 1)) as u32;
                    ExprIr::Concat {
                        hi: bx(self.expr(hw, d)),
                        lo: bx(self.expr(w - hw, d)),
                    }
                }
                _ => {
                    let iw = w + 1 + self.pick(2) as u32;
                    let inner = self.expr(iw, d);
                    fit(inner, w, self)
                }
            }
        }
    }

    /// The emitted BTOR2 computes what the IR means: random expressions, each minted into a model
    /// whose leaves are state cells, stepped once by the concrete BTOR2 simulator into a sink
    /// register, against [`ExprIr::eval`] on EVERY valuation of the 8 leaf bits.
    #[test]
    fn minted_btor2_agrees_with_the_reference_evaluator_on_every_valuation() {
        let mut g = Gen(0x5eed_0001);
        for case in 0..300 {
            let w = 1 + g.pick(4) as u32;
            let e = g.expr(w, 4);
            let name = e.minted_name();
            let base = format!(
                "1 sort bitvec 3\n2 sort bitvec 2\n3 sort bitvec {w}\n4 state 1 a\n5 state 1 b\n\
                 6 state 2 c\n7 state 3 sink\n"
            );
            let minted = augment_with_expressions(&base, &[(name.as_str(), &e)])
                .unwrap_or_else(|err| panic!("case {case}: {}", err.message));
            let root = minted
                .lines()
                .find(|l| l.ends_with(&format!(" {name}")))
                .and_then(|l| l.split_whitespace().next())
                .unwrap_or_else(|| panic!("case {case}: `{name}` not minted:\n{minted}"))
                .to_string();
            let model = format!("{minted}9999 next 3 7 {root}\n");
            let f = parser::parse(&model)
                .unwrap_or_else(|err| panic!("case {case}: {}\n{model}", err.message));
            for v in 0u128..256 {
                let (a, b, c) = (v & 7, (v >> 3) & 7, (v >> 6) & 3);
                let regs: HashMap<String, u128> = [
                    ("a".to_string(), a),
                    ("b".to_string(), b),
                    ("c".to_string(), c),
                    ("sink".to_string(), 0),
                ]
                .into_iter()
                .collect();
                let got =
                    crate::adapter::btor2::bit_blast::simulate_one_step(&f, &regs, &HashMap::new())
                        .expect("steps")["sink"];
                let want = e.eval(&|n| match n {
                    "a" => a,
                    "b" => b,
                    _ => c,
                });
                assert_eq!(
                    got,
                    want,
                    "case {case}, a={a} b={b} c={c}: {}",
                    e.canonical()
                );
            }
        }
    }

    #[test]
    fn an_unresolved_leaf_or_a_wrong_name_leaves_nothing_minted() {
        let base = "1 sort bitvec 3\n2 state 1 a\n";
        let e = ExprIr::Not(Box::new(leaf("ghost", 3)));
        assert_eq!(
            augment_with_expressions(base, &[(e.minted_name().as_str(), &e)]).unwrap(),
            base
        );
        let w = ExprIr::Not(Box::new(leaf("a", 4))); // `a` is 3 bits
        assert_eq!(
            augment_with_expressions(base, &[(w.minted_name().as_str(), &w)]).unwrap(),
            base
        );
        let ok = ExprIr::Not(Box::new(leaf("a", 3)));
        assert!(augment_with_expressions(base, &[("mununu_x_0", &ok)]).is_err());
        // Idempotent.
        let once = augment_with_expressions(base, &[(ok.minted_name().as_str(), &ok)]).unwrap();
        let twice = augment_with_expressions(&once, &[(ok.minted_name().as_str(), &ok)]).unwrap();
        assert_eq!(once, twice);
    }
}
