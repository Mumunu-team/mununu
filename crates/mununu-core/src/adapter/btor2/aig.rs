//! N5.0 (Track N) — an and-inverter graph (AIG) of the BTOR2 transition function: the CNF source
//! of the owned bit-level SAT engines (the bit-level BMC in [`super::sat_bmc`] now, the owned IC3
//! next).
//!
//! # Shape
//!
//! - [`Aig`] is a structurally hashed AIG. Node 0 is the constant FALSE; every other node is a free
//!   leaf (a register or input bit) or a two-input AND. A [`Lit`] is a node index plus a
//!   complement bit, so NOT is free. Nodes are created in topological order, which the simulator
//!   and the CNF encoder rely on.
//! - [`AigBackend`] is a fourth [`BvTermBackend`], beside the concrete evaluator, the Z3 encoder
//!   and the BDD bit-blaster. Its value is a bit-vector of literals, LSB first. The gadgets are
//!   its own, not a generic refactor of the BDD bit-blaster's: those are interleaved with OxiDD
//!   budget checks and level validation, and the exact engine is not touched for this.
//! - [`AigModel::build`] walks a BTOR2 design once and keeps, per state, its leaves, its `next`
//!   and its `init`, plus every `bad` and `constraint` as one literal.
//!
//! # Correctness gate
//!
//! Every operator is checked bit for bit against the concrete evaluator
//! ([`super::bit_blast::simulate_one_step`]) on random operands at widths 1–8. Division, remainder
//! and the overflow predicates are not in the concrete evaluator, so they are checked against an
//! SMT-LIB reference written in the test. Arrays (`read` / `write`) are refused, not approximated:
//! the model reports them and the engine abstains.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::adapter::btor2::ast::{Btor2File, ConstValue, Nid, Node, Op, Operand, Sort};
use crate::adapter::btor2::bit_blast::eval_const_value;
use crate::adapter::btor2::parser::bv_width;
use crate::adapter::btor2::term_backend::{BvTermBackend, walk_design};

/// An AIG literal: `2 * node + complement`. Node 0 is FALSE, so `Lit(0)` is FALSE and `Lit(1)` TRUE.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Lit(u32);

impl Lit {
    pub const FALSE: Lit = Lit(0);
    pub const TRUE: Lit = Lit(1);

    /// The node this literal reads.
    pub fn node(self) -> u32 {
        self.0 >> 1
    }

    /// Whether the literal is the complement of its node.
    pub fn is_complemented(self) -> bool {
        self.0 & 1 == 1
    }

    fn of(node: u32, complemented: bool) -> Lit {
        Lit((node << 1) | u32::from(complemented))
    }
}

impl std::ops::Not for Lit {
    type Output = Lit;
    fn not(self) -> Lit {
        Lit(self.0 ^ 1)
    }
}

/// One AIG node.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AigNode {
    /// Node 0: the constant FALSE.
    False,
    /// A free leaf: a register bit or an input bit of some frame.
    Leaf,
    /// `a ∧ b`, with `a < b`.
    And(Lit, Lit),
}

/// mununu#1142 — the most nodes an AIG can hold: a [`Lit`] is `2 * node + complement` in a
/// `u32`, so a node index past this would alias literals.
pub const MAX_NODES: u32 = (1 << 31) - 1;

/// mununu#1142 — the default node cap of a build ([`BuildLimits::node_cap`]), `MUNUNU_AIG_NODE_CAP`
/// unset: 2^26 nodes (about 1.5 GB of nodes and hash entries). Below [`MAX_NODES`] by far, so a
/// build stops with a named abstention long before the literal space is exhausted.
pub const DEFAULT_NODE_CAP: u32 = 1 << 26;

/// How often (in nodes) the build polls its deadline, cancel flag and the run budget.
const POLL_EVERY: usize = 4096;

/// mununu#1142 — what stops an [`AigModel::build_with`] before it has every gate: the design's
/// own size against the cap, the member's deadline or cancel flag, or the thread's run budget.
/// Every arm names how far the build got, so the report says what was built when it stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildStop {
    /// The design cannot be modelled (arrays, an operator the backend refuses): a refusal, more
    /// budget will not help.
    Refused(String),
    /// The AIG passed the node cap.
    NodeCap { nodes: u32, cap: u32 },
    /// The member's deadline expired during the build.
    Deadline { nodes: u32 },
    /// The thread's run budget expired during the build.
    RunBudget { nodes: u32 },
    /// The member's cancel flag went up during the build (a peer decided).
    Cancelled { nodes: u32 },
}

impl std::fmt::Display for BuildStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildStop::Refused(why) => f.write_str(why),
            BuildStop::NodeCap { nodes, cap } if *cap >= MAX_NODES => write!(
                f,
                "aig: the model passed the node cap while being built ({nodes} nodes, cap \
                 {cap}, the most the engine's literals can address — a limit of the technique); \
                 a smaller design, or the SMT members"
            ),
            BuildStop::NodeCap { nodes, cap } => write!(
                f,
                "aig: the model passed the node cap while being built ({nodes} nodes, cap \
                 {cap}); MUNUNU_AIG_NODE_CAP raises the cap (at most {MAX_NODES}), or the SMT \
                 members take the design"
            ),
            BuildStop::Deadline { nodes } => write!(
                f,
                "aig: the member's deadline expired while the model was being built ({nodes} \
                 nodes so far); more time on the verb's timeout, or a smaller design"
            ),
            BuildStop::RunBudget { nodes } => write!(
                f,
                "aig: the run budget expired while the model was being built ({nodes} nodes so \
                 far); more time on the run's budget, or a smaller design"
            ),
            BuildStop::Cancelled { nodes } => {
                write!(f, "aig: the build was cancelled ({nodes} nodes so far)")
            }
        }
    }
}

impl From<String> for BuildStop {
    fn from(why: String) -> Self {
        BuildStop::Refused(why)
    }
}

/// mununu#1142 — the stops a build honours. `Default` is the configured node cap and no clock.
#[derive(Clone, Debug, Default)]
pub struct BuildLimits {
    /// The most nodes the build may create; `None` = [`default_node_cap`].
    pub node_cap: Option<u32>,
    /// A wall-clock deadline, polled every [`POLL_EVERY`] nodes.
    pub deadline: Option<Instant>,
    /// An external cancellation, polled with the deadline.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl BuildLimits {
    /// The limits a member passes: its own deadline and cancel flag, the configured cap.
    pub fn member(deadline: Option<Instant>, cancel: Option<Arc<AtomicBool>>) -> Self {
        Self {
            node_cap: None,
            deadline,
            cancel,
        }
    }
}

/// mununu#1142 — the node cap from `MUNUNU_AIG_NODE_CAP` (a node count, clamped to
/// [`MAX_NODES`]), else [`DEFAULT_NODE_CAP`]. An unparsable value is the default, logged.
pub fn default_node_cap() -> u32 {
    node_cap_from(crate::adapter::env_var::read("MUNUNU_AIG_NODE_CAP").as_deref())
}

/// [`default_node_cap`] over the variable's value.
pub fn node_cap_from(value: Option<&str>) -> u32 {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => DEFAULT_NODE_CAP,
        Some(v) => match v.parse::<u64>() {
            Ok(n) if n >= 1 => u32::try_from(n.min(u64::from(MAX_NODES))).unwrap_or(MAX_NODES),
            _ => {
                tracing::warn!(
                    value = v,
                    "MUNUNU_AIG_NODE_CAP is not a node count; using the default"
                );
                DEFAULT_NODE_CAP
            }
        },
    }
}

/// A structurally hashed and-inverter graph.
#[derive(Clone, Debug)]
pub struct Aig {
    nodes: Vec<AigNode>,
    strash: HashMap<(Lit, Lit), u32>,
    /// mununu#1142 — the cap and the clocks the builder honours; [`Aig::new`] has the hard cap
    /// and no clock.
    cap: u32,
    deadline: Option<Instant>,
    cancel: Option<Arc<AtomicBool>>,
    /// The first stop hit; once set, no node is created (every request returns FALSE) and the
    /// build returns it.
    tripped: Option<BuildStop>,
    /// Whether the clocks and the run budget are polled: true while a build runs, false once
    /// it finished (a finished model is extended without polls and never trips).
    polling: bool,
}

impl Default for Aig {
    fn default() -> Self {
        Self::new()
    }
}

impl Aig {
    /// An AIG with the hard cap ([`MAX_NODES`]) and no clock.
    pub fn new() -> Self {
        Self::with_limits(&BuildLimits {
            node_cap: Some(MAX_NODES),
            deadline: None,
            cancel: None,
        })
    }

    /// mununu#1142 — an AIG that stops at `limits` (see [`BuildStop`]).
    pub fn with_limits(limits: &BuildLimits) -> Self {
        Self {
            nodes: vec![AigNode::False],
            strash: HashMap::new(),
            cap: limits
                .node_cap
                .unwrap_or_else(default_node_cap)
                .min(MAX_NODES),
            deadline: limits.deadline,
            cancel: limits.cancel.clone(),
            tripped: None,
            polling: true,
        }
    }

    /// mununu#1142 — the stop the builder hit, if any.
    pub fn tripped(&self) -> Option<&BuildStop> {
        self.tripped.as_ref()
    }

    /// The number of nodes, the constant included.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the graph holds only the constant.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    /// The node at `index` (`None` past the end).
    pub fn node(&self, index: u32) -> Option<AigNode> {
        self.nodes.get(index as usize).copied()
    }

    fn push(&mut self, node: AigNode) -> u32 {
        // mununu#1142 — a tripped graph creates nothing more: the build returns the stop, and
        // memory stops growing at the cap (every later request is answered with FALSE).
        // SOUNDNESS: the FALSE placeholder is never read as a model — `build_with` returns the
        // stop before any caller sees the graph, and a successful build clears its limits and
        // its polls, so a finished model trips only at the literal space's own limit.
        if self.tripped.is_some() {
            return 0;
        }
        let n = self.nodes.len();
        let nodes = u32::try_from(n).unwrap_or(u32::MAX);
        let stop = if nodes >= self.cap {
            Some(BuildStop::NodeCap {
                nodes,
                cap: self.cap,
            })
        } else if self.polling && n % POLL_EVERY == 1 {
            if self
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                Some(BuildStop::Cancelled { nodes })
            } else if self.deadline.is_some_and(|d| Instant::now() >= d) {
                Some(BuildStop::Deadline { nodes })
            } else if crate::adapter::run_budget::expired() {
                Some(BuildStop::RunBudget { nodes })
            } else {
                None
            }
        } else {
            None
        };
        if let Some(stop) = stop {
            self.tripped = Some(stop);
            return 0;
        }
        self.nodes.push(node);
        nodes
    }

    /// A fresh free leaf.
    pub fn leaf(&mut self) -> Lit {
        Lit::of(self.push(AigNode::Leaf), false)
    }

    /// `n` fresh leaves, LSB first.
    pub fn leaves(&mut self, n: u32) -> Vec<Lit> {
        (0..n).map(|_| self.leaf()).collect()
    }

    /// `a ∧ b`, folding constants and trivial cases, and sharing a structurally equal gate.
    pub fn and(&mut self, a: Lit, b: Lit) -> Lit {
        if a == Lit::FALSE || b == Lit::FALSE || a == !b {
            return Lit::FALSE;
        }
        if a == Lit::TRUE || a == b {
            return b;
        }
        if b == Lit::TRUE {
            return a;
        }
        let key = if a < b { (a, b) } else { (b, a) };
        if let Some(&n) = self.strash.get(&key) {
            return Lit::of(n, false);
        }
        let n = self.push(AigNode::And(key.0, key.1));
        self.strash.insert(key, n);
        Lit::of(n, false)
    }

    pub fn or(&mut self, a: Lit, b: Lit) -> Lit {
        !self.and(!a, !b)
    }

    pub fn xor(&mut self, a: Lit, b: Lit) -> Lit {
        if a == Lit::FALSE {
            return b;
        }
        if b == Lit::FALSE {
            return a;
        }
        if a == Lit::TRUE {
            return !b;
        }
        if b == Lit::TRUE {
            return !a;
        }
        let x = self.and(a, !b);
        let y = self.and(!a, b);
        self.or(x, y)
    }

    /// `c ? t : e`.
    pub fn mux(&mut self, c: Lit, t: Lit, e: Lit) -> Lit {
        if t == e || c == Lit::TRUE {
            return t;
        }
        if c == Lit::FALSE {
            return e;
        }
        let x = self.and(c, t);
        let y = self.and(!c, e);
        self.or(x, y)
    }

    /// Evaluate every node under `leaf_value` (indexed by node; non-leaf entries ignored) and
    /// return the value of `lit`. The simulator the correctness gate and the trace replay use.
    pub fn eval(&self, values: &[bool], lit: Lit) -> bool {
        values.get(lit.node() as usize).copied().unwrap_or(false) ^ lit.is_complemented()
    }

    /// Compute every node's value from the leaves' values (`leaf(i)` for leaf node `i`).
    pub fn simulate(&self, leaf: impl Fn(u32) -> bool) -> Vec<bool> {
        let mut v = vec![false; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            v[i] = match *n {
                AigNode::False => false,
                AigNode::Leaf => leaf(i as u32),
                AigNode::And(a, b) => {
                    (v[a.node() as usize] ^ a.is_complemented())
                        && (v[b.node() as usize] ^ b.is_complemented())
                }
            };
        }
        v
    }

    // ---- word-level gadgets (LSB first) ----

    /// `a + b + cin`, truncated to `a.len()` bits, and the carry out.
    pub fn add(&mut self, a: &[Lit], b: &[Lit], cin: Lit) -> (Vec<Lit>, Lit) {
        let mut carry = cin;
        let mut sum = Vec::with_capacity(a.len());
        for (i, &ai) in a.iter().enumerate() {
            let bi = b.get(i).copied().unwrap_or(Lit::FALSE);
            let axb = self.xor(ai, bi);
            sum.push(self.xor(axb, carry));
            let ab = self.and(ai, bi);
            let c_axb = self.and(carry, axb);
            carry = self.or(ab, c_axb);
        }
        (sum, carry)
    }

    pub fn neg(&mut self, a: &[Lit]) -> Vec<Lit> {
        let na: Vec<Lit> = a.iter().map(|&x| !x).collect();
        let zero = vec![Lit::FALSE; a.len()];
        self.add(&na, &zero, Lit::TRUE).0
    }

    pub fn sub(&mut self, a: &[Lit], b: &[Lit]) -> Vec<Lit> {
        let nb: Vec<Lit> = b.iter().map(|&x| !x).collect();
        self.add(a, &nb, Lit::TRUE).0
    }

    /// `a <u b`: no carry out of `a + ¬b + 1`.
    pub fn ult(&mut self, a: &[Lit], b: &[Lit]) -> Lit {
        let nb: Vec<Lit> = b.iter().map(|&x| !x).collect();
        !self.add(a, &nb, Lit::TRUE).1
    }

    /// `a <s b`: unsigned compare with both sign bits flipped.
    pub fn slt(&mut self, a: &[Lit], b: &[Lit]) -> Lit {
        let flip = |v: &[Lit]| -> Vec<Lit> {
            let mut v = v.to_vec();
            if let Some(msb) = v.last_mut() {
                *msb = !*msb;
            }
            v
        };
        let (fa, fb) = (flip(a), flip(b));
        self.ult(&fa, &fb)
    }

    pub fn eq(&mut self, a: &[Lit], b: &[Lit]) -> Lit {
        let mut acc = Lit::TRUE;
        for (i, &ai) in a.iter().enumerate() {
            let bi = b.get(i).copied().unwrap_or(Lit::FALSE);
            let d = self.xor(ai, bi);
            acc = self.and(acc, !d);
        }
        acc
    }

    pub fn and_reduce(&mut self, a: &[Lit]) -> Lit {
        a.iter().fold(Lit::TRUE, |acc, &x| self.and(acc, x))
    }

    pub fn or_reduce(&mut self, a: &[Lit]) -> Lit {
        a.iter().fold(Lit::FALSE, |acc, &x| self.or(acc, x))
    }

    pub fn xor_reduce(&mut self, a: &[Lit]) -> Lit {
        a.iter().fold(Lit::FALSE, |acc, &x| self.xor(acc, x))
    }

    pub fn mux_vec(&mut self, c: Lit, t: &[Lit], e: &[Lit]) -> Vec<Lit> {
        t.iter()
            .zip(e)
            .map(|(&ti, &ei)| self.mux(c, ti, ei))
            .collect()
    }

    /// Shift-and-add multiplication, truncated to `a.len()` bits.
    pub fn mul(&mut self, a: &[Lit], b: &[Lit]) -> Vec<Lit> {
        let w = a.len();
        let mut acc = vec![Lit::FALSE; w];
        for (i, &bi) in b.iter().enumerate().take(w) {
            if bi == Lit::FALSE {
                continue;
            }
            let partial: Vec<Lit> = (0..w)
                .map(|j| {
                    if j >= i {
                        self.and(a[j - i], bi)
                    } else {
                        Lit::FALSE
                    }
                })
                .collect();
            acc = self.add(&acc, &partial, Lit::FALSE).0;
        }
        acc
    }

    /// Barrel shift of `a` by the variable amount `s`: left (fill 0), or right with `fill`.
    /// A stage whose `2^k` reaches the width shifts everything out, so an amount ≥ width gives
    /// the fill: BTOR2's saturating semantics.
    pub fn shift(&mut self, a: &[Lit], s: &[Lit], left: bool, fill: Lit) -> Vec<Lit> {
        let w = a.len();
        let mut cur = a.to_vec();
        for (k, &sk) in s.iter().enumerate() {
            let by = 1usize.checked_shl(k as u32).unwrap_or(usize::MAX);
            let shifted: Vec<Lit> = (0..w)
                .map(|j| {
                    if left {
                        if by <= j { cur[j - by] } else { Lit::FALSE }
                    } else if by < w && j + by < w {
                        cur[j + by]
                    } else {
                        fill
                    }
                })
                .collect();
            cur = self.mux_vec(sk, &shifted, &cur);
        }
        cur
    }

    /// Rotate `a` by `s mod width`.
    pub fn rotate(&mut self, a: &[Lit], s: &[Lit], left: bool) -> Vec<Lit> {
        let w = a.len();
        if w <= 1 {
            return a.to_vec();
        }
        let wconst = self.constant(w as u128, s.len().max(1));
        let amount = self.udivrem(s, &wconst).1;
        let mut cur = a.to_vec();
        for (k, &sk) in amount.iter().enumerate() {
            let by = 1usize.checked_shl(k as u32).map_or(0, |b| b % w);
            if by == 0 {
                continue;
            }
            let rotated: Vec<Lit> = (0..w)
                .map(|j| {
                    if left {
                        cur[(j + w - by) % w]
                    } else {
                        cur[(j + by) % w]
                    }
                })
                .collect();
            cur = self.mux_vec(sk, &rotated, &cur);
        }
        cur
    }

    /// The constant `v` as `width` literals (bits of `v` past 128 are zero).
    pub fn constant(&self, v: u128, width: usize) -> Vec<Lit> {
        (0..width)
            .map(|i| {
                if i < 128 && (v >> i) & 1 == 1 {
                    Lit::TRUE
                } else {
                    Lit::FALSE
                }
            })
            .collect()
    }

    /// Restoring division: `(a udiv b, a urem b)` at `a.len()` bits. A zero divisor gives
    /// all-ones and `a`, SMT-LIB's semantics, with no special case: every step's `r ≥ 0` holds.
    pub fn udivrem(&mut self, a: &[Lit], b: &[Lit]) -> (Vec<Lit>, Vec<Lit>) {
        let w = a.len();
        // The partial remainder carries one extra bit so the shift never overflows.
        let mut bx: Vec<Lit> = (0..w)
            .map(|i| b.get(i).copied().unwrap_or(Lit::FALSE))
            .collect();
        bx.push(Lit::FALSE);
        let mut r = vec![Lit::FALSE; w + 1];
        let mut q = vec![Lit::FALSE; w];
        for i in (0..w).rev() {
            r.pop();
            r.insert(0, a[i]);
            let lt = self.ult(&r, &bx);
            let diff = self.sub(&r, &bx);
            q[i] = !lt;
            r = self.mux_vec(lt, &r, &diff);
        }
        r.truncate(w);
        (q, r)
    }

    fn sign(a: &[Lit]) -> Lit {
        a.last().copied().unwrap_or(Lit::FALSE)
    }

    fn abs(&mut self, a: &[Lit]) -> Vec<Lit> {
        let n = self.neg(a);
        self.mux_vec(Self::sign(a), &n, a)
    }

    /// SMT-LIB `bvsdiv`.
    pub fn sdiv(&mut self, a: &[Lit], b: &[Lit]) -> Vec<Lit> {
        let (aa, ab) = (self.abs(a), self.abs(b));
        let q = self.udivrem(&aa, &ab).0;
        let s = self.xor(Self::sign(a), Self::sign(b));
        let nq = self.neg(&q);
        self.mux_vec(s, &nq, &q)
    }

    /// SMT-LIB `bvsrem`: the sign follows the dividend.
    pub fn srem(&mut self, a: &[Lit], b: &[Lit]) -> Vec<Lit> {
        let (aa, ab) = (self.abs(a), self.abs(b));
        let r = self.udivrem(&aa, &ab).1;
        let nr = self.neg(&r);
        self.mux_vec(Self::sign(a), &nr, &r)
    }

    /// SMT-LIB `bvsmod`: the sign follows the divisor.
    pub fn smod(&mut self, a: &[Lit], b: &[Lit]) -> Vec<Lit> {
        let (aa, ab) = (self.abs(a), self.abs(b));
        let u = self.udivrem(&aa, &ab).1;
        let (sa, sb) = (Self::sign(a), Self::sign(b));
        let nu = self.neg(&u);
        let nu_plus_b = self.add(&nu, b, Lit::FALSE).0;
        let u_plus_b = self.add(&u, b, Lit::FALSE).0;
        // (sa, sb) = (0,0) → u; (1,0) → −u + b; (0,1) → u + b; (1,1) → −u.
        let when_b_pos = self.mux_vec(sa, &nu_plus_b, &u);
        let when_b_neg = self.mux_vec(sa, &nu, &u_plus_b);
        let r = self.mux_vec(sb, &when_b_neg, &when_b_pos);
        let u_zero = !self.or_reduce(&u);
        self.mux_vec(u_zero, &u, &r)
    }

    fn uext(a: &[Lit], width: usize) -> Vec<Lit> {
        let mut v = a.to_vec();
        v.resize(width, Lit::FALSE);
        v
    }

    fn sext(a: &[Lit], width: usize) -> Vec<Lit> {
        let s = Self::sign(a);
        let mut v = a.to_vec();
        v.resize(width, s);
        v
    }
}

/// The AIG interpretation of a BTOR2 design: the fourth [`BvTermBackend`].
pub struct AigBackend<'a> {
    pub aig: &'a mut Aig,
    env: HashMap<Nid, Vec<Lit>>,
    honor_init: bool,
}

impl<'a> AigBackend<'a> {
    /// A backend over `aig` with `env` pre-bound (each state and input nid to its leaves).
    pub fn new(aig: &'a mut Aig, env: HashMap<Nid, Vec<Lit>>, honor_init: bool) -> Self {
        Self {
            aig,
            env,
            honor_init,
        }
    }

    /// The literals an operand names (complemented for a negative operand).
    pub fn resolve(&self, op: Operand) -> Result<Vec<Lit>, String> {
        let v = self
            .env
            .get(&op.nid())
            .ok_or_else(|| format!("aig: operand {} is unevaluated", op.nid()))?;
        Ok(if op.is_negated() {
            v.iter().map(|&l| !l).collect()
        } else {
            v.clone()
        })
    }
}

impl BvTermBackend for AigBackend<'_> {
    type Value = Vec<Lit>;
    type Error = String;

    fn eval_const(&mut self, value: &ConstValue, width: u32) -> Result<Self::Value, Self::Error> {
        let bv = eval_const_value(value, width)?;
        Ok((0..u64::from(width))
            .map(|b| if bv.bit(b) { Lit::TRUE } else { Lit::FALSE })
            .collect())
    }

    fn eval_op(
        &mut self,
        nid: Nid,
        op: Op,
        immediates: &[u32],
        args: &[Operand],
        width: u32,
    ) -> Result<Self::Value, Self::Error> {
        let w = width as usize;
        let vals: Vec<Vec<Lit>> = args
            .iter()
            .map(|a| self.resolve(*a))
            .collect::<Result<_, _>>()?;
        let read = |i: usize| -> Result<Vec<Lit>, String> {
            vals.get(i)
                .cloned()
                .ok_or_else(|| format!("aig: node {nid} lacks operand {i}"))
        };
        let g = &mut *self.aig;
        let bit = |l: Lit| vec![l];
        let out: Vec<Lit> = match op {
            Op::Not => read(0)?.iter().map(|&x| !x).collect(),
            Op::And | Op::Or | Op::Xor | Op::Nand | Op::Nor | Op::Xnor => {
                let (a, b) = (read(0)?, read(1)?);
                a.iter()
                    .zip(&b)
                    .map(|(&x, &y)| match op {
                        Op::And => g.and(x, y),
                        Op::Or => g.or(x, y),
                        Op::Xor => g.xor(x, y),
                        Op::Nand => !g.and(x, y),
                        Op::Nor => !g.or(x, y),
                        _ => !g.xor(x, y),
                    })
                    .collect()
            }
            Op::Inc | Op::Dec => {
                let a = read(0)?;
                let one = g.constant(1, a.len());
                if op == Op::Inc {
                    g.add(&a, &one, Lit::FALSE).0
                } else {
                    g.sub(&a, &one)
                }
            }
            Op::Neg => {
                let a = read(0)?;
                g.neg(&a)
            }
            Op::Redand => {
                let a = read(0)?;
                bit(g.and_reduce(&a))
            }
            Op::Redor => {
                let a = read(0)?;
                bit(g.or_reduce(&a))
            }
            Op::Redxor => {
                let a = read(0)?;
                bit(g.xor_reduce(&a))
            }
            Op::Iff | Op::Eq => {
                let (a, b) = (read(0)?, read(1)?);
                bit(g.eq(&a, &b))
            }
            Op::Neq => {
                let (a, b) = (read(0)?, read(1)?);
                bit(!g.eq(&a, &b))
            }
            Op::Implies => {
                let (a, b) = (read(0)?, read(1)?);
                let (x, y) = (a.first().copied(), b.first().copied());
                bit(g.or(!x.unwrap_or(Lit::FALSE), y.unwrap_or(Lit::FALSE)))
            }
            Op::Add => {
                let (a, b) = (read(0)?, read(1)?);
                g.add(&a, &b, Lit::FALSE).0
            }
            Op::Sub => {
                let (a, b) = (read(0)?, read(1)?);
                g.sub(&a, &b)
            }
            Op::Mul => {
                let (a, b) = (read(0)?, read(1)?);
                g.mul(&a, &b)
            }
            Op::Ult | Op::Ugte | Op::Ugt | Op::Ulte => {
                let (a, b) = (read(0)?, read(1)?);
                bit(match op {
                    Op::Ult => g.ult(&a, &b),
                    Op::Ugte => !g.ult(&a, &b),
                    Op::Ugt => g.ult(&b, &a),
                    _ => !g.ult(&b, &a),
                })
            }
            Op::Slt | Op::Sgte | Op::Sgt | Op::Slte => {
                let (a, b) = (read(0)?, read(1)?);
                bit(match op {
                    Op::Slt => g.slt(&a, &b),
                    Op::Sgte => !g.slt(&a, &b),
                    Op::Sgt => g.slt(&b, &a),
                    _ => !g.slt(&b, &a),
                })
            }
            Op::Sll | Op::Srl | Op::Sra => {
                let (a, b) = (read(0)?, read(1)?);
                let fill = if op == Op::Sra {
                    Aig::sign(&a)
                } else {
                    Lit::FALSE
                };
                g.shift(&a, &b, op == Op::Sll, fill)
            }
            Op::Rol | Op::Ror => {
                let (a, b) = (read(0)?, read(1)?);
                g.rotate(&a, &b, op == Op::Rol)
            }
            Op::Concat => {
                // BTOR2 `concat a b`: `a` is the high part.
                let (hi, lo) = (read(0)?, read(1)?);
                lo.into_iter().chain(hi).collect()
            }
            Op::Slice => {
                let a = read(0)?;
                let (upper, lower) = match immediates {
                    [u, l, ..] => (*u as usize, *l as usize),
                    _ => return Err(format!("aig: slice {nid} lacks its bounds")),
                };
                a.get(lower..=upper)
                    .ok_or_else(|| format!("aig: slice {nid} [{upper}:{lower}] out of range"))?
                    .to_vec()
            }
            Op::Uext => Aig::uext(&read(0)?, w),
            Op::Sext => Aig::sext(&read(0)?, w),
            Op::Ite => {
                let (c, t, e) = (read(0)?, read(1)?, read(2)?);
                let c = c.first().copied().unwrap_or(Lit::FALSE);
                g.mux_vec(c, &t, &e)
            }
            Op::Udiv | Op::Urem => {
                let (a, b) = (read(0)?, read(1)?);
                let (q, r) = g.udivrem(&a, &b);
                if op == Op::Udiv { q } else { r }
            }
            Op::Sdiv => {
                let (a, b) = (read(0)?, read(1)?);
                g.sdiv(&a, &b)
            }
            Op::Srem => {
                let (a, b) = (read(0)?, read(1)?);
                g.srem(&a, &b)
            }
            Op::Smod => {
                let (a, b) = (read(0)?, read(1)?);
                g.smod(&a, &b)
            }
            Op::Uaddo => {
                let (a, b) = (read(0)?, read(1)?);
                bit(g.add(&a, &b, Lit::FALSE).1)
            }
            Op::Usubo => {
                let (a, b) = (read(0)?, read(1)?);
                bit(g.ult(&a, &b))
            }
            Op::Saddo | Op::Ssubo => {
                let (a, b) = (read(0)?, read(1)?);
                let r = if op == Op::Saddo {
                    g.add(&a, &b, Lit::FALSE).0
                } else {
                    g.sub(&a, &b)
                };
                let (sa, sb, sr) = (Aig::sign(&a), Aig::sign(&b), Aig::sign(&r));
                // add: operands agree and the result differs; sub: operands differ and the
                // result differs from the minuend.
                let same = !g.xor(sa, sb);
                let operands = if op == Op::Saddo { same } else { !same };
                let flipped = g.xor(sr, sa);
                bit(g.and(operands, flipped))
            }
            Op::Umulo => {
                let (a, b) = (read(0)?, read(1)?);
                let n = a.len();
                let p = g.mul(&Aig::uext(&a, 2 * n), &Aig::uext(&b, 2 * n));
                bit(g.or_reduce(&p[n..]))
            }
            Op::Smulo => {
                let (a, b) = (read(0)?, read(1)?);
                let n = a.len();
                let p = g.mul(&Aig::sext(&a, 2 * n), &Aig::sext(&b, 2 * n));
                // Fits in n signed bits iff bits n-1 ..= 2n-1 all equal the sign bit n-1.
                let s = p.get(n.wrapping_sub(1)).copied().unwrap_or(Lit::FALSE);
                let mut any = Lit::FALSE;
                for &x in &p[n..] {
                    let d = g.xor(x, s);
                    any = g.or(any, d);
                }
                bit(any)
            }
            Op::Sdivo => {
                // The one overflowing case: INT_MIN / −1.
                let (a, b) = (read(0)?, read(1)?);
                let n = a.len();
                let min = {
                    let mut v = vec![Lit::FALSE; n];
                    if let Some(m) = v.last_mut() {
                        *m = Lit::TRUE;
                    }
                    v
                };
                let a_min = g.eq(&a, &min);
                let b_m1 = g.and_reduce(&b);
                bit(g.and(a_min, b_m1))
            }
            Op::Read | Op::Write => {
                return Err(crate::adapter::owner::mununu_limitation(
                    &format!(
                        "aig: node {nid} is an array operation ({}); the bit-level SAT engines \
                         take no arrays",
                        op.keyword()
                    ),
                    Some("the SMT members take arrays"),
                ));
            }
        };
        if out.len() != w {
            return Err(format!(
                "aig: node {nid} ({}) produced {} bits for a {w}-bit sort",
                op.keyword(),
                out.len()
            ));
        }
        Ok(out)
    }

    fn bind(&mut self, nid: Nid, value: Self::Value) {
        self.env.insert(nid, value);
    }

    fn honor_init(&self) -> bool {
        self.honor_init
    }

    fn read_operand(&self, op: Operand) -> Option<Self::Value> {
        self.resolve(op).ok()
    }

    fn uf_substitute(&mut self, _nid: Nid, _width: u32) -> Option<Self::Value> {
        None
    }
}

/// One register of an [`AigModel`].
#[derive(Clone, Debug)]
pub struct AigState {
    pub nid: Nid,
    pub symbol: Option<String>,
    /// The register's current-value leaves, LSB first.
    pub leaves: Vec<Lit>,
    /// `next`, over the current leaves and the inputs; `None` = a free register (no `next`).
    pub next: Option<Vec<Lit>>,
    /// `init`, over the frame-0 leaves; `None` = an unconstrained initial value.
    pub init: Option<Vec<Lit>>,
}

/// One input of an [`AigModel`].
#[derive(Clone, Debug)]
pub struct AigInput {
    pub nid: Nid,
    pub symbol: Option<String>,
    pub leaves: Vec<Lit>,
}

/// A BTOR2 design as one AIG: one frame of the transition function.
#[derive(Clone, Debug)]
pub struct AigModel {
    pub aig: Aig,
    pub states: Vec<AigState>,
    pub inputs: Vec<AigInput>,
    /// Every `bad`, one literal each, in file order.
    pub bads: Vec<Lit>,
    /// Every `constraint`, one literal each: assumed to hold in every frame.
    pub constraints: Vec<Lit>,
}

impl AigModel {
    /// Build the model under the default limits ([`BuildLimits::default`]: the configured node
    /// cap, no clock). Refuses (with the reason) a design with arrays, or an operator the
    /// backend cannot express; stops with the count at the cap or the run budget.
    pub fn build(file: &Btor2File) -> Result<Self, BuildStop> {
        Self::build_with(file, &BuildLimits::default())
    }

    /// mununu#1142 — [`Self::build`] under `limits`: the node cap, the member's deadline and
    /// cancel flag, and the thread's run budget are polled as the graph grows, so a member's
    /// wall or a peer's verdict stops the build and not only the solve after it.
    pub fn build_with(file: &Btor2File, limits: &BuildLimits) -> Result<Self, BuildStop> {
        let mut aig = Aig::with_limits(limits);
        let mut env = HashMap::new();
        let mut states = Vec::new();
        let mut inputs = Vec::new();
        for line in &file.lines {
            match &line.node {
                Node::State { sort, symbol } | Node::Input { sort, symbol } => {
                    let width = bv_width(file, *sort).ok_or_else(|| {
                        if matches!(
                            file.lookup(*sort).map(|l| &l.node),
                            Some(Node::Sort {
                                sort: Sort::Array { .. }
                            })
                        ) {
                            crate::adapter::owner::mununu_limitation(
                                &format!(
                                    "aig: `{}` is an array; the bit-level SAT engines take no \
                                     arrays",
                                    symbol.clone().unwrap_or_else(|| line.nid.to_string())
                                ),
                                Some("the SMT members take arrays"),
                            )
                        } else {
                            format!("aig: node {} has no bit-vector sort", line.nid)
                        }
                    })?;
                    let leaves = aig.leaves(width);
                    env.insert(line.nid, leaves.clone());
                    if matches!(line.node, Node::State { .. }) {
                        states.push(AigState {
                            nid: line.nid,
                            symbol: symbol.clone(),
                            leaves,
                            next: None,
                            init: None,
                        });
                    } else {
                        inputs.push(AigInput {
                            nid: line.nid,
                            symbol: symbol.clone(),
                            leaves,
                        });
                    }
                }
                _ => {}
            }
        }
        let mut backend = AigBackend::new(&mut aig, env, false);
        let walked = walk_design(file, &mut backend);
        // mununu#1142 — a stop during the walk outranks whatever the walk made of the FALSE
        // answers it got after it.
        if let Some(stop) = backend.aig.tripped.take() {
            return Err(stop);
        }
        walked.map_err(|e| match e {
            crate::adapter::btor2::term_backend::WalkError::Backend(m) => m,
            crate::adapter::btor2::term_backend::WalkError::NonBitvecSort(n) => {
                format!("aig: node {n} has a sort the bit-level engines take no values of")
            }
            crate::adapter::btor2::term_backend::WalkError::Unevaluated(n) => {
                format!("aig: node {n} reads a value that was never evaluated")
            }
        })?;
        let index: HashMap<Nid, usize> =
            states.iter().enumerate().map(|(i, s)| (s.nid, i)).collect();
        let mut bads = Vec::new();
        let mut constraints = Vec::new();
        for line in &file.lines {
            match &line.node {
                Node::Next { state, value, .. } => {
                    let v = backend.resolve(*value)?;
                    if let Some(&i) = index.get(state) {
                        states[i].next = Some(v);
                    }
                }
                Node::Init { state, value, .. } => {
                    let v = backend.resolve(*value)?;
                    if let Some(&i) = index.get(state) {
                        states[i].init = Some(v);
                    }
                }
                Node::Bad { signal } => {
                    bads.push(
                        backend
                            .resolve(*signal)?
                            .first()
                            .copied()
                            .unwrap_or(Lit::FALSE),
                    );
                }
                Node::Constraint { signal } => {
                    constraints.push(
                        backend
                            .resolve(*signal)?
                            .first()
                            .copied()
                            .unwrap_or(Lit::TRUE),
                    );
                }
                _ => {}
            }
        }
        drop(backend);
        if let Some(stop) = aig.tripped.take() {
            return Err(stop);
        }
        // The limits were the BUILD's: a finished model keeps no clock, no flag and no poll, so
        // a later extension of the graph cannot be answered with silent FALSE literals (the
        // hard cap of the literal space is the one bound that stays).
        aig.cap = MAX_NODES;
        aig.deadline = None;
        aig.cancel = None;
        aig.polling = false;
        Ok(Self {
            aig,
            states,
            inputs,
            bads,
            constraints,
        })
    }
}

/// The SMT-LIB definitions of BTOR2's division, remainder and overflow operators over `u64` /
/// `i64`, for operand widths up to 8: the reference every BV backend that encodes them is tested
/// against (this module's AIG gadgets, the z3 encoder, mununu#1037).
#[cfg(test)]
#[allow(clippy::panic)]
pub(crate) mod smtlib_reference {
    /// The operators [`eval`] defines.
    pub(crate) const OPS: [&str; 12] = [
        "udiv", "urem", "sdiv", "srem", "smod", "uaddo", "usubo", "saddo", "ssubo", "umulo",
        "smulo", "sdivo",
    ];

    fn mask(w: u32) -> u64 {
        if w >= 64 { u64::MAX } else { (1u64 << w) - 1 }
    }

    fn sx(v: u64, w: u32) -> i64 {
        let s = 64 - w;
        ((v << s) as i64) >> s
    }

    /// `op(a, b)` at width `w` (operands already masked), and the result width.
    pub(crate) fn eval(op: &str, w: u32, a: u64, b: u64) -> (u64, u32) {
        let (sa, sb) = (sx(a, w), sx(b, w));
        let m = mask(w);
        let min = sx(1 << (w - 1), w);
        let max = (m >> 1) as i64;
        match op {
            "udiv" => (a.checked_div(b).unwrap_or(m), w),
            "urem" => (a.checked_rem(b).unwrap_or(a), w),
            "sdiv" => (
                if sb == 0 {
                    if sa < 0 { 1 } else { m }
                } else {
                    (sa.wrapping_div(sb) as u64) & m
                },
                w,
            ),
            "srem" => (
                if sb == 0 {
                    a
                } else {
                    (sa.wrapping_rem(sb) as u64) & m
                },
                w,
            ),
            "smod" => (
                if sb == 0 {
                    a
                } else {
                    let r = sa.wrapping_rem(sb);
                    let r = if r != 0 && ((r < 0) != (sb < 0)) {
                        r + sb
                    } else {
                        r
                    };
                    (r as u64) & m
                },
                w,
            ),
            "uaddo" => (u64::from(a + b > m), 1),
            "usubo" => (u64::from(a < b), 1),
            "saddo" => (u64::from(!(min..=max).contains(&(sa + sb))), 1),
            "ssubo" => (u64::from(!(min..=max).contains(&(sa - sb))), 1),
            "umulo" => (u64::from(a * b > m), 1),
            "smulo" => (u64::from(!(min..=max).contains(&(sa * sb))), 1),
            "sdivo" => (u64::from(sa == min && sb == -1), 1),
            other => panic!("no SMT-LIB reference for {other}"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::adapter::btor2::bit_blast::simulate_one_step;
    use crate::adapter::btor2::parser;
    use proptest::prelude::*;

    fn mask(w: u32) -> u64 {
        if w >= 64 { u64::MAX } else { (1u64 << w) - 1 }
    }

    /// The op's result through the AIG: a one-op design, both operands as inputs.
    fn via_aig(text: &str, a: u64, b: u64) -> u64 {
        let file = parser::parse(text).unwrap();
        let m = AigModel::build(&file).unwrap();
        let (ia, ib) = (&m.inputs[0].leaves, &m.inputs[1].leaves);
        let lv: HashMap<u32, bool> = ia
            .iter()
            .enumerate()
            .map(|(i, l)| (l.node(), (a >> i) & 1 == 1))
            .chain(
                ib.iter()
                    .enumerate()
                    .map(|(i, l)| (l.node(), (b >> i) & 1 == 1)),
            )
            .collect();
        let vals = m.aig.simulate(|n| lv.get(&n).copied().unwrap_or(false));
        let next = m.states[0].next.as_ref().unwrap();
        next.iter()
            .enumerate()
            .map(|(i, &l)| u64::from(m.aig.eval(&vals, l)) << i)
            .sum()
    }

    /// The same design through the concrete evaluator.
    fn via_concrete(text: &str, a: u64, b: u64) -> u64 {
        let file = parser::parse(text).unwrap();
        let regs = HashMap::from([("r".to_string(), 0u128)]);
        let ins = HashMap::from([
            ("a".to_string(), u128::from(a)),
            ("b".to_string(), u128::from(b)),
        ]);
        simulate_one_step(&file, &regs, &ins).unwrap()["r"] as u64
    }

    /// A design computing `r' = op(a, b)` (result width `rw`), the operands `wa` / `wb` wide.
    fn design(op: &str, wa: u32, wb: u32, rw: u32, extra: &str) -> String {
        format!(
            "1 sort bitvec {wa}\n2 sort bitvec {wb}\n3 sort bitvec {rw}\n4 input 1 a\n5 input 2 b\n\
             6 state 3 r\n7 {op} 3 4 5{extra}\n8 next 3 6 7\n"
        )
    }

    const CONCRETE_BINARY: &[&str] = &[
        "and", "or", "xor", "nand", "nor", "xnor", "add", "sub", "mul", "sll", "srl", "sra", "rol",
        "ror",
    ];
    const CONCRETE_COMPARE: &[&str] = &[
        "eq", "neq", "ult", "ulte", "ugt", "ugte", "slt", "slte", "sgt", "sgte",
    ];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        #[test]
        fn binary_ops_match_the_concrete_evaluator(
            w in 1u32..=8, op in 0usize..14, a in any::<u64>(), b in any::<u64>()
        ) {
            let (a, b) = (a & mask(w), b & mask(w));
            let d = design(CONCRETE_BINARY[op], w, w, w, "");
            prop_assert_eq!(via_aig(&d, a, b), via_concrete(&d, a, b), "{} w={} a={} b={}", CONCRETE_BINARY[op], w, a, b);
        }

        #[test]
        fn comparisons_match_the_concrete_evaluator(
            w in 1u32..=8, op in 0usize..10, a in any::<u64>(), b in any::<u64>()
        ) {
            let (a, b) = (a & mask(w), b & mask(w));
            let d = design(CONCRETE_COMPARE[op], w, w, 1, "");
            prop_assert_eq!(via_aig(&d, a, b), via_concrete(&d, a, b), "{} w={} a={} b={}", CONCRETE_COMPARE[op], w, a, b);
        }

        #[test]
        fn structural_ops_match_the_concrete_evaluator(
            wa in 1u32..=6, wb in 1u32..=6, a in any::<u64>(), b in any::<u64>(), pick in 0usize..6
        ) {
            let (a, b) = (a & mask(wa), b & mask(wb));
            // concat; slice / uext / sext / unary of `a` (b unused); ite on a's low bit.
            let d = match pick {
                0 => design("concat", wa, wb, wa + wb, ""),
                1 => format!("1 sort bitvec {wa}\n2 sort bitvec {wb}\n3 sort bitvec 1\n4 input 1 a\n5 input 2 b\n6 state 3 r\n7 slice 3 4 {} {}\n8 next 3 6 7\n", wa - 1, wa - 1),
                2 => format!("1 sort bitvec {wa}\n2 sort bitvec {wb}\n3 sort bitvec {}\n4 input 1 a\n5 input 2 b\n6 state 3 r\n7 uext 3 4 {wb}\n8 next 3 6 7\n", wa + wb),
                3 => format!("1 sort bitvec {wa}\n2 sort bitvec {wb}\n3 sort bitvec {}\n4 input 1 a\n5 input 2 b\n6 state 3 r\n7 sext 3 4 {wb}\n8 next 3 6 7\n", wa + wb),
                4 => format!("1 sort bitvec {wa}\n2 sort bitvec {wb}\n3 sort bitvec 1\n4 input 1 a\n5 input 2 b\n6 state 1 r\n7 slice 3 5 0 0\n8 neg 1 4\n9 ite 1 7 4 8\n10 next 1 6 9\n"),
                _ => format!("1 sort bitvec {wa}\n2 sort bitvec {wb}\n3 sort bitvec 1\n4 input 1 a\n5 input 2 b\n6 state 3 r\n7 redxor 3 4\n8 redand 3 4\n9 redor 3 4\n10 and 3 7 8\n11 xor 3 10 9\n12 next 3 6 11\n"),
            };
            prop_assert_eq!(via_aig(&d, a, b), via_concrete(&d, a, b), "pick {} wa={} wb={} a={} b={}\n{}", pick, wa, wb, a, b, d);
        }

        /// Division, remainder and the overflow predicates have no concrete evaluator; the
        /// reference is SMT-LIB's definition ([`smtlib_reference`]).
        #[test]
        fn division_and_overflow_match_smtlib(
            w in 1u32..=8, op in 0usize..12, a in any::<u64>(), b in any::<u64>()
        ) {
            let (a, b) = (a & mask(w), b & mask(w));
            let name = smtlib_reference::OPS[op];
            let (expect, rw) = smtlib_reference::eval(name, w, a, b);
            let d = design(name, w, w, rw, "");
            prop_assert_eq!(via_aig(&d, a, b), expect, "{} w={} a={} b={}", name, w, a, b);
        }
    }

    #[test]
    fn structural_hashing_shares_gates_and_folds_constants() {
        let mut g = Aig::new();
        let (a, b) = (g.leaf(), g.leaf());
        let x = g.and(a, b);
        assert_eq!(g.and(b, a), x);
        assert_eq!(g.and(a, !a), Lit::FALSE);
        assert_eq!(g.and(a, Lit::TRUE), a);
        assert_eq!(g.len(), 4);
    }

    #[test]
    fn an_array_design_is_refused_with_its_name() {
        let text = "1 sort bitvec 2\n2 sort bitvec 4\n3 sort array 1 2\n4 state 3 mem\n";
        let err = AigModel::build(&parser::parse(text).unwrap()).unwrap_err();
        assert!(err.to_string().contains("`mem` is an array"), "{err}");
    }

    /// mununu#1142 — the build stops at the node cap with the count, names the variable that
    /// raises it, and creates nothing past the cap.
    #[test]
    fn x1142_the_build_stops_at_the_node_cap_and_names_the_count() {
        let text = "1 sort bitvec 8\n2 state 1 a\n3 input 1 b\n4 add 1 2 3\n5 mul 1 4 4\n\
                    6 next 1 2 5\n7 sort bitvec 1\n8 redor 7 5\n9 bad 8\n";
        let file = parser::parse(text).unwrap();
        let full = AigModel::build(&file).unwrap();
        assert!(full.aig.len() > 40, "{}", full.aig.len());
        let limits = BuildLimits {
            node_cap: Some(40),
            ..Default::default()
        };
        match AigModel::build_with(&file, &limits) {
            Err(BuildStop::NodeCap { nodes, cap }) => {
                assert_eq!((nodes, cap), (40, 40));
                let text = BuildStop::NodeCap { nodes, cap }.to_string();
                assert!(text.contains("MUNUNU_AIG_NODE_CAP"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        // Past the cap the graph creates nothing: a direct user sees the trip and FALSE answers.
        let mut aig = Aig::with_limits(&limits);
        let leaves = aig.leaves(60);
        assert_eq!(aig.len(), 40, "nothing past the cap");
        assert!(matches!(aig.tripped(), Some(BuildStop::NodeCap { .. })));
        assert!(leaves[41..].iter().all(|l| *l == Lit::FALSE));
    }

    /// mununu#1142 — a cancel flag already up, or a deadline already past, stops the build at
    /// its first node with the count; a fresh run budget that has expired does the same.
    #[test]
    fn x1142_a_cancelled_or_expired_build_stops_at_once() {
        let text = "1 sort bitvec 4\n2 state 1 a\n3 input 1 b\n4 add 1 2 3\n5 next 1 2 4\n\
                    6 sort bitvec 1\n7 redor 6 4\n8 bad 7\n";
        let file = parser::parse(text).unwrap();
        let cancelled = BuildLimits::member(None, Some(Arc::new(AtomicBool::new(true))));
        assert!(matches!(
            AigModel::build_with(&file, &cancelled),
            Err(BuildStop::Cancelled { nodes: 1 })
        ));
        let late = BuildLimits::member(Some(Instant::now()), None);
        assert!(matches!(
            AigModel::build_with(&file, &late),
            Err(BuildStop::Deadline { nodes: 1 })
        ));
        // A finished model keeps no limit: the graph can grow past the build's cap afterwards.
        let mut done = AigModel::build(&file).expect("builds");
        let before = done.aig.len();
        done.aig.leaves(4);
        assert_eq!(done.aig.len(), before + 4);
        assert!(done.aig.tripped().is_none());
        let expired = crate::adapter::run_budget::Budget::with_ms(0);
        let _g = crate::adapter::run_budget::enter(&expired);
        assert!(matches!(
            AigModel::build(&file),
            Err(BuildStop::RunBudget { nodes: 1 })
        ));
        // Every stop names its count, and the texts are told apart.
        for stop in [
            BuildStop::NodeCap { nodes: 40, cap: 40 },
            BuildStop::NodeCap {
                nodes: 7,
                cap: MAX_NODES,
            },
            BuildStop::Deadline { nodes: 7 },
            BuildStop::RunBudget { nodes: 7 },
            BuildStop::Cancelled { nodes: 7 },
        ] {
            let text = stop.to_string();
            assert!(
                text.starts_with("aig: ") && text.contains(" nodes"),
                "{text}"
            );
        }
        assert!(
            BuildStop::NodeCap { nodes: 40, cap: 40 }
                .to_string()
                .contains("MUNUNU_AIG_NODE_CAP")
        );
        assert!(
            !BuildStop::NodeCap {
                nodes: 7,
                cap: MAX_NODES
            }
            .to_string()
            .contains("MUNUNU_AIG_NODE_CAP"),
            "the hard cap offers no variable"
        );
    }

    /// mununu#1142 — the cap variable: unset is the default, a count is clamped to the literal
    /// space, junk is the default.
    #[test]
    fn the_node_cap_variable_is_a_clamped_count() {
        assert_eq!(node_cap_from(None), DEFAULT_NODE_CAP);
        assert_eq!(node_cap_from(Some("")), DEFAULT_NODE_CAP);
        assert_eq!(node_cap_from(Some("1000")), 1000);
        assert_eq!(node_cap_from(Some("99999999999")), MAX_NODES);
        assert_eq!(node_cap_from(Some("0")), DEFAULT_NODE_CAP);
        assert_eq!(node_cap_from(Some("lots")), DEFAULT_NODE_CAP);
    }
}
