//! BTOR2 AST.
//!
//! BTOR2 (Niemetz–Preiner–Wolf, FMCAD 2018) is a line-based, word-level
//! verification IR. Each line declares a node identified by its line id (NID).
//! Sorts, operators, and outputs all live in the same flat namespace.
//!
//! # Format summary
//!
//! ```text
//! <nid> sort bitvec <width>
//! <nid> sort array <index_sort_nid> <element_sort_nid>
//! <nid> input <sort_nid> [<symbol>]
//! <nid> state <sort_nid> [<symbol>]
//! <nid> init  <sort_nid> <state_nid> <value_nid>
//! <nid> next  <sort_nid> <state_nid> <value_nid>
//! <nid> <op> <sort_nid> <arg_nids...>     ; operator
//! <nid> bad        <signal_nid>
//! <nid> constraint <signal_nid>
//! <nid> fair       <signal_nid>
//! <nid> output     <signal_nid>
//! <nid> justice <num> <signal_nids...>
//! ```
//!
//! References:
//! - <https://fmv.jku.at/papers/NiemetzPreinerWolfBiere-FMCAD18.pdf>
//! - <https://github.com/Boolector/btor2tools>

use std::collections::HashMap;

/// Node id (line number in the BTOR2 file).
pub type Nid = i64;

/// Negative NIDs denote bit-vector negation in operand position
/// (BTOR2 supports `-N` as shorthand for "bit-not the value at N").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operand(pub Nid);

impl Operand {
    pub fn nid(&self) -> Nid {
        self.0.abs()
    }
    pub fn is_negated(&self) -> bool {
        self.0 < 0
    }
}

/// Sort declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sort {
    /// `sort bitvec <width>` — fixed-width bit vector.
    BitVec { width: u32 },
    /// `sort array <index_sort> <element_sort>` — array (out of v1 scope).
    Array { index: Nid, element: Nid },
}

/// One BTOR2 node.
#[derive(Debug, Clone)]
pub enum Node {
    /// Sort declaration line.
    Sort { sort: Sort },

    /// `<nid> input <sort> [name]`
    Input { sort: Nid, symbol: Option<String> },
    /// `<nid> state <sort> [name]`
    State { sort: Nid, symbol: Option<String> },

    /// Constant (zero/one/ones/const/constd/consth).
    Const { sort: Nid, value: ConstValue },

    /// Bit-vector operator with `n` operands.
    Op {
        sort: Nid,
        op: Op,
        args: Vec<Operand>,
        /// Optional trailing symbol from the BTOR2 line (`<nid> uext <sort>
        /// <nid> 0 <name>`). Yosys uses no-op `uext _ _ 0 NAME` lines as
        /// named aliases of internal nets — we forward the name through the
        /// alias chain to the underlying state so user-given register names
        /// (`fill`, `state`, …) appear on `valuations { … }` blocks instead
        /// of synthetic `$auto$async2sync.cc:…` strings.
        symbol: Option<String>,
    },

    /// `<nid> init <sort> <state> <value>`
    Init {
        sort: Nid,
        state: Nid,
        value: Operand,
    },
    /// `<nid> next <sort> <state> <value>`
    Next {
        sort: Nid,
        state: Nid,
        value: Operand,
    },

    /// `<nid> bad <signal>` — safety violation.
    Bad { signal: Operand },
    /// `<nid> constraint <signal>` — environment assumption.
    Constraint { signal: Operand },
    /// `<nid> fair <signal>` — fairness condition.
    Fair { signal: Operand },
    /// `<nid> output <signal>` — informational output.
    Output {
        signal: Operand,
        symbol: Option<String>,
    },
    /// `<nid> justice <num> <signals...>` — liveness conjunction (all must hold infinitely often).
    Justice { signals: Vec<Operand> },
}

/// Constant value representation (preserved at the original radix for fidelity).
#[derive(Debug, Clone)]
pub enum ConstValue {
    /// `zero` keyword.
    Zero,
    /// `one` keyword.
    One,
    /// `ones` keyword (all bits set).
    Ones,
    /// `const <bits>` — binary literal.
    Bin(String),
    /// `constd <decimal>` — decimal literal.
    Dec(i128),
    /// `consth <hex>` — hex literal.
    Hex(String),
}

/// Bit-vector / boolean operators supported by mununu's BTOR2 reader.
///
/// Phase 1 covers the core ~25 ops needed for FSM-class designs
/// elaborated by Yosys. Array ops, overflow detectors, and the
/// modular arithmetic (sdiv/udiv/smod/srem/urem) are recognized at
/// parse time and rejected with a clear error in the bit-blaster
/// for v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    // Unary
    Not,
    Inc,
    Dec,
    Neg,
    Redand,
    Redor,
    Redxor,
    // Binary boolean / bitwise
    Iff,
    Implies,
    Eq,
    Neq,
    And,
    Or,
    Xor,
    Nand,
    Nor,
    Xnor,
    // Compare
    Sgt,
    Ugt,
    Sgte,
    Ugte,
    Slt,
    Ult,
    Slte,
    Ulte,
    // Arithmetic
    Add,
    Sub,
    Mul,
    // Shifts
    Sll,
    Srl,
    Sra,
    Rol,
    Ror,
    // Concat / slice
    Concat,
    Slice, // ternary in BTOR2 (signal, upper, lower) — args = [sig, upper, lower]
    // Extension
    Uext,
    Sext,
    // Conditional
    Ite,
    // Recognized but unsupported in Phase 1 bit-blasting
    Sdiv,
    Udiv,
    Smod,
    Srem,
    Urem,
    Saddo,
    Ssubo,
    Smulo,
    Uaddo,
    Usubo,
    Umulo,
    Sdivo,
    Read,
    Write,
}

impl Op {
    /// Convert a BTOR2 keyword into an `Op`. Returns `None` for unrecognized
    /// keywords (which include `sort`, `input`, `state`, `init`, `next`, etc.,
    /// handled separately by the parser).
    pub fn from_keyword(kw: &str) -> Option<Op> {
        Some(match kw {
            "not" => Op::Not,
            "inc" => Op::Inc,
            "dec" => Op::Dec,
            "neg" => Op::Neg,
            "redand" => Op::Redand,
            "redor" => Op::Redor,
            "redxor" => Op::Redxor,
            "iff" => Op::Iff,
            "implies" => Op::Implies,
            "eq" => Op::Eq,
            "neq" => Op::Neq,
            "and" => Op::And,
            "or" => Op::Or,
            "xor" => Op::Xor,
            "nand" => Op::Nand,
            "nor" => Op::Nor,
            "xnor" => Op::Xnor,
            "sgt" => Op::Sgt,
            "ugt" => Op::Ugt,
            "sgte" => Op::Sgte,
            "ugte" => Op::Ugte,
            "slt" => Op::Slt,
            "ult" => Op::Ult,
            "slte" => Op::Slte,
            "ulte" => Op::Ulte,
            "add" => Op::Add,
            "sub" => Op::Sub,
            "mul" => Op::Mul,
            "sll" => Op::Sll,
            "srl" => Op::Srl,
            "sra" => Op::Sra,
            "rol" => Op::Rol,
            "ror" => Op::Ror,
            "concat" => Op::Concat,
            "slice" => Op::Slice,
            "uext" => Op::Uext,
            "sext" => Op::Sext,
            "ite" => Op::Ite,
            "sdiv" => Op::Sdiv,
            "udiv" => Op::Udiv,
            "smod" => Op::Smod,
            "srem" => Op::Srem,
            "urem" => Op::Urem,
            "saddo" => Op::Saddo,
            "ssubo" => Op::Ssubo,
            "smulo" => Op::Smulo,
            "uaddo" => Op::Uaddo,
            "usubo" => Op::Usubo,
            "umulo" => Op::Umulo,
            "sdivo" => Op::Sdivo,
            "read" => Op::Read,
            "write" => Op::Write,
            _ => return None,
        })
    }

    /// The BTOR2 keyword for this operator (inverse of [`Op::from_keyword`]).
    /// Used by the BTOR2 emitter ([`super::emit`]).
    pub fn keyword(&self) -> &'static str {
        match self {
            Op::Not => "not",
            Op::Inc => "inc",
            Op::Dec => "dec",
            Op::Neg => "neg",
            Op::Redand => "redand",
            Op::Redor => "redor",
            Op::Redxor => "redxor",
            Op::Iff => "iff",
            Op::Implies => "implies",
            Op::Eq => "eq",
            Op::Neq => "neq",
            Op::And => "and",
            Op::Or => "or",
            Op::Xor => "xor",
            Op::Nand => "nand",
            Op::Nor => "nor",
            Op::Xnor => "xnor",
            Op::Sgt => "sgt",
            Op::Ugt => "ugt",
            Op::Sgte => "sgte",
            Op::Ugte => "ugte",
            Op::Slt => "slt",
            Op::Ult => "ult",
            Op::Slte => "slte",
            Op::Ulte => "ulte",
            Op::Add => "add",
            Op::Sub => "sub",
            Op::Mul => "mul",
            Op::Sll => "sll",
            Op::Srl => "srl",
            Op::Sra => "sra",
            Op::Rol => "rol",
            Op::Ror => "ror",
            Op::Concat => "concat",
            Op::Slice => "slice",
            Op::Uext => "uext",
            Op::Sext => "sext",
            Op::Ite => "ite",
            Op::Sdiv => "sdiv",
            Op::Udiv => "udiv",
            Op::Smod => "smod",
            Op::Srem => "srem",
            Op::Urem => "urem",
            Op::Saddo => "saddo",
            Op::Ssubo => "ssubo",
            Op::Smulo => "smulo",
            Op::Uaddo => "uaddo",
            Op::Usubo => "usubo",
            Op::Umulo => "umulo",
            Op::Sdivo => "sdivo",
            Op::Read => "read",
            Op::Write => "write",
        }
    }

    /// Number of operands the operator expects.
    pub fn arity(&self) -> usize {
        match self {
            Op::Not
            | Op::Inc
            | Op::Dec
            | Op::Neg
            | Op::Redand
            | Op::Redor
            | Op::Redxor
            | Op::Uext
            | Op::Sext => 1,
            Op::Slice => 1, // signal — upper/lower are immediate ints, not operands
            Op::Iff
            | Op::Implies
            | Op::Eq
            | Op::Neq
            | Op::And
            | Op::Or
            | Op::Xor
            | Op::Nand
            | Op::Nor
            | Op::Xnor
            | Op::Sgt
            | Op::Ugt
            | Op::Sgte
            | Op::Ugte
            | Op::Slt
            | Op::Ult
            | Op::Slte
            | Op::Ulte
            | Op::Add
            | Op::Sub
            | Op::Mul
            | Op::Sll
            | Op::Srl
            | Op::Sra
            | Op::Rol
            | Op::Ror
            | Op::Concat
            | Op::Sdiv
            | Op::Udiv
            | Op::Smod
            | Op::Srem
            | Op::Urem
            | Op::Saddo
            | Op::Ssubo
            | Op::Smulo
            | Op::Uaddo
            | Op::Usubo
            | Op::Umulo
            | Op::Sdivo
            | Op::Read => 2,
            Op::Ite | Op::Write => 3,
        }
    }

    /// True if this operator is supported by the Phase 1 bit-blaster.
    pub fn is_blastable(&self) -> bool {
        matches!(
            self,
            Op::Not
                | Op::Inc
                | Op::Dec
                | Op::Neg
                | Op::Redand
                | Op::Redor
                | Op::Redxor
                | Op::Iff
                | Op::Implies
                | Op::Eq
                | Op::Neq
                | Op::And
                | Op::Or
                | Op::Xor
                | Op::Nand
                | Op::Nor
                | Op::Xnor
                | Op::Sgt
                | Op::Ugt
                | Op::Sgte
                | Op::Ugte
                | Op::Slt
                | Op::Ult
                | Op::Slte
                | Op::Ulte
                | Op::Add
                | Op::Sub
                | Op::Mul
                | Op::Sll
                | Op::Srl
                | Op::Sra
                | Op::Rol
                | Op::Ror
                | Op::Concat
                | Op::Slice
                | Op::Uext
                | Op::Sext
                | Op::Ite
        )
    }
}

/// One BTOR2 line, retaining its NID and any operator-specific immediates.
#[derive(Debug, Clone)]
pub struct Line {
    pub nid: Nid,
    pub node: Node,
    /// Slice / extension immediates that follow the operand list:
    ///   `slice <sort> <signal> <upper> <lower>`
    ///   `uext / sext <sort> <signal> <amount>`
    pub immediates: Vec<u32>,
    /// Source line number (1-based) for diagnostics.
    pub source_line: usize,
}

/// A parsed BTOR2 file.
#[derive(Debug, Clone, Default)]
pub struct Btor2File {
    /// All lines in declaration order.
    pub lines: Vec<Line>,
    /// NID → index into `lines`, for fast lookup.
    pub by_nid: HashMap<Nid, usize>,
}

impl Btor2File {
    pub fn lookup(&self, nid: Nid) -> Option<&Line> {
        self.by_nid.get(&nid).map(|&i| &self.lines[i])
    }

    /// Iterate over input declarations.
    pub fn inputs(&self) -> impl Iterator<Item = &Line> {
        self.lines
            .iter()
            .filter(|l| matches!(l.node, Node::Input { .. }))
    }

    /// Iterate over state declarations.
    pub fn states(&self) -> impl Iterator<Item = &Line> {
        self.lines
            .iter()
            .filter(|l| matches!(l.node, Node::State { .. }))
    }

    /// N5 census column (2026-11-08) — the `state`s of this file whose sort is an ARRAY, in
    /// declaration order. Each is named by its own symbol, else by the first no-op zero-extension
    /// alias that names it (`uext <sort> <state> 0 NAME`, Yosys' naming of anonymous cells),
    /// else `nid<N>`.
    ///
    /// Applied to a property's `cone_slice`, this says whether the row's cone carries a memory —
    /// the row-facts `cone_array_states` field. A diagnostic only: nothing on a verdict path
    /// reads it.
    pub fn array_state_names(&self) -> Vec<String> {
        let is_array = |sort: Nid| {
            self.lookup(sort).is_some_and(|l| {
                matches!(
                    l.node,
                    Node::Sort {
                        sort: Sort::Array { .. }
                    }
                )
            })
        };
        let alias = |state: Nid| {
            self.lines.iter().find_map(|l| match &l.node {
                Node::Op {
                    op: Op::Uext,
                    args,
                    symbol: Some(name),
                    ..
                } if args.first().map(Operand::nid) == Some(state)
                    && l.immediates.first() == Some(&0) =>
                {
                    Some(name.clone())
                }
                _ => None,
            })
        };
        self.states()
            .filter_map(|l| match &l.node {
                Node::State { sort, symbol } if is_array(*sort) => Some(
                    symbol
                        .clone()
                        .or_else(|| alias(l.nid))
                        .unwrap_or_else(|| format!("nid{}", l.nid)),
                ),
                _ => None,
            })
            .collect()
    }

    /// Iterate over `bad` declarations.
    pub fn bads(&self) -> impl Iterator<Item = &Line> {
        self.lines
            .iter()
            .filter(|l| matches!(l.node, Node::Bad { .. }))
    }

    /// Iterate over `constraint` declarations.
    pub fn constraints(&self) -> impl Iterator<Item = &Line> {
        self.lines
            .iter()
            .filter(|l| matches!(l.node, Node::Constraint { .. }))
    }

    /// Iterate over `justice` declarations.
    pub fn justices(&self) -> impl Iterator<Item = &Line> {
        self.lines
            .iter()
            .filter(|l| matches!(l.node, Node::Justice { .. }))
    }

    /// Iterate over `fair` declarations.
    pub fn fairs(&self) -> impl Iterator<Item = &Line> {
        self.lines
            .iter()
            .filter(|l| matches!(l.node, Node::Fair { .. }))
    }
}
