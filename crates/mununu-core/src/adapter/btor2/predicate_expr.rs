//! B.1 — compound predicate expressions for the predicate-cube path.
//!
//! Today a cube/SMT predicate is a single atom `register == value`
//! ([`crate::adapter::btor2::kmts_lift::PredicateSpec`]). This module adds
//! **compound predicates** — a boolean combination of register comparisons,
//! e.g. `idle = cnt == 0 && en == 1`, evaluated as ONE cube dimension. A
//! compound predicate does not change `|P|` (it is still one cube bit); only
//! the function that decides that bit's truth changes from an equality test to
//! a recursive boolean evaluation.
//!
//! # The soundness obligation (the §4 PO)
//!
//! There are two evaluators that MUST compute the **same boolean function**
//! over any concrete state, or the cube abstraction is unsound:
//!
//! - [`PredicateExpr::eval`] — explicit evaluation over a concrete register
//!   valuation (the sampling / target-cube-truth path in `kmts_lift`).
//! - [`PredicateExpr::build_constraint`] — the Z3 `Bool` over a BV view (the
//!   SMT may/must-edge path in `smt_must_edge`).
//!
//! The `predicate_expr_eval_matches_smt` differential test enumerates atoms,
//! operators, and assignments and asserts `eval(e, s) == sat(build_constraint(e)
//! under s)`. With that, the cube preservation theorem (PO-1: `may ⊇ concrete`,
//! `must ⊆ concrete`) transfers to compound predicates unchanged — the KMTS
//! machinery is agnostic to whether a predicate is atomic or compound.
//!
//! # Width / masking convention
//!
//! `build_constraint` masks the comparison value to the BV width (matching the
//! simple-atom [`crate::adapter::btor2::smt_must_edge`] path). `eval` compares
//! the register's concrete value (already width-bounded when it comes from the
//! simulator) against the value as-is. The two agree whenever the comparison
//! value fits the register width — the same implicit assumption the existing
//! simple-atom cube path already makes (`next_v == pred.value as u128`).

use std::collections::BTreeSet;

/// Comparison operator for a predicate atom. Unsigned bit-vector semantics
/// (the register is an unsigned bit-vector value), matching the BTOR2 / Z3 BV
/// `bvult`/`bvule`/`bvugt`/`bvuge` operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A compound predicate expression: a boolean combination of register
/// comparisons. The leaf is a single `register <op> value` comparison; the
/// internal nodes are `And` / `Or` / `Not`. No arithmetic — that is the
/// register's job, not the predicate's (keeps the predicate layer inside the
/// audited modal fragment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredicateExpr {
    /// `register <op> value` (unsigned comparison against a literal).
    Cmp {
        register: String,
        op: CmpOp,
        value: u64,
    },
    /// REL — `lhs <op> rhs` where **both** operands are registers (a
    /// *relational* predicate, e.g. `state_q == state_q__past` for `$stable`, or
    /// `data_o == data_i` for dataflow). Like [`PredicateExpr::Cmp`] it is one
    /// cube bit; the second register is referenced (not a cube dimension), and
    /// the SMT image picks a consistent value for it. Only the SMT path honours
    /// it (the literal-based sampler cannot realise a two-register relation —
    /// the same reason compound predicates are gated to `SmtAllPairs`).
    CmpReg {
        lhs: String,
        op: CmpOp,
        rhs: String,
    },
    /// H.G — arithmetic relational `lhs <op> (rhs + addend)` over two registers
    /// with a **constant** addend, in unsigned bit-vector arithmetic **mod
    /// 2^width** (the register width — so it wraps exactly as the RTL's `+` does).
    /// The one arithmetic form the translator needs (`cnt_q == $past(cnt_q) + 1`,
    /// sysrst sva_15 `CntIncr_A`). No general arithmetic — a single `+const`
    /// keeps the predicate layer inside the audited modal fragment.
    ///
    /// **SMT-primary.** Production routes this SMT-only (a derived relational
    /// label, like [`PredicateExpr::CmpReg`]-with-an-input): [`build_constraint`]
    /// does BV `bvadd` at the operand's real width (sound). [`eval`] is the
    /// differential-test reference and computes `mod 2^width` from the embedded
    /// `width`; a `width == 0` leaf must never be `eval`'d (production never does
    /// — it uses `build_constraint`). The `predicate_expr_eval_matches_smt`
    /// differential (the §4 PO) sets `width` explicitly and checks the two agree,
    /// including at the wraparound boundary.
    ///
    /// [`build_constraint`]: PredicateExpr::build_constraint
    /// [`eval`]: PredicateExpr::eval
    CmpRegAddend {
        lhs: String,
        op: CmpOp,
        rhs: String,
        addend: u64,
        /// Modulus width for `eval` (the register width). `0` ⇒ SMT-only
        /// (`build_constraint` uses the operand's real BV width); `eval` on a
        /// `width == 0` leaf is a caller error (production never evals it).
        width: u32,
    },
    /// SEL (P1-a, shot ①b) — an **array-content** predicate
    /// `array[index] <op> value`: a Z3 `select` over an array-sorted state cell,
    /// compared to a literal. `array` names an in-cone `$mem` state cell; `index`
    /// is a BV register whose value chooses the cell; `value` is the literal.
    ///
    /// **SMT-ONLY** (like [`PredicateExpr::CmpReg`] / [`PredicateExpr::CmpRegAddend`]):
    /// the concrete sampler tracks BV registers only, not array *contents*, so
    /// this leaf can never be [`eval`]'d — it is gated to `SmtAllPairs` via
    /// [`has_select`]. It is realised only by [`build_constraint_arr`], which
    /// emits `select(arr, idx) <op> value` over Z3's **exact** array theory
    /// (QF_AUFBV). Soundness therefore rests on the exactness of `select`, NOT on
    /// an eval/SMT agreement (there is no eval counterpart) — the array image is
    /// the precise must-side oracle (`btor2_encode` §Phase 10), so a definite
    /// verdict transfers.
    ///
    /// [`eval`]: PredicateExpr::eval
    /// [`has_select`]: PredicateExpr::has_select
    /// [`build_constraint_arr`]: PredicateExpr::build_constraint_arr
    Select {
        array: String,
        index: String,
        op: CmpOp,
        value: u64,
    },
    And(Box<PredicateExpr>, Box<PredicateExpr>),
    Or(Box<PredicateExpr>, Box<PredicateExpr>),
    Not(Box<PredicateExpr>),
}

impl PredicateExpr {
    /// A simple `register == value` atom — the shape every existing
    /// `PredicateSpec` lowers to. Lets the cube path treat a simple predicate
    /// as a trivial compound without special-casing.
    pub fn eq(register: impl Into<String>, value: u64) -> Self {
        PredicateExpr::Cmp {
            register: register.into(),
            op: CmpOp::Eq,
            value,
        }
    }

    /// A relational `lhs == rhs` atom over two registers (REL). The
    /// `$stable(x)` / `$changed(x)` translator forms emit `x <op> x__past`.
    pub fn eq_reg(lhs: impl Into<String>, rhs: impl Into<String>) -> Self {
        PredicateExpr::CmpReg {
            lhs: lhs.into(),
            op: CmpOp::Eq,
            rhs: rhs.into(),
        }
    }

    /// H.G — an arithmetic relational `lhs <op> (rhs + addend)` (`width == 0` ⇒
    /// SMT-only; `eval` requires a nonzero `width`). The `cnt == $past(cnt) + 1`
    /// (`CntIncr_A`) translator form.
    pub fn cmp_reg_addend(
        lhs: impl Into<String>,
        op: CmpOp,
        rhs: impl Into<String>,
        addend: u64,
        width: u32,
    ) -> Self {
        PredicateExpr::CmpRegAddend {
            lhs: lhs.into(),
            op,
            rhs: rhs.into(),
            addend,
            width,
        }
    }

    /// True if the expression contains an arithmetic-addend leaf
    /// ([`PredicateExpr::CmpRegAddend`]). The seeder routes such an expression
    /// SMT-only (a derived relational label), since `eval` on a `width == 0`
    /// production leaf is invalid.
    pub fn has_addend(&self) -> bool {
        match self {
            PredicateExpr::CmpRegAddend { .. } => true,
            PredicateExpr::Cmp { .. }
            | PredicateExpr::CmpReg { .. }
            | PredicateExpr::Select { .. } => false,
            PredicateExpr::And(a, b) | PredicateExpr::Or(a, b) => a.has_addend() || b.has_addend(),
            PredicateExpr::Not(a) => a.has_addend(),
        }
    }

    /// SEL — true if the expression contains an array-content leaf
    /// ([`PredicateExpr::Select`]). Like [`has_addend`], such an expression is
    /// routed **SMT-only** (`SmtAllPairs`): the literal-based sampler cannot read
    /// array content, so [`eval`] must never see it — only
    /// [`build_constraint_arr`] realises it, over Z3's exact array theory.
    ///
    /// [`has_addend`]: PredicateExpr::has_addend
    /// [`eval`]: PredicateExpr::eval
    /// [`build_constraint_arr`]: PredicateExpr::build_constraint_arr
    pub fn has_select(&self) -> bool {
        match self {
            PredicateExpr::Select { .. } => true,
            PredicateExpr::Cmp { .. }
            | PredicateExpr::CmpReg { .. }
            | PredicateExpr::CmpRegAddend { .. } => false,
            PredicateExpr::And(a, b) | PredicateExpr::Or(a, b) => a.has_select() || b.has_select(),
            PredicateExpr::Not(a) => a.has_select(),
        }
    }

    /// SEL — an array-content atom `array[index] <op> value`. SMT-only.
    pub fn select(
        array: impl Into<String>,
        index: impl Into<String>,
        op: CmpOp,
        value: u64,
    ) -> Self {
        PredicateExpr::Select {
            array: array.into(),
            index: index.into(),
            op,
            value,
        }
    }

    /// Explicit evaluation over a concrete register valuation. Registers absent
    /// from `regs` default to `0` — matching the cube lifter's `.unwrap_or(0)`
    /// convention for next-state registers that no transition wrote.
    pub fn eval(&self, regs: &std::collections::HashMap<String, u128>) -> bool {
        match self {
            PredicateExpr::Cmp {
                register,
                op,
                value,
            } => {
                let lhs = regs.get(register).copied().unwrap_or(0);
                cmp_apply(*op, lhs, *value as u128)
            }
            PredicateExpr::CmpReg { lhs, op, rhs } => {
                let l = regs.get(lhs).copied().unwrap_or(0);
                let r = regs.get(rhs).copied().unwrap_or(0);
                cmp_apply(*op, l, r)
            }
            PredicateExpr::CmpRegAddend {
                lhs,
                op,
                rhs,
                addend,
                width,
            } => {
                let l = regs.get(lhs).copied().unwrap_or(0);
                let r = regs.get(rhs).copied().unwrap_or(0);
                // `mod 2^width` to match the BV `bvadd` in `build_constraint`
                // (wraps exactly as the RTL). A `width == 0` leaf is SMT-only and
                // must not be `eval`'d — debug-assert, and fall back to unmasked
                // (correct away from the wrap boundary) in release.
                debug_assert!(
                    *width > 0,
                    "eval on a width==0 CmpRegAddend (SMT-only leaf) — production must use build_constraint"
                );
                let sum = r.wrapping_add(*addend as u128);
                let sum = if *width == 0 || *width >= 128 {
                    sum
                } else {
                    sum & ((1u128 << *width) - 1)
                };
                cmp_apply(*op, l, sum)
            }
            PredicateExpr::Select { .. } => {
                // SMT-only: the concrete sampler tracks BV registers, not array
                // contents. `has_select` gates a Select-bearing predicate to
                // `SmtAllPairs`, so this is never reached in production — a
                // debug-assert catches a mis-route; release returns `false`
                // (a conservative label, not relied upon).
                debug_assert!(
                    false,
                    "eval on a Select leaf (array-content, SMT-only) — production must use build_constraint_arr"
                );
                false
            }
            PredicateExpr::And(a, b) => a.eval(regs) && b.eval(regs),
            PredicateExpr::Or(a, b) => a.eval(regs) || b.eval(regs),
            PredicateExpr::Not(a) => !a.eval(regs),
        }
    }

    /// All distinct register names referenced by the expression, sorted. Used
    /// to resolve + width-check every register a compound predicate touches
    /// (the simple atom touches exactly one; a compound may touch several).
    pub fn registers(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        self.collect_registers(&mut set);
        set.into_iter().collect()
    }

    fn collect_registers(&self, out: &mut BTreeSet<String>) {
        match self {
            PredicateExpr::Cmp { register, .. } => {
                out.insert(register.clone());
            }
            PredicateExpr::CmpReg { lhs, rhs, .. }
            | PredicateExpr::CmpRegAddend { lhs, rhs, .. } => {
                out.insert(lhs.clone());
                out.insert(rhs.clone());
            }
            PredicateExpr::Select { index, .. } => {
                // The BV INDEX register (resolved via the BV lookup); the array
                // itself is resolved separately via the array lookup (see
                // `arrays()` / `build_constraint_arr`).
                out.insert(index.clone());
            }
            PredicateExpr::And(a, b) | PredicateExpr::Or(a, b) => {
                a.collect_registers(out);
                b.collect_registers(out);
            }
            PredicateExpr::Not(a) => a.collect_registers(out),
        }
    }

    /// SEL — all distinct ARRAY names referenced by [`PredicateExpr::Select`]
    /// leaves, sorted. The caller resolves each to an array-sorted state cell
    /// (via the encoded view's `state_curr_arr`) and confirms it is in-cone.
    pub fn arrays(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        self.collect_arrays(&mut set);
        set.into_iter().collect()
    }

    fn collect_arrays(&self, out: &mut BTreeSet<String>) {
        match self {
            PredicateExpr::Select { array, .. } => {
                out.insert(array.clone());
            }
            PredicateExpr::Cmp { .. }
            | PredicateExpr::CmpReg { .. }
            | PredicateExpr::CmpRegAddend { .. } => {}
            PredicateExpr::And(a, b) | PredicateExpr::Or(a, b) => {
                a.collect_arrays(out);
                b.collect_arrays(out);
            }
            PredicateExpr::Not(a) => a.collect_arrays(out),
        }
    }

    /// SMT encoding: a Z3 `Bool` over the register BVs supplied by `lookup`
    /// (the caller maps a register name to its `state_curr` or `state_next` BV
    /// from the encoded view). Returns `None` if any referenced register is
    /// absent from the view — the caller treats that exactly as it treats a
    /// missing simple-atom register (an `Unknown` must-edge verdict).
    ///
    /// **Caller must hold a [`z3::with_z3_config`] scope.**
    pub fn build_constraint<F>(&self, lookup: &F) -> Option<z3::ast::Bool>
    where
        F: Fn(&str) -> Option<z3::ast::BV>,
    {
        // No array lookup supplied — a `Select` (array-content) leaf therefore
        // resolves to `None`, which the caller treats as an `Unknown` must-edge
        // (the conservative direction). Array-aware callers use
        // [`build_constraint_arr`].
        self.build_constraint_arr(lookup, &|_: &str| None)
    }

    /// SEL — the array-aware SMT encoding. Identical to [`build_constraint`] for
    /// every BV leaf, and additionally realises [`PredicateExpr::Select`] as
    /// `select(arr_lookup(array), bv_lookup(index)) <op> value` over Z3's exact
    /// array theory (QF_AUFBV). `arr_lookup` maps an array-sorted state-cell name
    /// to its `state_curr`/`state_next` [`z3::ast::Array`] from the encoded view.
    /// Returns `None` (⇒ conservative `Unknown`) if any referenced register OR
    /// array is absent from the view, matching the missing-register behaviour of
    /// the simple-atom path.
    ///
    /// **Caller must hold a [`z3::with_z3_config`] scope.**
    ///
    /// [`build_constraint`]: PredicateExpr::build_constraint
    pub fn build_constraint_arr<F, G>(&self, bv_lookup: &F, arr_lookup: &G) -> Option<z3::ast::Bool>
    where
        F: Fn(&str) -> Option<z3::ast::BV>,
        G: Fn(&str) -> Option<z3::ast::Array>,
    {
        match self {
            PredicateExpr::Cmp {
                register,
                op,
                value,
            } => {
                let bv = bv_lookup(register)?;
                Some(cmp_constraint(&bv, *op, *value))
            }
            PredicateExpr::CmpReg { lhs, op, rhs } => {
                let lbv = bv_lookup(lhs)?;
                let rbv = bv_lookup(rhs)?;
                Some(cmp_constraint_bv(&lbv, *op, &rbv))
            }
            PredicateExpr::CmpRegAddend {
                lhs,
                op,
                rhs,
                addend,
                ..
            } => {
                // `lhs <op> (rhs + addend)` in BV — `bvadd` wraps at `rhs`'s real
                // width (the RTL semantics). `width` is the eval-side modulus
                // only; here the operand BVs carry the authoritative width.
                let lbv = bv_lookup(lhs)?;
                let rbv = bv_lookup(rhs)?;
                let w = rbv.get_size();
                let mask: u64 = if w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
                let addend_bv = z3::ast::BV::from_u64(*addend & mask, w);
                let sum = rbv.bvadd(&addend_bv);
                Some(cmp_constraint_bv(&lbv, *op, &sum))
            }
            PredicateExpr::Select {
                array,
                index,
                op,
                value,
            } => {
                // SEL — `select(arr, idx) <op> value`. Exact array read (mirrors
                // the encoder's `Op::Read`, `btor2_encode.rs:762`). `None` (⇒
                // conservative Unknown) if the array/index is absent from the view
                // or the element is not a bit-vector.
                let arr = arr_lookup(array)?;
                let idx = bv_lookup(index)?;
                let elem = arr.select(&idx).as_bv()?;
                Some(cmp_constraint(&elem, *op, *value))
            }
            PredicateExpr::And(a, b) => {
                let ca = a.build_constraint_arr(bv_lookup, arr_lookup)?;
                let cb = b.build_constraint_arr(bv_lookup, arr_lookup)?;
                Some(z3::ast::Bool::and(&[&ca, &cb]))
            }
            PredicateExpr::Or(a, b) => {
                let ca = a.build_constraint_arr(bv_lookup, arr_lookup)?;
                let cb = b.build_constraint_arr(bv_lookup, arr_lookup)?;
                Some(z3::ast::Bool::or(&[&ca, &cb]))
            }
            PredicateExpr::Not(a) => Some(a.build_constraint_arr(bv_lookup, arr_lookup)?.not()),
        }
    }
}

/// Apply a comparison operator to two concrete unsigned values (shared by the
/// `Cmp` literal leaf and the `CmpReg` relational leaf in [`PredicateExpr::eval`]).
fn cmp_apply(op: CmpOp, lhs: u128, rhs: u128) -> bool {
    match op {
        CmpOp::Eq => lhs == rhs,
        CmpOp::Ne => lhs != rhs,
        CmpOp::Lt => lhs < rhs,
        CmpOp::Le => lhs <= rhs,
        CmpOp::Gt => lhs > rhs,
        CmpOp::Ge => lhs >= rhs,
    }
}

/// Build the Z3 `Bool` for a relational `lhs <op> rhs` atom over two BVs (REL).
/// Zero-extends the narrower BV to the wider width first (unsigned semantics) so
/// registers of different widths compare correctly.
fn cmp_constraint_bv(lhs: &z3::ast::BV, op: CmpOp, rhs: &z3::ast::BV) -> z3::ast::Bool {
    let (l, r) = match_widths(lhs, rhs);
    match op {
        CmpOp::Eq => l.eq(&r),
        CmpOp::Ne => l.eq(&r).not(),
        CmpOp::Lt => l.bvult(&r),
        CmpOp::Le => l.bvule(&r),
        CmpOp::Gt => l.bvugt(&r),
        CmpOp::Ge => l.bvuge(&r),
    }
}

/// Zero-extend the narrower of two BVs so both share the wider width.
fn match_widths(a: &z3::ast::BV, b: &z3::ast::BV) -> (z3::ast::BV, z3::ast::BV) {
    let (wa, wb) = (a.get_size(), b.get_size());
    if wa < wb {
        (a.zero_ext(wb - wa), b.clone())
    } else if wb < wa {
        (a.clone(), b.zero_ext(wa - wb))
    } else {
        (a.clone(), b.clone())
    }
}

/// Build the Z3 `Bool` for one `register <op> value` atom over `bv`. Masks the
/// value to the BV width (matching
/// `smt_must_edge::build_predicate_constraint`).
fn cmp_constraint(bv: &z3::ast::BV, op: CmpOp, value: u64) -> z3::ast::Bool {
    let width = bv.get_size();
    let mask: u64 = if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    let val_bv = z3::ast::BV::from_u64(value & mask, width);
    match op {
        CmpOp::Eq => bv.eq(&val_bv),
        CmpOp::Ne => bv.eq(&val_bv).not(),
        CmpOp::Lt => bv.bvult(&val_bv),
        CmpOp::Le => bv.bvule(&val_bv),
        CmpOp::Gt => bv.bvugt(&val_bv),
        CmpOp::Ge => bv.bvuge(&val_bv),
    }
}

// --------------------------------------------------------------------------
// B.1 — sidecar expr-string parser
//
// The MVP sidecar carries a compound predicate as a human-authored string
// (`"cnt == 0 && en == 1"`), parsed here into a [`PredicateExpr`]. (The
// structured-JSON form is a Phase-FP option; the string is friendlier to
// author.) Hand-rolled tokenizer + recursive-descent parser — no external
// grammar dep, and the supported surface is deliberately small (the §6 scope
// discipline): comparison atoms + `&&` / `||` / `!` / parentheses, nothing
// else (no arithmetic — that is the register's job, not the predicate's).
// --------------------------------------------------------------------------

/// Error parsing a predicate-expression string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("predicate expression parse error: {message}")]
pub struct PredicateExprParseError {
    pub message: String,
}

fn perr(message: impl Into<String>) -> PredicateExprParseError {
    PredicateExprParseError {
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Int(u64),
    Op(CmpOp),
    And,
    Or,
    Not,
    /// H.G — `+` for the arithmetic-addend RHS (`rhs + const`), the sole
    /// arithmetic form (`cnt == $past(cnt) + 1`).
    Plus,
    LParen,
    RParen,
    /// SEL — `[` / `]` delimiting the index of an array-content atom
    /// `array[index] <op> value`.
    LBracket,
    RBracket,
}

fn tokenize(s: &str) -> Result<Vec<Token>, PredicateExprParseError> {
    let chars: Vec<char> = s.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            '&' if chars.get(i + 1) == Some(&'&') => {
                tokens.push(Token::And);
                i += 2;
            }
            '|' if chars.get(i + 1) == Some(&'|') => {
                tokens.push(Token::Or);
                i += 2;
            }
            '=' if chars.get(i + 1) == Some(&'=') => {
                tokens.push(Token::Op(CmpOp::Eq));
                i += 2;
            }
            '!' if chars.get(i + 1) == Some(&'=') => {
                tokens.push(Token::Op(CmpOp::Ne));
                i += 2;
            }
            '!' => {
                tokens.push(Token::Not);
                i += 1;
            }
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '<' if chars.get(i + 1) == Some(&'=') => {
                tokens.push(Token::Op(CmpOp::Le));
                i += 2;
            }
            '<' => {
                tokens.push(Token::Op(CmpOp::Lt));
                i += 1;
            }
            '>' if chars.get(i + 1) == Some(&'=') => {
                tokens.push(Token::Op(CmpOp::Ge));
                i += 2;
            }
            '>' => {
                tokens.push(Token::Op(CmpOp::Gt));
                i += 1;
            }
            '0'..='9' => {
                let start = i;
                // 0x-hex or decimal.
                if c == '0' && matches!(chars.get(i + 1), Some('x') | Some('X')) {
                    i += 2;
                    let hex_start = i;
                    while i < chars.len() && chars[i].is_ascii_hexdigit() {
                        i += 1;
                    }
                    let hex: String = chars[hex_start..i].iter().collect();
                    if hex.is_empty() {
                        return Err(perr("empty hex literal after `0x`"));
                    }
                    let v = u64::from_str_radix(&hex, 16)
                        .map_err(|e| perr(format!("bad hex literal `0x{hex}`: {e}")))?;
                    tokens.push(Token::Int(v));
                } else {
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    let dec: String = chars[start..i].iter().collect();
                    let v = dec
                        .parse::<u64>()
                        .map_err(|e| perr(format!("bad decimal literal `{dec}`: {e}")))?;
                    tokens.push(Token::Int(v));
                }
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric()
                        || chars[i] == '_'
                        || chars[i] == '$'
                        || chars[i] == '.')
                {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                tokens.push(Token::Ident(ident));
            }
            other => {
                return Err(perr(format!("unexpected character `{other}`")));
            }
        }
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_or(&mut self) -> Result<PredicateExpr, PredicateExprParseError> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.pos += 1;
            let rhs = self.parse_and()?;
            lhs = PredicateExpr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<PredicateExpr, PredicateExprParseError> {
        let mut lhs = self.parse_not()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.pos += 1;
            let rhs = self.parse_not()?;
            lhs = PredicateExpr::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> Result<PredicateExpr, PredicateExprParseError> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.pos += 1;
            let inner = self.parse_not()?;
            Ok(PredicateExpr::Not(Box::new(inner)))
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<PredicateExpr, PredicateExprParseError> {
        match self.peek() {
            Some(Token::LParen) => {
                self.pos += 1;
                let e = self.parse_or()?;
                match self.next() {
                    Some(Token::RParen) => Ok(e),
                    _ => Err(perr("expected `)`")),
                }
            }
            Some(Token::Ident(_)) => self.parse_atom(),
            other => Err(perr(format!(
                "expected an atom or `(`, found {:?}",
                other.cloned()
            ))),
        }
    }

    fn parse_atom(&mut self) -> Result<PredicateExpr, PredicateExprParseError> {
        let register = match self.next() {
            Some(Token::Ident(s)) => s,
            other => return Err(perr(format!("expected a register name, found {other:?}"))),
        };
        // SEL — `array[index] <op> value` (array-content atom). An `[` after the
        // first identifier means it names an array; parse the index register + `]`,
        // then a comparison against an integer literal → `PredicateExpr::Select`.
        if matches!(self.peek(), Some(Token::LBracket)) {
            self.pos += 1; // consume `[`
            let index = match self.next() {
                Some(Token::Ident(s)) => s,
                other => {
                    return Err(perr(format!(
                        "expected an index register in `{register}[…]`, found {other:?}"
                    )));
                }
            };
            match self.next() {
                Some(Token::RBracket) => {}
                other => {
                    return Err(perr(format!(
                        "expected `]` after `{register}[{index}`, found {other:?}"
                    )));
                }
            }
            let op = match self.next() {
                Some(Token::Op(o)) => o,
                other => {
                    return Err(perr(format!(
                        "expected a comparison operator after `{register}[{index}]`, found {other:?}"
                    )));
                }
            };
            let value = match self.next() {
                Some(Token::Int(v)) => v,
                other => {
                    return Err(perr(format!(
                        "expected an integer value after `{register}[{index}] <op>`, found {other:?}"
                    )));
                }
            };
            return Ok(PredicateExpr::Select {
                array: register,
                index,
                op,
                value,
            });
        }
        let op = match self.next() {
            Some(Token::Op(o)) => o,
            other => {
                return Err(perr(format!(
                    "expected a comparison operator after `{register}`, found {other:?}"
                )));
            }
        };
        // RHS is an integer literal (→ `Cmp`) or a register name (→ `CmpReg`,
        // the REL relational form, e.g. `state_q == state_q__past`).
        match self.next() {
            Some(Token::Int(value)) => Ok(PredicateExpr::Cmp {
                register,
                op,
                value,
            }),
            Some(Token::Ident(rhs)) => {
                // H.G — `rhs + const` → CmpRegAddend (`cnt == $past(cnt) + 1`);
                // bare `rhs` → CmpReg (REL). width=0 ⇒ SMT-only (resolved at
                // build_constraint from the operand BV; see CmpRegAddend docs).
                if matches!(self.peek(), Some(Token::Plus)) {
                    self.pos += 1; // consume `+`
                    match self.next() {
                        Some(Token::Int(addend)) => Ok(PredicateExpr::CmpRegAddend {
                            lhs: register,
                            op,
                            rhs,
                            addend,
                            width: 0,
                        }),
                        other => Err(perr(format!(
                            "expected an integer addend after `{register} <op> {rhs} +`, found {other:?}"
                        ))),
                    }
                } else {
                    Ok(PredicateExpr::CmpReg {
                        lhs: register,
                        op,
                        rhs,
                    })
                }
            }
            other => Err(perr(format!(
                "expected an integer value or a register after `{register} <op>`, found {other:?}"
            ))),
        }
    }
}

/// Parse a sidecar predicate-expression string into a [`PredicateExpr`].
///
/// Grammar (precedence low→high): `||`, `&&`, unary `!`, parentheses, then
/// comparison atoms `<register> <op> <rhs>` with `op ∈ { ==, !=, <, <=, >,
/// >= }`. Registers are identifiers (`[A-Za-z_][A-Za-z0-9_$.]*`; `$`/`.` are
/// allowed for BTOR2 hierarchical symbol names). The `<rhs>` is a decimal or
/// `0x`-hex integer (→ `Cmp`) **or another register** (→ `CmpReg`, the REL
/// relational form). `&&` binds tighter than `||`; `!` is unary prefix.
///
/// Examples: `cnt == 0 && en == 1`, `!(state == 5) || done >= 1`,
/// `state_q == state_q__past` (relational — REL).
pub fn parse_predicate_expr(s: &str) -> Result<PredicateExpr, PredicateExprParseError> {
    let tokens = tokenize(s)?;
    if tokens.is_empty() {
        return Err(perr("empty predicate expression"));
    }
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
    };
    let expr = parser.parse_or()?;
    if parser.pos != tokens.len() {
        return Err(perr(format!(
            "unexpected trailing tokens starting at {:?}",
            tokens.get(parser.pos)
        )));
    }
    Ok(expr)
}

/// Parse a predicate atom, tolerating a **bare boolean identifier** as `sig != 0`.
///
/// A single-identifier atom (`bnd_viol_o`, no comparison operator) is the
/// mu-calculus "signal is true" reading; `!= 0` — not `== 1` — is the SOUND
/// encoding (for a multi-bit signal `== 1` would spuriously exclude the truthy
/// values 2, 3, …). Everything else delegates to the strict
/// [`parse_predicate_expr`] grammar, so a malformed atom still errors.
///
/// This is a **caller-opt-in** relaxation. The verify_auto seeder deliberately
/// keeps the strict parser (its own `Err`-path routes bare atoms into the
/// combinational-input soundness machinery), so only callers that want the bare
/// boolean read directly — the exact-symbolic engine parsing mu-calculus atoms —
/// use this entry point.
pub fn parse_predicate_atom_bool(s: &str) -> Result<PredicateExpr, PredicateExprParseError> {
    let tokens = tokenize(s)?;
    if let [Token::Ident(name)] = tokens.as_slice() {
        return Ok(PredicateExpr::Cmp {
            register: name.clone(),
            op: CmpOp::Ne,
            value: 0,
        });
    }
    parse_predicate_expr(s)
}

#[cfg(test)]
mod tests {
    /// mununu#543 PROBE — at what depth does a `PredicateExpr` chain overflow the stack?
    ///
    /// Not a regression test: it deliberately ABORTS the process when the depth exceeds what the
    /// stack holds, which is the measurement. Driven one depth per process:
    ///
    /// ```bash
    /// MUNUNU_PROBE_DEPTH=20000 MUNUNU_PROBE_OP=eval \
    ///   cargo test -p mununu-core --lib -- --ignored probe_543_predicate_expr_depth --nocapture
    /// ```
    ///
    /// `MUNUNU_PROBE_OP` selects which walk to exercise: `build` (construction only), `eval`,
    /// `collect` (`collect_registers`), or `drop` (construct, then drop — the walk no depth bound
    /// can ever catch, because it runs while unwinding).
    ///
    /// Exit 0 = survived that depth. Abort / exit 134 = that depth overflows.
    #[test]
    #[ignore = "mununu#543 probe — aborts by design; set MUNUNU_PROBE_DEPTH (and MUNUNU_PROBE_OP)"]
    fn probe_543_predicate_expr_depth() {
        let depth: usize = std::env::var("MUNUNU_PROBE_DEPTH")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);
        let op = std::env::var("MUNUNU_PROBE_OP").unwrap_or_else(|_| "eval".to_string());

        // A LEFT-leaning chain, exactly the shape `cvc5::sexpr_to_predicate_expr`'s fold builds
        // from an interpolant's conjuncts: And(And(And(..), c), c).
        let leaf = |i: usize| PredicateExpr::Cmp {
            register: format!("r{i}"),
            op: CmpOp::Eq,
            value: i as u64,
        };
        let mut e = leaf(0);
        for i in 1..depth {
            e = PredicateExpr::And(Box::new(e), Box::new(leaf(i)));
        }
        eprintln!("[probe] built depth={depth} op={op}");

        match op.as_str() {
            "build" => {
                // Construction is iterative; this arm measures the DROP implicitly at scope end,
                // so `build` and `drop` differ only in whether a walk ran first.
            }
            "eval" => {
                let regs: std::collections::HashMap<String, u128> =
                    std::collections::HashMap::new();
                let v = e.eval(&regs);
                eprintln!("[probe] eval survived depth={depth} -> {v}");
            }
            "collect" => {
                let mut out = std::collections::BTreeSet::new();
                e.collect_registers(&mut out);
                eprintln!(
                    "[probe] collect survived depth={depth} -> {} regs",
                    out.len()
                );
            }
            "drop" => {}
            other => panic!("unknown MUNUNU_PROBE_OP {other}"),
        }
        drop(e);
        eprintln!("[probe] SURVIVED depth={depth} op={op} (including drop)");
    }

    use super::*;
    use std::collections::HashMap;

    fn regs(pairs: &[(&str, u128)]) -> HashMap<String, u128> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn eval_simple_eq_atom() {
        let e = PredicateExpr::eq("cnt", 0);
        assert!(e.eval(&regs(&[("cnt", 0)])));
        assert!(!e.eval(&regs(&[("cnt", 1)])));
        // Absent register defaults to 0.
        assert!(e.eval(&regs(&[])));
    }

    #[test]
    fn eval_compound_and_or_not() {
        // idle = cnt == 0 && en == 1
        let idle = PredicateExpr::And(
            Box::new(PredicateExpr::eq("cnt", 0)),
            Box::new(PredicateExpr::eq("en", 1)),
        );
        assert!(idle.eval(&regs(&[("cnt", 0), ("en", 1)])));
        assert!(!idle.eval(&regs(&[("cnt", 0), ("en", 0)])));
        assert!(!idle.eval(&regs(&[("cnt", 3), ("en", 1)])));

        // busy = !(cnt == 0) || en == 0
        let busy = PredicateExpr::Or(
            Box::new(PredicateExpr::Not(Box::new(PredicateExpr::eq("cnt", 0)))),
            Box::new(PredicateExpr::eq("en", 0)),
        );
        assert!(busy.eval(&regs(&[("cnt", 2), ("en", 1)])));
        assert!(busy.eval(&regs(&[("cnt", 0), ("en", 0)])));
        assert!(!busy.eval(&regs(&[("cnt", 0), ("en", 1)])));
    }

    #[test]
    fn eval_ordering_operators() {
        let lt = PredicateExpr::Cmp {
            register: "x".into(),
            op: CmpOp::Lt,
            value: 4,
        };
        assert!(lt.eval(&regs(&[("x", 3)])));
        assert!(!lt.eval(&regs(&[("x", 4)])));
        let ge = PredicateExpr::Cmp {
            register: "x".into(),
            op: CmpOp::Ge,
            value: 4,
        };
        assert!(ge.eval(&regs(&[("x", 4)])));
        assert!(!ge.eval(&regs(&[("x", 3)])));
    }

    #[test]
    fn parse_simple_atom() {
        assert_eq!(
            parse_predicate_expr("cnt == 0").unwrap(),
            PredicateExpr::eq("cnt", 0)
        );
        assert_eq!(
            parse_predicate_expr("state_q != 41").unwrap(),
            PredicateExpr::Cmp {
                register: "state_q".into(),
                op: CmpOp::Ne,
                value: 41
            }
        );
    }

    #[test]
    fn parse_all_comparison_ops_and_hex() {
        let cases = [
            ("a == 1", CmpOp::Eq),
            ("a != 1", CmpOp::Ne),
            ("a < 1", CmpOp::Lt),
            ("a <= 1", CmpOp::Le),
            ("a > 1", CmpOp::Gt),
            ("a >= 1", CmpOp::Ge),
        ];
        for (src, op) in cases {
            assert_eq!(
                parse_predicate_expr(src).unwrap(),
                PredicateExpr::Cmp {
                    register: "a".into(),
                    op,
                    value: 1
                },
                "parsing {src}"
            );
        }
        // 0x-hex value.
        assert_eq!(
            parse_predicate_expr("x == 0x1f").unwrap(),
            PredicateExpr::eq("x", 31)
        );
    }

    #[test]
    fn parse_compound_and_precedence() {
        // idle = cnt == 0 && en == 1
        assert_eq!(
            parse_predicate_expr("cnt == 0 && en == 1").unwrap(),
            PredicateExpr::And(
                Box::new(PredicateExpr::eq("cnt", 0)),
                Box::new(PredicateExpr::eq("en", 1)),
            )
        );
        // && binds tighter than ||: a==1 || b==2 && c==3  ==  Or(a, And(b, c))
        assert_eq!(
            parse_predicate_expr("a == 1 || b == 2 && c == 3").unwrap(),
            PredicateExpr::Or(
                Box::new(PredicateExpr::eq("a", 1)),
                Box::new(PredicateExpr::And(
                    Box::new(PredicateExpr::eq("b", 2)),
                    Box::new(PredicateExpr::eq("c", 3)),
                )),
            )
        );
        // Parens override precedence + unary !.
        assert_eq!(
            parse_predicate_expr("!(state == 5) || done >= 1").unwrap(),
            PredicateExpr::Or(
                Box::new(PredicateExpr::Not(Box::new(PredicateExpr::eq("state", 5)))),
                Box::new(PredicateExpr::Cmp {
                    register: "done".into(),
                    op: CmpOp::Ge,
                    value: 1
                }),
            )
        );
    }

    #[test]
    fn parse_round_trips_through_eval() {
        let e = parse_predicate_expr("cnt == 0 && en == 1").unwrap();
        assert!(e.eval(&regs(&[("cnt", 0), ("en", 1)])));
        assert!(!e.eval(&regs(&[("cnt", 1), ("en", 1)])));
    }

    // --- REL (relational predicates: register == register) ---------------

    #[test]
    fn parse_relational_atom_to_cmpreg() {
        // `state_q == state_q__past` ($stable) parses to a CmpReg, not a Cmp.
        assert_eq!(
            parse_predicate_expr("state_q == state_q__past").unwrap(),
            PredicateExpr::eq_reg("state_q", "state_q__past")
        );
        assert_eq!(
            parse_predicate_expr("data_o != data_i").unwrap(),
            PredicateExpr::CmpReg {
                lhs: "data_o".into(),
                op: CmpOp::Ne,
                rhs: "data_i".into(),
            }
        );
        // Literal RHS still parses to Cmp (no regression).
        assert_eq!(
            parse_predicate_expr("state_q == 5").unwrap(),
            PredicateExpr::eq("state_q", 5)
        );
    }

    #[test]
    fn parse_predicate_atom_bool_bare_is_ne_zero() {
        // A lone identifier atom (no comparison operator) is the mu-calculus
        // "signal is true" reading, normalised to `register != 0` — matching the
        // SVA translator's bool_expr. `!= 0` (not `== 1`) is the sound multi-bit form.
        assert_eq!(
            parse_predicate_atom_bool("bnd_viol_o").unwrap(),
            PredicateExpr::Cmp {
                register: "bnd_viol_o".into(),
                op: CmpOp::Ne,
                value: 0,
            }
        );
        // Hierarchical BTOR2 symbol names (`$`/`.`) work as bare booleans too.
        assert_eq!(
            parse_predicate_atom_bool("top.err_o").unwrap(),
            PredicateExpr::Cmp {
                register: "top.err_o".into(),
                op: CmpOp::Ne,
                value: 0,
            }
        );
        // The STRICT parser is unchanged — the seeder relies on a bare atom erroring
        // so it can route it through its own combinational-input machinery.
        assert!(parse_predicate_expr("bnd_viol_o").is_err());
    }

    #[test]
    fn parse_predicate_atom_bool_delegates_for_non_bare() {
        // A comparison atom delegates to the strict grammar (no change).
        assert_eq!(
            parse_predicate_atom_bool("cnt == 3").unwrap(),
            PredicateExpr::eq("cnt", 3)
        );
        assert_eq!(
            parse_predicate_atom_bool("state_q == state_q__past").unwrap(),
            PredicateExpr::eq_reg("state_q", "state_q__past")
        );
        // Only a LONE identifier is rescued — a bare operand inside a compound is
        // still an error (the exact engine receives pre-normalised compounds).
        assert!(parse_predicate_atom_bool("en && cnt == 3").is_err());
        // A malformed atom still errors.
        assert!(parse_predicate_atom_bool("cnt ==").is_err());
    }

    #[test]
    fn bare_boolean_atom_bool_evals_as_nonzero() {
        // `flag` (≡ flag != 0) is true iff the register is nonzero.
        let e = parse_predicate_atom_bool("flag").unwrap();
        assert!(e.eval(&regs(&[("flag", 1)])));
        assert!(e.eval(&regs(&[("flag", 7)])));
        assert!(!e.eval(&regs(&[("flag", 0)])));
    }

    #[test]
    fn eval_relational_atom() {
        let stable = PredicateExpr::eq_reg("x", "x_past");
        assert!(stable.eval(&regs(&[("x", 7), ("x_past", 7)]))); // $stable true
        assert!(!stable.eval(&regs(&[("x", 7), ("x_past", 6)]))); // changed
        // A relational atom inside a compound (the real $stable-under-disable shape).
        let e = parse_predicate_expr("rst == 0 && state == state_past").unwrap();
        assert!(e.eval(&regs(&[("rst", 0), ("state", 3), ("state_past", 3)])));
        assert!(!e.eval(&regs(&[("rst", 0), ("state", 3), ("state_past", 2)])));
        assert!(!e.eval(&regs(&[("rst", 1), ("state", 3), ("state_past", 3)])));
    }

    #[test]
    fn registers_collects_both_sides_of_a_relational_atom() {
        let e = parse_predicate_expr("a == b && c != a").unwrap();
        assert_eq!(
            e.registers(),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    // The §4 soundness obligation, extended to REL: eval ≡ SMT for register-vs-
    // register leaves (the ONLY new proof obligation relational predicates add).
    #[test]
    fn relational_eval_matches_smt() {
        let exprs: Vec<PredicateExpr> = vec![
            PredicateExpr::eq_reg("a", "b"),
            PredicateExpr::CmpReg {
                lhs: "a".into(),
                op: CmpOp::Ne,
                rhs: "b".into(),
            },
            PredicateExpr::CmpReg {
                lhs: "a".into(),
                op: CmpOp::Lt,
                rhs: "b".into(),
            },
            PredicateExpr::CmpReg {
                lhs: "a".into(),
                op: CmpOp::Ge,
                rhs: "b".into(),
            },
            // relational leaf combined with a literal leaf + boolean structure
            PredicateExpr::And(
                Box::new(PredicateExpr::eq("a", 0)),
                Box::new(PredicateExpr::eq_reg("a", "b")),
            ),
            PredicateExpr::Not(Box::new(PredicateExpr::eq_reg("a", "b"))),
        ];

        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            for e in &exprs {
                for a in 0u64..4 {
                    for b in 0u64..4 {
                        let want = e.eval(&regs(&[("a", a as u128), ("b", b as u128)]));
                        let a_bv = z3::ast::BV::new_const("a", 2);
                        let b_bv = z3::ast::BV::new_const("b", 2);
                        let lookup = |name: &str| -> Option<z3::ast::BV> {
                            match name {
                                "a" => Some(a_bv.clone()),
                                "b" => Some(b_bv.clone()),
                                _ => None,
                            }
                        };
                        let constraint = e.build_constraint(&lookup).expect("all regs present");
                        let solver = z3::Solver::new();
                        let a_val = z3::ast::BV::from_u64(a, 2);
                        let b_val = z3::ast::BV::from_u64(b, 2);
                        let a_pin = a_bv.eq(&a_val);
                        let b_pin = b_bv.eq(&b_val);
                        solver.assert(&a_pin);
                        solver.assert(&b_pin);
                        solver.assert(&constraint);
                        let got = matches!(solver.check(), z3::SatResult::Sat);
                        assert_eq!(
                            want, got,
                            "eval/SMT disagree for {e:?} at a={a}, b={b}: eval={want}, smt={got}"
                        );
                    }
                }
            }
        });
    }

    #[test]
    fn arithmetic_addend_eval_matches_smt() {
        // H.G — the §4 PO for the arithmetic leaf: `a <op> (b + k) mod 2^width`
        // must agree between `eval` (embedded width) and `build_constraint` (BV
        // `bvadd`) across ALL 2-bit assignments, INCLUDING the wraparound
        // boundary (width=2 → `b=3, +1` wraps to `0`). This is the guard that
        // makes the arithmetic predicate sound.
        const W: u32 = 2; // 2-bit → wraps at 4
        let exprs: Vec<PredicateExpr> = vec![
            PredicateExpr::cmp_reg_addend("a", CmpOp::Eq, "b", 1, W),
            PredicateExpr::cmp_reg_addend("a", CmpOp::Ne, "b", 1, W),
            PredicateExpr::cmp_reg_addend("a", CmpOp::Ge, "b", 1, W),
            PredicateExpr::cmp_reg_addend("a", CmpOp::Lt, "b", 2, W),
            PredicateExpr::And(
                Box::new(PredicateExpr::eq("a", 0)),
                Box::new(PredicateExpr::cmp_reg_addend("a", CmpOp::Eq, "b", 1, W)),
            ),
            PredicateExpr::Not(Box::new(PredicateExpr::cmp_reg_addend(
                "a",
                CmpOp::Eq,
                "b",
                1,
                W,
            ))),
        ];
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            for e in &exprs {
                for a in 0u64..4 {
                    for b in 0u64..4 {
                        let want = e.eval(&regs(&[("a", a as u128), ("b", b as u128)]));
                        let a_bv = z3::ast::BV::new_const("a", W);
                        let b_bv = z3::ast::BV::new_const("b", W);
                        let lookup = |name: &str| -> Option<z3::ast::BV> {
                            match name {
                                "a" => Some(a_bv.clone()),
                                "b" => Some(b_bv.clone()),
                                _ => None,
                            }
                        };
                        let constraint = e.build_constraint(&lookup).expect("all regs present");
                        let solver = z3::Solver::new();
                        let a_val = z3::ast::BV::from_u64(a, W);
                        let b_val = z3::ast::BV::from_u64(b, W);
                        let a_pin = a_bv.eq(&a_val);
                        let b_pin = b_bv.eq(&b_val);
                        solver.assert(&a_pin);
                        solver.assert(&b_pin);
                        solver.assert(&constraint);
                        let got = matches!(solver.check(), z3::SatResult::Sat);
                        assert_eq!(
                            want, got,
                            "eval/SMT disagree for {e:?} at a={a}, b={b}: eval={want}, smt={got}"
                        );
                    }
                }
            }
        });
    }

    #[test]
    fn parse_arithmetic_addend() {
        // H.G — `cnt == cnt_past + 1` parses to a `CmpRegAddend` (width=0, filled
        // by build_constraint from the operand BV in production).
        let e = parse_predicate_expr("cnt == cnt_past + 1").expect("parse arithmetic addend");
        assert_eq!(
            e,
            PredicateExpr::cmp_reg_addend("cnt", CmpOp::Eq, "cnt_past", 1, 0)
        );
        assert!(e.has_addend());
        // a bare relational is still CmpReg (no addend)
        let rel = parse_predicate_expr("cnt >= cnt_past").expect("parse rel");
        assert!(!rel.has_addend());
        assert_eq!(
            rel,
            PredicateExpr::CmpReg {
                lhs: "cnt".into(),
                op: CmpOp::Ge,
                rhs: "cnt_past".into(),
            }
        );
    }

    #[test]
    fn eval_addend_wraps_at_width() {
        // width=2: `3 + 1 = 0 (mod 4)`, so `cnt == cnt_past + 1` is TRUE at
        // cnt=0, cnt_past=3 (the wrap) — matching the RTL BV `+`.
        let e = PredicateExpr::cmp_reg_addend("cnt", CmpOp::Eq, "cnt_past", 1, 2);
        assert!(
            e.eval(&regs(&[("cnt", 0), ("cnt_past", 3)])),
            "3+1 wraps to 0 at width 2"
        );
        assert!(!e.eval(&regs(&[("cnt", 0), ("cnt_past", 2)])), "2+1=3 != 0");
        assert!(e.eval(&regs(&[("cnt", 3), ("cnt_past", 2)])), "2+1=3");
    }

    #[test]
    fn relational_eval_matches_smt_mismatched_widths() {
        // a is 2-bit, b is 4-bit → zero-extend a; eval (u128) must agree.
        let e = PredicateExpr::eq_reg("a", "b");
        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            for a in 0u64..4 {
                for b in 0u64..16 {
                    let want = e.eval(&regs(&[("a", a as u128), ("b", b as u128)]));
                    let a_bv = z3::ast::BV::new_const("a", 2);
                    let b_bv = z3::ast::BV::new_const("b", 4);
                    let lookup = |name: &str| -> Option<z3::ast::BV> {
                        match name {
                            "a" => Some(a_bv.clone()),
                            "b" => Some(b_bv.clone()),
                            _ => None,
                        }
                    };
                    let constraint = e.build_constraint(&lookup).expect("present");
                    let solver = z3::Solver::new();
                    let a_val = z3::ast::BV::from_u64(a, 2);
                    let b_val = z3::ast::BV::from_u64(b, 4);
                    let a_pin = a_bv.eq(&a_val);
                    let b_pin = b_bv.eq(&b_val);
                    solver.assert(&a_pin);
                    solver.assert(&b_pin);
                    solver.assert(&constraint);
                    let got = matches!(solver.check(), z3::SatResult::Sat);
                    assert_eq!(
                        want, got,
                        "width-mismatch eval/SMT disagree at a={a}, b={b}"
                    );
                }
            }
        });
    }

    #[test]
    fn parse_rejects_malformed() {
        // Missing value, missing op, trailing op, unclosed paren, empty,
        // bad char, arithmetic (out of scope).
        for bad in [
            "",
            "cnt ==",
            "cnt 0",
            "cnt == 0 &&",
            "(cnt == 0",
            "cnt == 0)",
            "cnt == ", // op then nothing
            "cnt + 1 == 0",
            "cnt == 0x",
        ] {
            assert!(
                parse_predicate_expr(bad).is_err(),
                "expected parse error for {bad:?}"
            );
        }
    }

    #[test]
    fn registers_collects_all_referenced_sorted() {
        let e = PredicateExpr::And(
            Box::new(PredicateExpr::Or(
                Box::new(PredicateExpr::eq("b", 1)),
                Box::new(PredicateExpr::eq("a", 0)),
            )),
            Box::new(PredicateExpr::Not(Box::new(PredicateExpr::eq("a", 2)))),
        );
        assert_eq!(e.registers(), vec!["a".to_string(), "b".to_string()]);
    }

    // The §4 soundness obligation: the explicit evaluator and the SMT
    // encoding compute the SAME boolean function over every assignment. If
    // they ever diverge, compound predicates are unsound on the cube path.
    #[test]
    fn predicate_expr_eval_matches_smt() {
        // Two 2-bit registers a, b ranging 0..=3. A spread of expressions
        // covering every operator + And/Or/Not nesting.
        let exprs: Vec<PredicateExpr> = vec![
            PredicateExpr::eq("a", 0),
            PredicateExpr::Cmp {
                register: "a".into(),
                op: CmpOp::Ne,
                value: 2,
            },
            PredicateExpr::Cmp {
                register: "a".into(),
                op: CmpOp::Lt,
                value: 2,
            },
            PredicateExpr::Cmp {
                register: "b".into(),
                op: CmpOp::Ge,
                value: 1,
            },
            PredicateExpr::And(
                Box::new(PredicateExpr::eq("a", 0)),
                Box::new(PredicateExpr::Cmp {
                    register: "b".into(),
                    op: CmpOp::Gt,
                    value: 1,
                }),
            ),
            PredicateExpr::Or(
                Box::new(PredicateExpr::Not(Box::new(PredicateExpr::eq("a", 3)))),
                Box::new(PredicateExpr::Cmp {
                    register: "b".into(),
                    op: CmpOp::Le,
                    value: 1,
                }),
            ),
            PredicateExpr::Not(Box::new(PredicateExpr::And(
                Box::new(PredicateExpr::eq("a", 1)),
                Box::new(PredicateExpr::eq("b", 1)),
            ))),
        ];

        let cfg = z3::Config::new();
        z3::with_z3_config(&cfg, || {
            for e in &exprs {
                for a in 0u64..4 {
                    for b in 0u64..4 {
                        let want = e.eval(&regs(&[("a", a as u128), ("b", b as u128)]));

                        let a_bv = z3::ast::BV::new_const("a", 2);
                        let b_bv = z3::ast::BV::new_const("b", 2);
                        let lookup = |name: &str| -> Option<z3::ast::BV> {
                            match name {
                                "a" => Some(a_bv.clone()),
                                "b" => Some(b_bv.clone()),
                                _ => None,
                            }
                        };
                        let constraint = e.build_constraint(&lookup).expect("all regs present");

                        let solver = z3::Solver::new();
                        // Pin the assignment (named bindings, mirroring the
                        // smt_must_edge assert pattern).
                        let a_val = z3::ast::BV::from_u64(a, 2);
                        let b_val = z3::ast::BV::from_u64(b, 2);
                        let a_pin = a_bv.eq(&a_val);
                        let b_pin = b_bv.eq(&b_val);
                        solver.assert(&a_pin);
                        solver.assert(&b_pin);
                        solver.assert(&constraint);
                        let got = matches!(solver.check(), z3::SatResult::Sat);

                        assert_eq!(
                            want, got,
                            "eval/SMT disagree for {e:?} at a={a}, b={b}: eval={want}, smt={got}"
                        );
                    }
                }
            }
        });
    }
}
