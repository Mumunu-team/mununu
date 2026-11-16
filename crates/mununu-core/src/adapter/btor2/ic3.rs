//! N5.1 (Track N) — the owned bit-level **IC3 / PDR** over the [`super::aig`] model, on CaDiCaL
//! in-process: the first owned member that PROVES.
//!
//! engine: owned IC3/PDR over the N5.0 AIG → CNF (one frame of the transition function, Tseitin),
//! CaDiCaL in-process (`cadical` 0.1.16: `solve_with` + `failed` for assumption cores; no freeze),
//! frames as delta-encoded clause sets under activation literals, counterexamples-to-induction from
//! the SAT model lifted by an assumption core, relative-inductiveness queries, MIC (drop literals
//! while relatively inductive, each candidate checked against `init`), forward propagation,
//! convergence `F_i == F_{i+1}`. Role: a member of the reach portfolio; `Safe` and `Violated` are
//! both checked independently before they ship (below), so a search bug abstains, never lies.
//!
//! # What is decided
//!
//! States are the register bits; inputs are existential at every step. With the file's
//! `constraint`s `C(s, i)` (asserted on the current frame of every query, as the BTOR2 witness
//! semantics asserts them on every frame of a trace):
//! `Init(s) = ∃i. init-equalities(s, i)`, `T(s, s') = ∃i. C(s, i) ∧ s' = next(s, i)`,
//! `Bad(s) = ∃i. C(s, i) ∧ bad(s, i)`. A register with no `next` is free at every step, one with
//! no `init` free at the first.
//!
//! # The two checks
//!
//! - **`Safe`** ships only after the inductive invariant `Inv` (the converged frame's clauses) passes
//!   one direct check in a FRESH solver, independent of the search's solver state:
//!   `Init ∧ ¬Inv`, `Inv ∧ T ∧ ¬Inv'` and `Inv ∧ Bad` all UNSAT. Never through native k-induction's
//!   ladder (its depth cap, mununu#1038, has nothing to do with an invariant).
//! - **`Violated`** ships only after the obligation chain is concretised step by step (each step a
//!   SAT model of `T`) into a trace, and that trace is replayed on the concrete simulator through
//!   the repo's own BTOR2-witness path ([`super::witness::trace_from_witness`]). A trace the
//!   simulator cannot carry (a cell wider than 128 bits) ships with [`Replay::Unavailable`] and its
//!   reason; a trace the simulator REFUSES is a bug, and the engine abstains with it.
//!
//! SOUNDNESS: with constraints, a lifted cube may contain states that admit no constraint-satisfying
//! input (dead states): keeping them in a cube over-approximates, which is sound for blocking; the
//! concretisation re-solves every step with `C` asserted and abstains (`Stopped`, with the step) if a
//! chain does not concretise, so no trace is ever assembled from a cube's guarantee alone.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::adapter::btor2::aig::{AigModel, AigNode, Lit};
use crate::adapter::btor2::ast::Btor2File;
use crate::adapter::btor2::native_bmc::BmcTrace;
use crate::adapter::btor2::sat_bmc::{btor2_witness_with_free, cell_name};
use crate::adapter::btor2::witness::{parse_btor2_witness, trace_from_witness};

/// How long the search may run.
#[derive(Clone, Debug, Default)]
pub struct Ic3Options {
    /// A wall-clock deadline for the whole search (and the certificate check).
    pub deadline: Option<Instant>,
    /// An external cancellation, polled by the solver.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Stop after this many frames (`None` = unbounded). A diagnostic bound, not a verdict.
    pub max_frames: Option<u32>,
}

/// Whether a `Violated` trace was replayed on the concrete simulator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Replay {
    /// The simulator reached the falsified `bad` on the trace — carried as the replayed trace
    /// itself (N5.4: the portfolio ships it, so no witness is re-derived).
    Confirmed(BmcTrace),
    /// The simulator cannot carry this design (the reason says why); the trace is the SAT
    /// concretisation alone, the same trust `sat-bmc`'s traces have.
    Unavailable(String),
}

/// What the search established.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ic3Outcome {
    /// No `bad` state is reachable: an inductive invariant of `clauses` clauses, found at frame
    /// `frames` and checked directly.
    Safe { frames: u32, clauses: usize },
    /// `bad` number `bad_index` holds at `depth` on a trace that meets every constraint;
    /// `initial` / `inputs` are keyed as the concrete simulator keys them (the symbol, else
    /// `st_n<nid>` / `in_n<nid>`), one input frame per step `0 ..= depth`.
    Violated {
        depth: u32,
        bad_index: usize,
        initial: Vec<(String, u128)>,
        inputs: Vec<Vec<(String, u128)>>,
        /// The values the registers WITHOUT `next` take at frame `k ≥ 1` (`free_states[k - 1]`,
        /// model order): a free register's step is the model's choice, so the trace carries it and
        /// the replay follows it. Empty when every register has a `next`.
        free_states: Vec<Vec<(String, u128)>>,
        replay: Replay,
    },
    /// The search stopped (the deadline, a cancellation, the frame bound, or a check that did not
    /// pass) at `frames` frames; `reason` says which.
    Stopped { frames: u32, reason: String },
}

struct Stop {
    deadline: Option<Instant>,
    cancel: Option<Arc<AtomicBool>>,
}

impl Stop {
    fn halted(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
            || self.deadline.is_some_and(|d| Instant::now() >= d)
    }

    fn reason(&self, otherwise: &str) -> String {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            "cancelled".to_string()
        } else if self.deadline.is_some_and(|d| Instant::now() >= d) {
            "deadline reached".to_string()
        } else {
            otherwise.to_string()
        }
    }

    fn clone_spec(&self) -> Stop {
        Stop {
            deadline: self.deadline,
            cancel: self.cancel.clone(),
        }
    }
}

impl cadical::Callbacks for Stop {
    fn terminate(&mut self) -> bool {
        self.halted()
    }
}

/// The solver stopped before answering (the deadline or a cancellation) — propagated as an error
/// so every query site unwinds to one `Stopped`.
#[derive(Debug)]
struct Halted;

/// One frame of the delta encoding: the clauses learned AT this level (they hold in `F_1 ..= F_i`)
/// and the activation literal they are added under.
struct Frame {
    act: i32,
    clauses: Vec<Vec<i32>>,
}

/// A proof obligation: block `cube` at `level`; `parent` is the obligation whose cube this one's
/// states step into (the chain a counterexample is read off).
struct Obligation {
    cube: Vec<i32>,
    level: usize,
    parent: Option<usize>,
}

/// A SAT answer with the model's state, input and free-next assignment captured before anything
/// else touches the solver (a later `add_clause` invalidates the model).
struct Witness {
    state: Vec<i32>,
    inputs: Vec<i32>,
    free_next: Vec<i32>,
}

enum Rel {
    /// `¬cube` is inductive relative to the frame; the cube literals whose primed assumption the
    /// solver used (a subset of the cube that is still inductive).
    Inductive(Vec<i32>),
    /// A predecessor in the frame steps into the cube.
    Predecessor(Witness),
}

/// The Tseitin encoder of one frame of the AIG: every node's SAT literal, the clauses it needed.
struct Encoder<'m> {
    model: &'m AigModel,
    /// `enc[node]` = the SAT variable of the node (0 = not yet encoded; node 0 is FALSE = `-1`).
    enc: Vec<i32>,
    next_var: i32,
    clauses: Vec<Vec<i32>>,
}

impl<'m> Encoder<'m> {
    fn fresh(&mut self) -> i32 {
        let v = self.next_var;
        self.next_var += 1;
        v
    }

    fn lit(&self, l: Lit) -> i32 {
        let v = self.enc[l.node() as usize];
        if l.is_complemented() { -v } else { v }
    }

    fn encode(&mut self, l: Lit) -> i32 {
        self.encode_node(l.node());
        self.lit(l)
    }

    /// Iterative (an explicit stack): a deep multiplier chain must not overflow the stack.
    fn encode_node(&mut self, root: u32) {
        let mut stack = vec![root];
        while let Some(&n) = stack.last() {
            if self.enc[n as usize] != 0 {
                stack.pop();
                continue;
            }
            let value = match self.model.aig.node(n) {
                None | Some(AigNode::False) => Some(-1),
                // SOUNDNESS: a leaf that is neither a state nor an input bit (the model creates
                // none; defensive) becomes a free variable — an over-approximation of T: a proof
                // over it still transfers, and a trace is replayed concretely before it ships.
                Some(AigNode::Leaf) => Some(self.fresh()),
                Some(AigNode::And(a, b)) => {
                    let (av, bv) = (self.enc[a.node() as usize], self.enc[b.node() as usize]);
                    if av == 0 || bv == 0 {
                        if av == 0 {
                            stack.push(a.node());
                        }
                        if bv == 0 {
                            stack.push(b.node());
                        }
                        None
                    } else {
                        let (x, y) = (self.lit(a), self.lit(b));
                        let v = self.fresh();
                        self.clauses.push(vec![-v, x]);
                        self.clauses.push(vec![-v, y]);
                        self.clauses.push(vec![v, -x, -y]);
                        Some(v)
                    }
                }
            };
            if let Some(v) = value {
                self.enc[n as usize] = v;
                stack.pop();
            }
        }
    }
}

/// The IC3 search over one design.
struct Ic3<'m> {
    model: &'m AigModel,
    solver: cadical::Solver<Stop>,
    stop: Stop,
    next_var: i32,
    /// Every register bit's current-frame variable, contiguous from 2 (so `|lit| - 2` is the bit),
    /// in model order: state 0's bits LSB first, then state 1's, …
    cur: Vec<i32>,
    /// Every input bit's variable.
    inputs: Vec<i32>,
    /// Every register bit's next-state literal (a fresh variable for a register with no `next`).
    nxt: Vec<i32>,
    /// The fresh variables among `nxt`.
    free_next: Vec<i32>,
    /// `bad` (the disjunction of every `bad`), and each `bad` on its own.
    bad: i32,
    bad_lits: Vec<i32>,
    act_init: i32,
    /// `T`'s clauses (the Tseitin definitions, the constraints as units, the `bad` disjunction).
    trans: Vec<Vec<i32>>,
    /// The `init` equalities, each already carrying `¬act_init`.
    init_clauses: Vec<Vec<i32>>,
    /// `frames[0]` stands for `Init` (its clause list stays empty); the search's frames follow.
    frames: Vec<Frame>,
    /// Clauses in the solver that no longer matter (released temporaries, moved frame clauses);
    /// the solver is rebuilt without them when they outnumber the live ones.
    dead: usize,
    seq: u64,
    queries: u64,
    /// N5.2 — the phase the next SAT query is charged to ([`Phase`]); a diagnostic that never
    /// changes the search.
    phase: Phase,
    /// Per phase: the queries made and the time spent inside the solver (nanoseconds).
    by_phase: [(u64, u64); Phase::COUNT],
    /// N5.2 — a cube literal's activity: how many learned clauses kept it (as `¬l`). MIC tries to
    /// drop the rarest literals first (IC3ref's `litOrder`), so an early success's core removes
    /// many at once.
    activity: HashMap<i32, u32>,
    /// N5.2 — rIC3-style activity (`MUNUNU_IC3_ACT=decay`): per VARIABLE, bumped by a growing
    /// increment on every learned clause (recent lemmas weigh more), rescaled near overflow.
    act_var: HashMap<i32, f64>,
    act_inc: f64,
    act_decay: bool,
    /// N5.2 — the widths profile: lifted CTIs (literals, count) and learned clauses (literals,
    /// count).
    cti_lits: u64,
    ctis: u64,
    learned_lits: u64,
    learned: u64,
    /// N5.2 — `down`'s joins taken, and the drops it gave up on at the join cap.
    down_joins: u64,
    down_capped: u64,
    /// N5.2 — which MIC runs (`MUNUNU_IC3_MIC`, a diagnostic for the A/B profile on one build).
    mic: Mic,
    /// N5.2 — how a predecessor state is lifted (`MUNUNU_IC3_LIFT`, the same diagnostic).
    lift_mode: Lift,
    /// N5.2 — a seed for the MIC order's tie-break (`MUNUNU_IC3_SEED`; 0 = the literal itself):
    /// the "one lucky order" falsifier of a path-dependent proof.
    seed: u64,
    /// N5.2 — the lifted-width profile split by obligation kind: root lifts (from `bad`) and
    /// descendant lifts (into an obligation cube), plus the descendant lifts' target widths.
    root_lits: u64,
    roots: u64,
    desc_lits: u64,
    descs: u64,
    target_lits: u64,
    /// N5.2 — how many of the queries were `initial` checks (a syntactic check could replace
    /// them when every `init` is a constant).
    init_queries: u64,
    /// … and the solver time they took (nanoseconds); the same for `repair_initiation`.
    init_ns: u64,
    repair_queries: u64,
    repair_ns: u64,
    /// N5.2 — the width of the cube entering MIC's drop loop (after the relative core and the
    /// initiation repair, before any drop), summed, and the count.
    mic_in_lits: u64,
    mic_ins: u64,
    /// N5.2 — the ternary X-out probe (`MUNUNU_IC3_PROBE=<lifts per kind>`, 0 = off): on the first
    /// lifts of each kind, how many state literals an offline ternary simulation can leave at X
    /// while the target keeps its value — from the full state, and from the SAT-lifted cube.
    probe: Probe,
}

/// N5.2 — the ternary X-out probe's budget and tallies (see [`Ic3::probe`]).
#[derive(Default)]
struct Probe {
    /// Lifts of each kind still to probe.
    left_root: u32,
    left_desc: u32,
    /// Per kind: probes run, SAT-lifted width, ternary width from the full state, ternary width
    /// from the SAT-lifted cube, and how many SAT-lifted cubes ternary simulation could not
    /// justify (its X-propagation is weaker than a SAT core).
    root: ProbeTally,
    desc: ProbeTally,
}

#[derive(Default)]
struct ProbeTally {
    n: u64,
    sat_lits: u64,
    tern_lits: u64,
    tern_from_sat_lits: u64,
    sat_unjustified: u64,
}

impl ProbeTally {
    fn summary(&self, kind: &str) -> String {
        let avg = |lits: u64| if self.n == 0 { 0.0 } else { lits as f64 / self.n as f64 };
        format!(
            "probe_{kind}: n={} sat_w={:.1} tern_w={:.1} tern_from_sat_w={:.1} sat_unjustified={}",
            self.n,
            avg(self.sat_lits),
            avg(self.tern_lits),
            avg(self.tern_from_sat_lits),
            self.sat_unjustified
        )
    }
}

/// A ternary value: `0` false, `1` true, `2` unknown (X).
const TX: u8 = 2;

/// N5.2 — the MIC variants under measurement (`MUNUNU_IC3_MIC=legacy|order|join`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mic {
    /// Before N5.2: one pass in the cube's order, a failed drop keeps the literal.
    Legacy,
    /// Activity order (rarest literals first), each literal tried once, no join.
    Order,
    /// `Order` plus Bradley's `down` join on a failed drop, up to [`DOWN_JOINS`] joins.
    Join,
}

/// N5.2 — the lifting variants under measurement (`MUNUNU_IC3_LIFT=core|iter|first`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lift {
    /// The failed-assumption core of one query (state, inputs, free next bits assumed in that
    /// order).
    Core,
    /// `Core`, then the query re-solved on the core alone until it stops shrinking (at most
    /// [`LIFT_ROUNDS`] rounds): a solver's core is rarely minimal on the first pass.
    Iter,
    /// `Iter` with the inputs and free next bits assumed BEFORE the state literals, so the
    /// conflict is attributed to as few state literals as the decision order allows.
    IterFirst,
}

/// N5.2 — the most re-solves [`Lift::Iter`] / [`Lift::IterFirst`] spend shrinking one core.
const LIFT_ROUNDS: usize = 3;

/// N5.2 — how many times a failed drop may be joined with its predecessor before the literal is
/// given up (Bradley's `down`, without CTGs): each join weakens the candidate towards a cube that
/// is relatively inductive; a join that admits an initial state fails the drop.
const DOWN_JOINS: usize = 3;

/// N5.2 — where the search's SAT queries go, for the per-phase profile in the `ic3 search` log
/// line (`RUST_LOG=mununu_core::adapter::btor2::ic3=debug`) and the manual profile test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// The obligation loop's own queries: relative inductiveness of a CTI, `initial` checks.
    Block,
    /// MIC: the literal-drop queries.
    Generalize,
    /// Pushing a learned clause to the highest frame it is inductive at.
    AddBlocked,
    /// Forward propagation of every frame's clauses.
    Propagate,
    /// Shrinking a predecessor state to the literals that force the step.
    Lift,
    /// The certificate check and the top-level `bad_state` queries.
    Other,
}

impl Phase {
    const COUNT: usize = 6;
    const ALL: [Phase; Phase::COUNT] = [
        Phase::Block,
        Phase::Generalize,
        Phase::AddBlocked,
        Phase::Propagate,
        Phase::Lift,
        Phase::Other,
    ];
    fn name(self) -> &'static str {
        match self {
            Phase::Block => "block",
            Phase::Generalize => "generalize",
            Phase::AddBlocked => "add_blocked",
            Phase::Propagate => "propagate",
            Phase::Lift => "lift",
            Phase::Other => "other",
        }
    }
}

impl<'m> Ic3<'m> {
    /// Run `f` with the queries charged to `phase`, then charge the caller's phase again.
    fn in_phase<T>(&mut self, phase: Phase, f: impl FnOnce(&mut Self) -> T) -> T {
        let outer = self.phase;
        self.phase = phase;
        let r = f(self);
        self.phase = outer;
        r
    }

    /// `phase: queries / solver ms` for every phase, then the average lifted-CTI and learned-clause
    /// widths, in one string for the log line.
    fn phase_summary(&self) -> String {
        let avg = |lits: u64, n: u64| if n == 0 { 0.0 } else { lits as f64 / n as f64 };
        let mut parts: Vec<String> = Phase::ALL
            .iter()
            .map(|&p| {
                let (q, ns) = self.by_phase[p as usize];
                format!("{}={}q/{}ms", p.name(), q, ns / 1_000_000)
            })
            .collect();
        parts.push(format!(
            "latches={} trans_clauses={} vars={} init_queries={} init_ms={} repair_queries={} repair_ms={} mic_in_width={:.1}/{} cti_width={:.1} root_width={:.1}/{} desc_width={:.1}/{} target_width={:.1} clause_width={:.1} down_joins={} down_capped={} mic={:?} lift={:?} seed={} act={}",
            self.cur.len(),
            self.trans.len(),
            self.next_var,
            self.init_queries,
            self.init_ns / 1_000_000,
            self.repair_queries,
            self.repair_ns / 1_000_000,
            avg(self.mic_in_lits, self.mic_ins),
            self.mic_ins,
            avg(self.cti_lits, self.ctis),
            avg(self.root_lits, self.roots),
            self.roots,
            avg(self.desc_lits, self.descs),
            self.descs,
            avg(self.target_lits, self.descs),
            avg(self.learned_lits, self.learned),
            self.down_joins,
            self.down_capped,
            self.mic,
            self.lift_mode,
            self.seed,
            if self.act_decay { "decay" } else { "count" }
        ));
        if self.probe.root.n + self.probe.desc.n > 0 {
            parts.push(self.probe.root.summary("root"));
            parts.push(self.probe.desc.summary("desc"));
        }
        parts.join(" ")
    }

    /// N5.2 — the MIC order's tie-break: the literal itself, or a seeded hash of it.
    fn tie(&self, lit: i32) -> u64 {
        if self.seed == 0 {
            return lit.unsigned_abs() as u64 * 2 + u64::from(lit < 0);
        }
        // splitmix64 of (lit, seed): a fixed, seed-dependent permutation of the literals.
        let mut z = (lit as i64 as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(self.seed);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// N5.2 — the ternary X-out probe on one lift (a diagnostic: it never changes the search).
    /// Simulates one step of the AIG over `{0, 1, X}` under the witness's concrete inputs and free
    /// next bits, and greedily sets state bits to X while the target (`succ'`, or `bad`) keeps its
    /// value — once from the full state, once from the SAT-lifted cube `lifted`.
    fn probe_lift(&mut self, w: &Witness, succ: Option<&[i32]>, lifted: &[i32]) {
        let tally_left = match succ {
            Some(_) => &mut self.probe.left_desc,
            None => &mut self.probe.left_root,
        };
        if *tally_left == 0 {
            return;
        }
        *tally_left -= 1;
        let model = self.model;
        let n = model.aig.len();
        // Leaf node → value, for the inputs (fixed for the whole probe).
        let mut base = vec![TX; n];
        let mut flat = 0usize;
        for i in &model.inputs {
            for l in &i.leaves {
                let v = w.inputs.get(flat).copied().unwrap_or(0);
                base[l.node() as usize] = u8::from(v > 0);
                flat += 1;
            }
        }
        // Flat state bit → its leaf node and its next literal (`None`: free or fresh).
        let mut state_leaf = Vec::with_capacity(self.cur.len());
        let mut state_next: Vec<Option<Lit>> = Vec::with_capacity(self.cur.len());
        for s in &model.states {
            for (bi, l) in s.leaves.iter().enumerate() {
                state_leaf.push(l.node() as usize);
                state_next.push(s.next.as_ref().and_then(|nx| nx.get(bi).copied()));
            }
        }
        // The target: next literals that must evaluate to a value (`None` = free, always met).
        let targets: Vec<(Option<Lit>, bool)> = match succ {
            Some(c) => c
                .iter()
                .map(|&l| {
                    let bit = (l.unsigned_abs() - 2) as usize;
                    (state_next.get(bit).copied().flatten(), l > 0)
                })
                .collect(),
            None => Vec::new(),
        };
        let sim = |vals: &mut Vec<u8>| {
            for i in 0..n {
                if let Some(AigNode::And(a, b)) = model.aig.node(i as u32) {
                    let va = vals[a.node() as usize];
                    let vb = vals[b.node() as usize];
                    let va = if va == TX { TX } else { va ^ u8::from(a.is_complemented()) };
                    let vb = if vb == TX { TX } else { vb ^ u8::from(b.is_complemented()) };
                    vals[i] = if va == 0 || vb == 0 {
                        0
                    } else if va == 1 && vb == 1 {
                        1
                    } else {
                        TX
                    };
                }
            }
            vals[0] = 0;
        };
        let tval = |vals: &[u8], l: Lit| -> u8 {
            let v = vals[l.node() as usize];
            if v == TX { TX } else { v ^ u8::from(l.is_complemented()) }
        };
        let holds = |vals: &[u8]| -> bool {
            match succ {
                Some(_) => targets
                    .iter()
                    .all(|&(nx, want)| nx.is_none_or(|nx| tval(vals, nx) == u8::from(want))),
                None => model.bads.iter().any(|&b| tval(vals, b) == 1),
            }
        };
        // Greedy X-out from a starting assignment of the state bits (`Some(v)` concrete, `None` X),
        // trying only the bits in `order`; returns the number left concrete, or `None` when the
        // start itself does not hold.
        let xout = |start: &[Option<bool>], order: &[usize]| -> Option<usize> {
            let mut vals = base.clone();
            let mut cur: Vec<Option<bool>> = start.to_vec();
            let set = |vals: &mut Vec<u8>, cur: &[Option<bool>]| {
                for (bit, &leaf) in state_leaf.iter().enumerate() {
                    vals[leaf] = match cur[bit] {
                        Some(v) => u8::from(v),
                        None => TX,
                    };
                }
            };
            set(&mut vals, &cur);
            sim(&mut vals);
            if !holds(&vals) {
                return None;
            }
            for &bit in order {
                if cur[bit].is_none() {
                    continue;
                }
                let saved = cur[bit];
                cur[bit] = None;
                set(&mut vals, &cur);
                sim(&mut vals);
                if !holds(&vals) {
                    cur[bit] = saved;
                }
            }
            Some(cur.iter().filter(|v| v.is_some()).count())
        };
        let full: Vec<Option<bool>> = w.state.iter().map(|&l| Some(l > 0)).collect();
        let all: Vec<usize> = (0..self.cur.len()).collect();
        let tern_w = xout(&full, &all).unwrap_or(self.cur.len());
        let in_lifted: HashSet<usize> = lifted
            .iter()
            .map(|&l| (l.unsigned_abs() - 2) as usize)
            .collect();
        let from_sat: Vec<Option<bool>> = full
            .iter()
            .enumerate()
            .map(|(bit, &v)| if in_lifted.contains(&bit) { v } else { None })
            .collect();
        let lifted_bits: Vec<usize> = in_lifted.iter().copied().collect();
        let from_sat_w = xout(&from_sat, &lifted_bits);
        let tally = match succ {
            Some(_) => &mut self.probe.desc,
            None => &mut self.probe.root,
        };
        tally.n += 1;
        tally.sat_lits += lifted.len() as u64;
        tally.tern_lits += tern_w as u64;
        match from_sat_w {
            Some(wd) => tally.tern_from_sat_lits += wd as u64,
            None => {
                tally.sat_unjustified += 1;
                tally.tern_from_sat_lits += lifted.len() as u64;
            }
        }
    }

    /// N5.2 — a cube literal's activity (see the field).
    fn activity(&self, lit: i32) -> u32 {
        self.activity.get(&lit).copied().unwrap_or(0)
    }

    /// N5.2 — the MIC sort key: the literal's count activity, or under `MUNUNU_IC3_ACT=decay` the
    /// variable's decayed activity quantised to an integer (the order is all that matters).
    fn mic_key(&self, lit: i32) -> (u64, u64) {
        let a = if self.act_decay {
            let v = self.act_var.get(&lit.abs()).copied().unwrap_or(0.0);
            // Ascending in activity; the scale keeps distinct values distinct after quantising.
            (v / self.act_inc * 1e6) as u64
        } else {
            u64::from(self.activity(lit))
        };
        (a, self.tie(lit))
    }

    fn new(model: &'m AigModel, stop: Stop) -> Self {
        let mut enc = Encoder {
            model,
            enc: vec![0; model.aig.len().max(1)],
            next_var: 2,
            clauses: Vec::new(),
        };
        enc.enc[0] = -1;
        let mut cur = Vec::new();
        // `first[si]` = the flat index of state `si`'s bit 0.
        let mut first = Vec::with_capacity(model.states.len());
        for s in &model.states {
            first.push(cur.len());
            for l in &s.leaves {
                let v = enc.fresh();
                enc.enc[l.node() as usize] = v;
                cur.push(v);
            }
        }
        let mut inputs = Vec::new();
        for i in &model.inputs {
            for l in &i.leaves {
                let v = enc.fresh();
                enc.enc[l.node() as usize] = v;
                inputs.push(v);
            }
        }
        let mut nxt = Vec::with_capacity(cur.len());
        let mut free_next = Vec::new();
        for s in &model.states {
            for bi in 0..s.leaves.len() {
                match s.next.as_ref() {
                    // A register without `next` is free at every step; its value at the next
                    // frame is one of the model's choices, carried into the trace (`free_words`).
                    None => {
                        let v = enc.fresh();
                        free_next.push(v);
                        nxt.push(v);
                    }
                    Some(n) => match n.get(bi) {
                        Some(&l) => nxt.push(enc.encode(l)),
                        // SOUNDNESS: a `next` narrower than its register leaves the high bits
                        // unconstrained — fresh, and not carried by `free_words` — an
                        // over-approximation of the transition; a proof over it is a proof, and
                        // every trace is replayed before it ships.
                        None => nxt.push(enc.fresh()),
                    },
                }
            }
        }
        let act_init = enc.fresh();
        let mut init_clauses = Vec::new();
        for (si, s) in model.states.iter().enumerate() {
            if let Some(init) = &s.init {
                for (bi, &il) in init.iter().enumerate() {
                    let iv = enc.encode(il);
                    if let Some(&c) = cur.get(first[si] + bi) {
                        init_clauses.push(vec![-act_init, -c, iv]);
                        init_clauses.push(vec![-act_init, c, -iv]);
                    }
                }
            }
        }
        for &c in &model.constraints {
            let l = enc.encode(c);
            enc.clauses.push(vec![l]);
        }
        let bad_lits: Vec<i32> = model.bads.iter().map(|&b| enc.encode(b)).collect();
        let bad = match bad_lits.as_slice() {
            [one] => *one,
            many => {
                let v = enc.fresh();
                let mut cl = vec![-v];
                cl.extend(many.iter().copied());
                enc.clauses.push(cl);
                for &b in many {
                    enc.clauses.push(vec![v, -b]);
                }
                v
            }
        };
        let mut me = Self {
            model,
            solver: cadical::Solver::new(),
            stop,
            next_var: enc.next_var,
            cur,
            inputs,
            nxt,
            free_next,
            bad,
            bad_lits,
            act_init,
            trans: enc.clauses,
            init_clauses,
            frames: vec![Frame {
                act: act_init,
                clauses: Vec::new(),
            }],
            dead: 0,
            seq: 0,
            queries: 0,
            phase: Phase::Other,
            by_phase: [(0, 0); Phase::COUNT],
            activity: HashMap::new(),
            act_var: HashMap::new(),
            act_inc: 1.0,
            act_decay: matches!(
                crate::adapter::env_var::read("MUNUNU_IC3_ACT").as_deref().map(str::trim),
                Some("decay")
            ),
            cti_lits: 0,
            ctis: 0,
            learned_lits: 0,
            learned: 0,
            down_joins: 0,
            down_capped: 0,
            mic: match crate::adapter::env_var::read("MUNUNU_IC3_MIC")
                .as_deref()
                .map(str::trim)
            {
                Some("legacy") => Mic::Legacy,
                Some("order") => Mic::Order,
                _ => Mic::Join,
            },
            lift_mode: match crate::adapter::env_var::read("MUNUNU_IC3_LIFT")
                .as_deref()
                .map(str::trim)
            {
                Some("iter") => Lift::Iter,
                Some("first") => Lift::IterFirst,
                _ => Lift::Core,
            },
            seed: crate::adapter::env_var::read("MUNUNU_IC3_SEED")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0),
            root_lits: 0,
            roots: 0,
            desc_lits: 0,
            descs: 0,
            target_lits: 0,
            init_queries: 0,
            init_ns: 0,
            repair_queries: 0,
            repair_ns: 0,
            mic_in_lits: 0,
            mic_ins: 0,
            probe: {
                let per_kind: u32 = crate::adapter::env_var::read("MUNUNU_IC3_PROBE")
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);
                Probe {
                    left_root: per_kind,
                    left_desc: per_kind,
                    ..Probe::default()
                }
            },
        };
        me.rebuild();
        me
    }

    /// A fresh solver holding `T`, the `init` equalities and every live frame clause.
    fn rebuild(&mut self) {
        let mut solver: cadical::Solver<Stop> = cadical::Solver::new();
        solver.set_callbacks(Some(self.stop.clone_spec()));
        solver.add_clause([1]);
        for cl in &self.trans {
            solver.add_clause(cl.iter().copied());
        }
        for cl in &self.init_clauses {
            solver.add_clause(cl.iter().copied());
        }
        for f in self.frames.iter().skip(1) {
            for cl in &f.clauses {
                solver.add_clause(cl.iter().copied().chain(std::iter::once(-f.act)));
            }
        }
        self.solver = solver;
        self.dead = 0;
    }

    fn live_clauses(&self) -> usize {
        self.frames.iter().map(|f| f.clauses.len()).sum()
    }

    fn maybe_rebuild(&mut self) {
        if self.dead > 2 * self.live_clauses() + 4_000 {
            self.rebuild();
        }
    }

    fn fresh(&mut self) -> i32 {
        let v = self.next_var;
        self.next_var += 1;
        v
    }

    /// The top frame's index (`k`).
    fn k(&self) -> usize {
        self.frames.len() - 1
    }

    fn solve(&mut self, assumptions: &[i32]) -> Result<bool, Halted> {
        self.queries += 1;
        let t0 = Instant::now();
        let r = self.solver.solve_with(assumptions.iter().copied());
        let slot = &mut self.by_phase[self.phase as usize];
        slot.0 += 1;
        slot.1 += u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        match r {
            Some(sat) => Ok(sat),
            None => Err(Halted),
        }
    }

    fn is_true(&self, lit: i32) -> bool {
        self.solver.value(lit) == Some(true)
    }

    /// A temporary clause under a fresh activation literal; [`Self::release`] retires it.
    fn temp_clause(&mut self, lits: impl IntoIterator<Item = i32>) -> i32 {
        let t = self.fresh();
        self.solver.add_clause(std::iter::once(-t).chain(lits));
        t
    }

    fn release(&mut self, t: i32) {
        self.solver.add_clause([-t]);
        self.dead += 1;
    }

    /// The primed copy of a current-frame state literal.
    fn prime(&self, lit: i32) -> i32 {
        let n = self.nxt[(lit.unsigned_abs() - 2) as usize];
        if lit > 0 { n } else { -n }
    }

    /// Assume `F_level`: `Init` at level 0, every clause learned at this level or above otherwise.
    fn assume_frame(&self, level: usize, out: &mut Vec<i32>) {
        if level == 0 {
            out.push(self.act_init);
        }
        for f in &self.frames[level.max(1)..] {
            out.push(f.act);
        }
    }

    /// The model's full assignment of the state bits, inputs and free next bits.
    fn witness(&self) -> Witness {
        let assign = |vars: &[i32]| -> Vec<i32> {
            vars.iter()
                .map(|&v| if self.is_true(v) { v } else { -v })
                .collect()
        };
        Witness {
            state: assign(&self.cur),
            inputs: assign(&self.inputs),
            free_next: assign(&self.free_next),
        }
    }

    /// Does `cube` contain an initial state?
    fn initial(&mut self, cube: &[i32]) -> Result<bool, Halted> {
        self.init_queries += 1;
        let mut a = vec![self.act_init];
        a.extend_from_slice(cube);
        let t0 = Instant::now();
        let r = self.solve(&a);
        self.init_ns += u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        r
    }

    /// `F_level ∧ ¬cube ∧ T ∧ cube'`.
    fn relative(&mut self, level: usize, cube: &[i32]) -> Result<Rel, Halted> {
        let t = self.temp_clause(cube.iter().map(|&l| -l));
        let mut a = vec![t];
        self.assume_frame(level, &mut a);
        let primed: Vec<i32> = cube.iter().map(|&l| self.prime(l)).collect();
        a.extend_from_slice(&primed);
        let r = match self.solve(&a) {
            Ok(false) => {
                let core = cube
                    .iter()
                    .zip(&primed)
                    .filter(|&(_, &p)| self.solver.failed(p))
                    .map(|(&l, _)| l)
                    .collect();
                Ok(Rel::Inductive(core))
            }
            Ok(true) => Ok(Rel::Predecessor(self.witness())),
            Err(h) => Err(h),
        };
        self.release(t);
        r
    }

    /// Shrink a model's state to the literals that, with its inputs and free next bits, force the
    /// step into `succ` (`None`: force `bad`): the failed assumptions of the negated target.
    fn lift(&mut self, w: &Witness, succ: Option<&[i32]>) -> Result<Vec<i32>, Halted> {
        self.in_phase(Phase::Lift, |me| me.lift_in(w, succ))
    }

    fn lift_in(&mut self, w: &Witness, succ: Option<&[i32]>) -> Result<Vec<i32>, Halted> {
        let t = match succ {
            Some(c) => {
                let neg: Vec<i32> = c.iter().map(|&l| -self.prime(l)).collect();
                self.temp_clause(neg)
            }
            None => self.temp_clause([-self.bad]),
        };
        // The fixed part of every lift query: the target's negation, the inputs and (for a step
        // into `succ`) the free next bits; the state literals go before or after it by mode.
        let mut fixed = vec![t];
        fixed.extend_from_slice(&w.inputs);
        if succ.is_some() {
            fixed.extend_from_slice(&w.free_next);
        }
        let assumptions = |me: &Self, state: &[i32]| -> Vec<i32> {
            let mut a = Vec::with_capacity(fixed.len() + state.len());
            match me.lift_mode {
                Lift::IterFirst => {
                    a.extend_from_slice(&fixed);
                    a.extend_from_slice(state);
                }
                Lift::Core | Lift::Iter => {
                    a.push(fixed[0]);
                    a.extend_from_slice(state);
                    a.extend_from_slice(&fixed[1..]);
                }
            }
            a
        };
        let rounds = match self.lift_mode {
            Lift::Core => 0,
            Lift::Iter | Lift::IterFirst => LIFT_ROUNDS,
        };
        let r = (|| {
            let mut core = w.state.clone();
            for round in 0..=rounds {
                let a = assumptions(self, &core);
                if self.solve(&a)? {
                    // Cannot happen on the first round (the assignment is a model that steps
                    // there), and a shrunk core is a subset of a core: keep what we have.
                    break;
                }
                let smaller: Vec<i32> = core
                    .iter()
                    .copied()
                    .filter(|&l| self.solver.failed(l))
                    .collect();
                let shrank = smaller.len() < core.len();
                core = smaller;
                if !shrank || round == rounds {
                    break;
                }
            }
            Ok::<Vec<i32>, Halted>(core)
        })();
        let r = r.map(|lifted| {
            self.cti_lits += lifted.len() as u64;
            self.ctis += 1;
            match succ {
                Some(c) => {
                    self.desc_lits += lifted.len() as u64;
                    self.descs += 1;
                    self.target_lits += c.len() as u64;
                }
                None => {
                    self.root_lits += lifted.len() as u64;
                    self.roots += 1;
                }
            }
            self.probe_lift(w, succ, &lifted);
            lifted
        });
        self.release(t);
        r
    }

    /// A state of `F_k` that satisfies `Bad`, lifted; `None` when there is none.
    fn bad_state(&mut self) -> Result<Option<Vec<i32>>, Halted> {
        let mut a = vec![self.bad];
        self.assume_frame(self.k(), &mut a);
        if !self.solve(&a)? {
            return Ok(None);
        }
        let w = self.witness();
        self.lift(&w, None).map(Some)
    }

    /// Is `¬cube` already implied by a clause of `F_level` (syntactically: a clause whose literals
    /// are all negations of the cube's)?
    fn blocked(&self, cube: &[i32], level: usize) -> bool {
        self.frames[level.max(1)..]
            .iter()
            .flat_map(|f| f.clauses.iter())
            .any(|cl| cl.iter().all(|&l| cube.contains(&-l)))
    }

    /// `core ⊆ cube` with `Init ∧ core` UNSAT: literals of `cube` are added back until it is.
    fn repair_initiation(&mut self, core: Vec<i32>, cube: &[i32]) -> Result<Vec<i32>, Halted> {
        let (q0, t0) = (self.queries, Instant::now());
        let r = self.repair_initiation_in(core, cube);
        self.repair_queries += self.queries - q0;
        self.repair_ns += u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        r
    }

    fn repair_initiation_in(
        &mut self,
        mut core: Vec<i32>,
        cube: &[i32],
    ) -> Result<Vec<i32>, Halted> {
        if !self.initial(&core)? {
            return Ok(core);
        }
        for &l in cube {
            if core.contains(&l) {
                continue;
            }
            core.push(l);
            if !self.initial(&core)? {
                return Ok(core);
            }
        }
        Ok(core)
    }

    /// MIC: drop literals while `¬g` stays inductive relative to `F_{level-1}` and `g` stays clear
    /// of `Init`. N5.2: the rarest literals (by [`Self::activity`]) are tried first, each literal
    /// at most once, the UNSAT core of every success shrinks the cube further, and a failed drop
    /// is joined with its predecessor ([`Self::down`]) before the literal is given up. Measured
    /// (frogs.5, 120 s): the earlier one-pass MIC made ~75 queries per learned clause, 93 % of
    /// the search's queries.
    fn generalize(
        &mut self,
        core: Vec<i32>,
        cube: &[i32],
        level: usize,
    ) -> Result<Vec<i32>, Halted> {
        self.in_phase(Phase::Generalize, |me| me.generalize_in(core, cube, level))
    }

    fn generalize_in(
        &mut self,
        core: Vec<i32>,
        cube: &[i32],
        level: usize,
    ) -> Result<Vec<i32>, Halted> {
        let mut g = self.repair_initiation(core, cube)?;
        self.mic_in_lits += g.len() as u64;
        self.mic_ins += 1;
        if self.mic == Mic::Legacy {
            return self.generalize_legacy(g, level);
        }
        let joins = if self.mic == Mic::Join { DOWN_JOINS } else { 0 };
        // Rarest first: the literals few learned clauses kept are the ones most likely to drop.
        g.sort_by_key(|&l| self.mic_key(l));
        // Literals whose drop failed (or that a join put back): not tried again.
        let mut kept: HashSet<i32> = HashSet::new();
        let mut tries = 0usize;
        let budget = 3 * g.len() + 8;
        while g.len() > 1 && tries < budget {
            let Some(i) = g.iter().position(|l| !kept.contains(l)) else {
                break;
            };
            tries += 1;
            let lit = g[i];
            let cand: Vec<i32> = g
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, &l)| l)
                .collect();
            match self.down(level, cand, joins)? {
                Some(smaller) => {
                    // The literals the join or the core kept are settled; the rest stay in order.
                    for &l in &smaller {
                        if !g.contains(&l) {
                            kept.insert(l);
                        }
                    }
                    g = smaller;
                    g.sort_by_key(|&l| self.mic_key(l));
                }
                None => {
                    kept.insert(lit);
                }
            }
        }
        Ok(g)
    }

    /// The MIC before N5.2 (`MUNUNU_IC3_MIC=legacy`): one pass in the cube's order, a failed drop
    /// keeps the literal, no join. Kept for the A/B profile only.
    fn generalize_legacy(&mut self, mut g: Vec<i32>, level: usize) -> Result<Vec<i32>, Halted> {
        let mut i = 0usize;
        let mut tries = 0usize;
        let budget = 3 * g.len() + 8;
        while i < g.len() && g.len() > 1 && tries < budget {
            tries += 1;
            let cand: Vec<i32> = g
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, &l)| l)
                .collect();
            if self.initial(&cand)? {
                i += 1;
                continue;
            }
            match self.relative(level - 1, &cand)? {
                Rel::Inductive(core) => {
                    g = self.repair_initiation(core, &cand)?;
                    i = i.min(g.len().saturating_sub(1));
                }
                Rel::Predecessor(_) => i += 1,
            }
        }
        Ok(g)
    }

    /// Bradley's `down` (no CTGs): is some sub-cube of `cand`, clear of `Init`, inductive relative
    /// to `F_{level-1}`? A predecessor that steps into `cand` is JOINED with it (the literals the
    /// predecessor disagrees with are dropped), up to [`DOWN_JOINS`] times; a join that admits an
    /// initial state, or runs out of joins, fails. `Some(g)`: the inductive cube, its initiation
    /// repaired from the core.
    fn down(
        &mut self,
        level: usize,
        mut cand: Vec<i32>,
        max_joins: usize,
    ) -> Result<Option<Vec<i32>>, Halted> {
        for join in 0..=max_joins {
            if cand.is_empty() || self.initial(&cand)? {
                return Ok(None);
            }
            match self.relative(level - 1, &cand)? {
                Rel::Inductive(core) => return Ok(Some(self.repair_initiation(core, &cand)?)),
                Rel::Predecessor(w) => {
                    if join == max_joins {
                        self.down_capped += 1;
                        return Ok(None);
                    }
                    let agrees: HashSet<i32> = w.state.iter().copied().collect();
                    let before = cand.len();
                    cand.retain(|l| agrees.contains(l));
                    if cand.len() == before {
                        // The predecessor is inside the candidate already: no join can help.
                        return Ok(None);
                    }
                    self.down_joins += 1;
                }
            }
        }
        Ok(None)
    }

    /// Add `¬g` at the highest frame it is inductive up to (from `level`); returns that frame.
    fn add_blocked(&mut self, g: Vec<i32>, level: usize) -> Result<usize, Halted> {
        let mut n = level;
        while n < self.k() {
            match self.in_phase(Phase::AddBlocked, |me| me.relative(n, &g))? {
                Rel::Inductive(_) => n += 1,
                Rel::Predecessor(_) => break,
            }
        }
        for &l in &g {
            *self.activity.entry(l).or_insert(0) += 1;
            *self.act_var.entry(l.abs()).or_insert(0.0) += self.act_inc;
        }
        // rIC3's `bump_cube_activity`: the increment grows by 1/0.9 per learned clause, so recent
        // clauses weigh more; everything is rescaled before the increment overflows.
        self.act_inc /= 0.9;
        if self.act_inc > 1e100 {
            for v in self.act_var.values_mut() {
                *v *= 1e-100;
            }
            self.act_inc *= 1e-100;
        }
        self.learned_lits += g.len() as u64;
        self.learned += 1;
        let clause: Vec<i32> = g.iter().map(|&l| -l).collect();
        for f in self.frames[1..=n].iter_mut() {
            let before = f.clauses.len();
            f.clauses.retain(|c| !clause.iter().all(|l| c.contains(l)));
            self.dead += before - f.clauses.len();
        }
        let act = self.frames[n].act;
        self.solver
            .add_clause(clause.iter().copied().chain(std::iter::once(-act)));
        self.frames[n].clauses.push(clause);
        self.maybe_rebuild();
        Ok(n)
    }

    /// Block the CTI `s` at `F_k`, recursively. `Ok(None)`: blocked; `Ok(Some(..))`: a
    /// counterexample (or the reason one did not concretise).
    fn block(&mut self, s: Vec<i32>) -> Result<Option<Ic3Outcome>, Halted> {
        self.in_phase(Phase::Block, |me| me.block_in(s))
    }

    fn block_in(&mut self, s: Vec<i32>) -> Result<Option<Ic3Outcome>, Halted> {
        let mut arena: Vec<Obligation> = Vec::new();
        let mut heap: BinaryHeap<Reverse<(usize, u64, usize)>> = BinaryHeap::new();
        let k = self.k();
        let push = |arena: &mut Vec<Obligation>,
                    heap: &mut BinaryHeap<_>,
                    o: Obligation,
                    seq: &mut u64| {
            *seq += 1;
            heap.push(Reverse((o.level, *seq, arena.len())));
            arena.push(o);
        };
        push(
            &mut arena,
            &mut heap,
            Obligation {
                cube: s,
                level: k,
                parent: None,
            },
            &mut self.seq,
        );
        while let Some(Reverse((level, _, id))) = heap.pop() {
            let cube = arena[id].cube.clone();
            // A cube with an initial state: the chain from here is a counterexample candidate (at
            // level 0 by construction, at a higher level when lifting kept an initial state).
            if level == 0 || self.initial(&cube)? {
                return self.concretize(&arena, id).map(Some);
            }
            if self.blocked(&cube, level) {
                continue;
            }
            match self.relative(level - 1, &cube)? {
                Rel::Inductive(core) => {
                    let g = self.generalize(core, &cube, level)?;
                    let n = self.add_blocked(g, level)?;
                    if n < k {
                        let parent = arena[id].parent;
                        push(
                            &mut arena,
                            &mut heap,
                            Obligation {
                                cube,
                                level: n + 1,
                                parent,
                            },
                            &mut self.seq,
                        );
                    }
                }
                Rel::Predecessor(w) => {
                    let t = self.lift(&w, Some(&cube))?;
                    let parent = arena[id].parent;
                    push(
                        &mut arena,
                        &mut heap,
                        Obligation {
                            cube: t,
                            level: level - 1,
                            parent: Some(id),
                        },
                        &mut self.seq,
                    );
                    push(
                        &mut arena,
                        &mut heap,
                        Obligation {
                            cube,
                            level,
                            parent,
                        },
                        &mut self.seq,
                    );
                }
            }
        }
        Ok(None)
    }

    /// The value of a state's bits in the model (an unencoded bit reads 0).
    fn word(&self, lits: &[i32]) -> u128 {
        lits.iter()
            .enumerate()
            .take(128)
            .map(|(i, &l)| u128::from(self.is_true(l)) << i)
            .sum()
    }

    fn state_words(&self, from: &[i32]) -> Vec<(String, u128)> {
        let mut flat = 0usize;
        self.model
            .states
            .iter()
            .map(|s| {
                let w = self.word(&from[flat..flat + s.leaves.len()]);
                flat += s.leaves.len();
                (cell_name(&s.symbol, "st", s.nid), w)
            })
            .collect()
    }

    fn input_words(&self) -> Vec<(String, u128)> {
        let mut flat = 0usize;
        self.model
            .inputs
            .iter()
            .map(|i| {
                let w = self.word(&self.inputs[flat..flat + i.leaves.len()]);
                flat += i.leaves.len();
                (cell_name(&i.symbol, "in", i.nid), w)
            })
            .collect()
    }

    /// The values the registers without `next` take at the next frame (model order), from the
    /// current SAT model — the trace carries them so the replay follows the same free choices.
    fn free_words(&self) -> Vec<(String, u128)> {
        let mut flat = 0usize;
        self.model
            .states
            .iter()
            .filter(|s| s.next.is_none())
            .map(|s| {
                let w = self.word(&self.free_next[flat..flat + s.leaves.len()]);
                flat += s.leaves.len();
                (cell_name(&s.symbol, "st", s.nid), w)
            })
            .collect()
    }

    /// Turn the obligation chain ending at `id` (whose cube meets `Init`) into a concrete trace,
    /// one SAT model of `T` per step, then replay it on the concrete simulator.
    fn concretize(&mut self, arena: &[Obligation], id: usize) -> Result<Ic3Outcome, Halted> {
        let mut chain: Vec<&[i32]> = Vec::new();
        let mut cur = Some(id);
        while let Some(i) = cur {
            chain.push(&arena[i].cube);
            cur = arena[i].parent;
        }
        let depth = chain.len() - 1;
        let stopped = |step: usize| Ic3Outcome::Stopped {
            frames: 0,
            reason: format!(
                "the counterexample chain did not concretise at step {step} of {depth} (a cube \
                 kept a state with no constraint-satisfying input)"
            ),
        };
        // Step 0: an initial state in chain[0] that steps into chain[1] (or is bad).
        let mut a = vec![self.act_init];
        a.extend_from_slice(chain[0]);
        match chain.get(1) {
            Some(next) => a.extend(next.iter().map(|&l| self.prime(l))),
            None => a.push(self.bad),
        }
        if !self.solve(&a)? {
            return Ok(stopped(0));
        }
        let initial = self.state_words(&self.cur);
        let mut inputs = vec![self.input_words()];
        let mut free_states = Vec::new();
        if chain.len() > 1 {
            free_states.push(self.free_words());
        }
        let next_state = |me: &Self| -> Vec<i32> {
            me.cur
                .iter()
                .zip(&me.nxt)
                .map(|(&c, &n)| if me.is_true(n) { c } else { -c })
                .collect()
        };
        let mut state = next_state(self);
        for step in 1..=depth {
            let mut a = state.clone();
            match chain.get(step + 1) {
                Some(next) => a.extend(next.iter().map(|&l| self.prime(l))),
                None => a.push(self.bad),
            }
            if !self.solve(&a)? {
                return Ok(stopped(step));
            }
            inputs.push(self.input_words());
            if chain.get(step + 1).is_some() {
                free_states.push(self.free_words());
            }
            state = next_state(self);
        }
        let bad_index = self
            .bad_lits
            .iter()
            .position(|&b| self.is_true(b))
            .unwrap_or(0);
        Ok(Ic3Outcome::Violated {
            depth: depth as u32,
            bad_index,
            initial,
            inputs,
            free_states,
            // Set by `replayed` before the outcome leaves `ic3`; never shipped as is.
            replay: Replay::Unavailable("not replayed yet".to_string()),
        })
    }

    /// Propagate every frame's clauses forward; `Some(inv)` when a frame empties (`F_i == F_{i+1}`).
    fn propagate(&mut self) -> Result<Option<Vec<Vec<i32>>>, Halted> {
        self.in_phase(Phase::Propagate, |me| me.propagate_in())
    }

    fn propagate_in(&mut self) -> Result<Option<Vec<Vec<i32>>>, Halted> {
        let top = self.k();
        for j in 1..top {
            let clauses = self.frames[j].clauses.clone();
            for cl in clauses {
                let cube: Vec<i32> = cl.iter().map(|&l| -l).collect();
                if let Rel::Inductive(_) = self.relative(j, &cube)? {
                    self.frames[j].clauses.retain(|c| *c != cl);
                    self.dead += 1;
                    let act = self.frames[j + 1].act;
                    self.solver
                        .add_clause(cl.iter().copied().chain(std::iter::once(-act)));
                    self.frames[j + 1].clauses.push(cl);
                }
            }
            if self.frames[j].clauses.is_empty() {
                let inv = self.frames[j + 1..]
                    .iter()
                    .flat_map(|f| f.clauses.iter().cloned())
                    .collect();
                return Ok(Some(inv));
            }
        }
        self.maybe_rebuild();
        Ok(None)
    }

    /// The direct check of an invariant, in THIS (fresh) instance: `Init ⟹ Inv`, `Inv ∧ T ⟹ Inv'`,
    /// `Inv ⟹ ¬Bad`. `Err` names the failing obligation.
    fn certify(&mut self, inv: &[Vec<i32>]) -> Result<(), String> {
        let stopped = |what: &str| format!("the certificate check was stopped during {what}");
        // ¬Inv = ∨ (¬clause): one selector per clause, forced to its clause's negation.
        let negation = |me: &mut Self, prime: bool| -> i32 {
            let mut sels = Vec::with_capacity(inv.len());
            for cl in inv {
                let s = me.fresh();
                for &l in cl {
                    let l = if prime { me.prime(l) } else { l };
                    me.solver.add_clause([-s, -l]);
                }
                sels.push(s);
            }
            me.temp_clause(sels)
        };
        let t = negation(self, false);
        match self.solve(&[self.act_init, t]) {
            Ok(true) => return Err("Init does not imply the invariant".to_string()),
            Ok(false) => {}
            Err(Halted) => return Err(stopped("initiation")),
        }
        self.release(t);
        for cl in inv {
            self.solver.add_clause(cl.iter().copied());
        }
        let t = negation(self, true);
        match self.solve(&[t]) {
            Ok(true) => return Err("the invariant is not inductive".to_string()),
            Ok(false) => {}
            Err(Halted) => return Err(stopped("consecution")),
        }
        self.release(t);
        match self.solve(&[self.bad]) {
            Ok(true) => Err("the invariant admits a bad state".to_string()),
            Ok(false) => Ok(()),
            Err(Halted) => Err(stopped("the bad check")),
        }
    }

    fn run(&mut self, opts: &Ic3Options) -> Result<Ic3Outcome, Halted> {
        // Depth 0: an initial state that is bad.
        if self.solve(&[self.act_init, self.bad])? {
            let w = self.witness();
            let arena = [Obligation {
                cube: w.state,
                level: 0,
                parent: None,
            }];
            return self.concretize(&arena, 0);
        }
        let act = self.fresh();
        self.frames.push(Frame {
            act,
            clauses: Vec::new(),
        });
        loop {
            while let Some(s) = self.bad_state()? {
                if let Some(out) = self.block(s)? {
                    return Ok(out);
                }
            }
            if opts.max_frames.is_some_and(|m| self.k() as u32 >= m) {
                return Ok(Ic3Outcome::Stopped {
                    frames: self.k() as u32,
                    reason: format!("the frame bound ({}) was reached", self.k()),
                });
            }
            let act = self.fresh();
            self.frames.push(Frame {
                act,
                clauses: Vec::new(),
            });
            if let Some(inv) = self.propagate()? {
                let frames = self.k() as u32;
                let mut fresh = Ic3::new(self.model, self.stop.clone_spec());
                return Ok(match fresh.certify(&inv) {
                    Ok(()) => Ic3Outcome::Safe {
                        frames,
                        clauses: inv.len(),
                    },
                    // The deadline or a cancel inside the check is a stop, not a failed certificate.
                    Err(_) if fresh.stop.halted() => return Err(Halted),
                    Err(why) => {
                        tracing::warn!(why, "ic3: the converged invariant failed its check");
                        Ic3Outcome::Stopped {
                            frames,
                            reason: format!(
                                "the converged invariant failed its direct check: {why}"
                            ),
                        }
                    }
                });
            }
        }
    }
}

/// Replay a `Violated` outcome on the concrete simulator through the BTOR2-witness path; the
/// outcome comes back with its [`Replay`] set, or as `Stopped` when the simulator refuses it.
fn replayed(file: &Btor2File, model: &AigModel, out: Ic3Outcome, frames: u32) -> Ic3Outcome {
    let Ic3Outcome::Violated {
        depth,
        bad_index,
        initial,
        inputs,
        free_states,
        ..
    } = out
    else {
        return out;
    };
    let witness = btor2_witness_with_free(model, bad_index, &initial, &inputs, &free_states);
    let too_wide = model
        .states
        .iter()
        .map(|s| s.leaves.len())
        .chain(model.inputs.iter().map(|i| i.leaves.len()))
        .any(|w| w > 128);
    let replay = match parse_btor2_witness(&witness).and_then(|w| trace_from_witness(file, &w)) {
        Ok(trace) => Replay::Confirmed(trace),
        Err(why) if too_wide => Replay::Unavailable(format!(
            "the concrete simulator carries 128-bit cells; this design has a wider one ({why})"
        )),
        Err(why) => {
            tracing::warn!(why, "ic3: the concretised counterexample did not replay");
            return Ic3Outcome::Stopped {
                frames,
                reason: format!(
                    "the counterexample did not replay on the concrete simulator: {why}"
                ),
            };
        }
    };
    Ic3Outcome::Violated {
        depth,
        bad_index,
        initial,
        inputs,
        free_states,
        replay,
    }
}

/// IC3 on `file`. `Err` = the design cannot be modelled (arrays, an operator the AIG backend
/// refuses), with the reason; every other way of not answering is a `Stopped` with its reason.
pub fn ic3(file: &Btor2File, opts: &Ic3Options) -> Result<Ic3Outcome, String> {
    let model = AigModel::build(file)?;
    if model.bads.is_empty() {
        // A design with no `bad` has nothing for a reachability engine to decide; every other
        // member refuses it, and so does this one — `Safe` would read as a proof of nothing.
        return Err(NO_BAD.to_string());
    }
    Ok(ic3_model(file, &model, opts))
}

const NO_BAD: &str = "the design has no bad property; nothing to decide";

/// [`ic3`] over an already-built model. A model without a `bad` is a `Stopped` with the reason
/// ([`ic3`] refuses it before the search).
pub fn ic3_model(file: &Btor2File, model: &AigModel, opts: &Ic3Options) -> Ic3Outcome {
    if model.bads.is_empty() {
        return Ic3Outcome::Stopped {
            frames: 0,
            reason: NO_BAD.to_string(),
        };
    }
    let stop = Stop {
        deadline: opts.deadline,
        cancel: opts.cancel.clone(),
    };
    let t0 = Instant::now();
    let mut search = Ic3::new(model, stop);
    let out = match search.run(opts) {
        Ok(out) => replayed(file, model, out, search.k() as u32),
        Err(Halted) => Ic3Outcome::Stopped {
            frames: search.k() as u32,
            reason: search.stop.reason("stopped"),
        },
    };
    let kind = match &out {
        Ic3Outcome::Safe { .. } => "safe",
        Ic3Outcome::Violated { .. } => "violated",
        Ic3Outcome::Stopped { .. } => "stopped",
    };
    tracing::debug!(
        frames = search.k(),
        clauses = search.live_clauses(),
        queries = search.queries,
        elapsed_ms = t0.elapsed().as_millis() as u64,
        phases = %search.phase_summary(),
        outcome = kind,
        "ic3 search"
    );
    out
}

/// N5.2 — the per-phase profile of one corpus row, for the generalisation work: `frames`,
/// `clauses`, `queries` and `phase: queries / solver ms` per phase, printed to stderr. Reads
/// `MUNUNU_IC3_PROFILE_FILE` (a BTOR2 file) and `MUNUNU_IC3_PROFILE_MS` (the deadline, default
/// 60 000). `#[ignore]`d: an external file, minutes.
#[cfg(test)]
#[test]
#[ignore = "manual: ic3 phase profile of one BTOR2 file; set MUNUNU_IC3_PROFILE_FILE (+ MUNUNU_IC3_PROFILE_MS)"]
fn ic3_phase_profile_of_one_file() {
    let Ok(path) = std::env::var("MUNUNU_IC3_PROFILE_FILE") else {
        eprintln!("MUNUNU_IC3_PROFILE_FILE unset — skipped");
        return;
    };
    let ms: u64 = std::env::var("MUNUNU_IC3_PROFILE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60_000);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let file = crate::adapter::btor2::parser::parse(&text).unwrap_or_else(|e| panic!("{e:?}"));
    let model = AigModel::build(&file).unwrap_or_else(|e| panic!("{e}"));
    let stop = Stop {
        deadline: Some(Instant::now() + std::time::Duration::from_millis(ms)),
        cancel: None,
    };
    let t0 = Instant::now();
    let mut search = Ic3::new(&model, stop);
    let out = search.run(&Ic3Options::default());
    let kind = match &out {
        Ok(Ic3Outcome::Safe { .. }) => "safe",
        Ok(Ic3Outcome::Violated { .. }) => "violated",
        Ok(Ic3Outcome::Stopped { .. }) => "stopped",
        Err(Halted) => "halted",
    };
    eprintln!(
        "ic3 profile {path}: outcome={kind} frames={} clauses={} queries={} elapsed_ms={} {}",
        search.k(),
        search.live_clauses(),
        search.queries,
        t0.elapsed().as_millis(),
        search.phase_summary()
    );
}

/// The direct invariant check on its own, for the negative controls: does `inv` (clauses over the
/// register bits, as `(state index, bit, value)` literals) certify `file`?
#[cfg(test)]
pub(crate) fn certify_literal_clauses(
    file: &Btor2File,
    inv: &[Vec<(usize, usize, bool)>],
) -> Result<(), String> {
    let model = AigModel::build(file)?;
    let mut me = Ic3::new(
        &model,
        Stop {
            deadline: None,
            cancel: None,
        },
    );
    let first: Vec<usize> = model
        .states
        .iter()
        .scan(0, |acc, s| {
            let f = *acc;
            *acc += s.leaves.len();
            Some(f)
        })
        .collect();
    let clauses: Vec<Vec<i32>> = inv
        .iter()
        .map(|cl| {
            cl.iter()
                .map(|&(si, bi, val)| {
                    let v = me.cur[first[si] + bi];
                    if val { v } else { -v }
                })
                .collect()
        })
        .collect();
    me.certify(&clauses)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;

    fn run(text: &str) -> Ic3Outcome {
        ic3(&parser::parse(text).unwrap(), &Ic3Options::default()).unwrap()
    }

    /// A 3-bit counter from 0 by 1: `c = 7` first holds at depth 7.
    const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 one 1\n\
                           6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                           10 eq 9 3 8\n11 bad 10\n";

    /// The same counter by 2: it only ever holds even values, so `c = 7` never — the invariant is
    /// one clause (`¬c[0]`), which no bounded search finds and k-induction needs k = 1 for.
    const EVEN_COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 constd 1 2\n\
                                6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                                10 eq 9 3 8\n11 bad 10\n";

    #[test]
    fn a_reachable_bad_is_a_replayed_trace_at_its_first_depth() {
        match run(COUNTER) {
            Ic3Outcome::Violated {
                depth,
                bad_index,
                initial,
                inputs,
                replay,
                ..
            } => {
                assert_eq!((depth, bad_index), (7, 0));
                assert_eq!(initial, vec![("c".to_string(), 0)]);
                assert_eq!(inputs.len(), 8);
                assert!(matches!(replay, Replay::Confirmed(_)), "{replay:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unreachable_bad_is_proved_by_a_checked_invariant() {
        match run(EVEN_COUNTER) {
            Ic3Outcome::Safe { clauses, .. } => assert!(clauses >= 1, "clauses {clauses}"),
            other => panic!("{other:?}"),
        }
    }

    /// An input steers the counter (it steps only while `go` holds); a constraint forbidding `go`
    /// makes `bad` unreachable, and without it the trace drives `go` every step.
    #[test]
    fn constraints_hold_on_every_step_and_the_trace_carries_the_inputs() {
        let text = "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 c\n5 init 1 4 3\n\
                    6 input 2 go\n7 one 1\n8 add 1 4 7\n9 ite 1 6 8 4\n10 next 1 4 9\n\
                    11 ones 1\n12 eq 2 4 11\n13 bad 12\n14 not 2 6\n15 constraint 14\n";
        assert!(
            matches!(run(text), Ic3Outcome::Safe { .. }),
            "{:?}",
            run(text)
        );
        let free = text.replace("15 constraint 14\n", "");
        match run(&free) {
            Ic3Outcome::Violated {
                depth,
                inputs,
                replay,
                ..
            } => {
                assert_eq!(depth, 7);
                assert!(
                    inputs[..7]
                        .iter()
                        .all(|fr| fr == &vec![("go".to_string(), 1)]),
                    "{inputs:?}"
                );
                assert!(matches!(replay, Replay::Confirmed(_)), "{replay:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The even counter with a `go` input: the invariant must hold whatever the input does.
    #[test]
    fn a_proof_quantifies_over_the_inputs() {
        let text = "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 c\n5 init 1 4 3\n\
                    6 input 2 go\n7 constd 1 2\n8 add 1 4 7\n9 ite 1 6 8 4\n10 next 1 4 9\n\
                    11 ones 1\n12 eq 2 4 11\n13 bad 12\n";
        assert!(
            matches!(run(text), Ic3Outcome::Safe { .. }),
            "{:?}",
            run(text)
        );
    }

    #[test]
    fn a_register_without_init_is_free_at_frame_zero() {
        let text = "1 sort bitvec 3\n2 sort bitvec 1\n3 state 1 c\n4 next 1 3 3\n5 constd 1 5\n\
                    6 eq 2 3 5\n7 bad 6\n";
        match run(text) {
            Ic3Outcome::Violated { depth, initial, .. } => {
                assert_eq!((depth, initial), (0, vec![("c".to_string(), 5)]));
            }
            other => panic!("{other:?}"),
        }
    }

    /// Two `bad`s: the index names the one true at the depth found (the second, at depth 3).
    #[test]
    fn the_bad_index_names_the_property_the_trace_falsifies() {
        let text = "1 sort bitvec 3\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 one 1\n\
                    6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n10 eq 9 3 8\n11 bad 10\n\
                    12 constd 1 3\n13 eq 9 3 12\n14 bad 13\n";
        match run(text) {
            Ic3Outcome::Violated {
                depth, bad_index, ..
            } => assert_eq!((depth, bad_index), (3, 1)),
            other => panic!("{other:?}"),
        }
    }

    /// A relational `init` (two registers start equal) and a `bad` on their difference.
    #[test]
    fn a_relational_init_is_an_invariant_the_search_keeps() {
        let text = "1 sort bitvec 4\n2 sort bitvec 1\n3 state 1 a\n4 state 1 b\n5 init 1 4 3\n\
                    6 one 1\n7 add 1 3 6\n8 add 1 4 6\n9 next 1 3 7\n10 next 1 4 8\n\
                    11 neq 2 3 4\n12 bad 11\n";
        assert!(
            matches!(run(text), Ic3Outcome::Safe { .. }),
            "{:?}",
            run(text)
        );
    }

    #[test]
    fn a_cancelled_search_stops_and_says_so() {
        let cancel = Arc::new(AtomicBool::new(true));
        let out = ic3(
            &parser::parse(EVEN_COUNTER).unwrap(),
            &Ic3Options {
                cancel: Some(cancel),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            matches!(&out, Ic3Outcome::Stopped { reason, .. } if reason == "cancelled"),
            "{out:?}"
        );
    }

    #[test]
    fn the_frame_bound_is_a_stop_not_a_verdict() {
        // A 6-bit counter from 0 by 1 reaching 63: a deep counterexample.
        let text = "1 sort bitvec 6\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 one 1\n\
                    6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n10 eq 9 3 8\n11 bad 10\n";
        let out = ic3(
            &parser::parse(text).unwrap(),
            &Ic3Options {
                max_frames: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            matches!(&out, Ic3Outcome::Stopped { frames: 2, reason } if reason.contains("frame bound")),
            "{out:?}"
        );
    }

    /// Wider than the simulator carries: the verdict ships, the replay says it could not run.
    #[test]
    fn a_trace_the_simulator_cannot_carry_ships_unreplayed_and_says_so() {
        let text = "1 sort bitvec 130\n2 sort bitvec 1\n3 state 1 c\n4 next 1 3 3\n5 constd 1 5\n\
                    6 eq 2 3 5\n7 bad 6\n";
        match run(text) {
            Ic3Outcome::Violated { depth, replay, .. } => {
                assert_eq!(depth, 0);
                assert!(
                    matches!(&replay, Replay::Unavailable(why) if why.contains("128")),
                    "{replay:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// No `bad` is nothing to decide: a refusal (as every other member), never a `Safe`.
    #[test]
    fn a_design_without_bad_is_refused_not_proved() {
        let text = "1 sort bitvec 3\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 one 1\n6 add 1 3 5\n\
                    7 next 1 3 6\n";
        let file = parser::parse(text).unwrap();
        let err = ic3(&file, &Ic3Options::default()).unwrap_err();
        assert!(err.contains("no bad property"), "{err}");
        let model = AigModel::build(&file).unwrap();
        assert!(matches!(
            ic3_model(&file, &model, &Ic3Options::default()),
            Ic3Outcome::Stopped { frames: 0, reason } if reason.contains("no bad property")
        ));
    }

    /// A register without `next` is free at every step; the trace carries the value it takes at
    /// each frame, and the replay follows it (held at 0 it would never reach 5).
    #[test]
    fn a_register_without_next_is_free_and_its_trace_carries_each_frames_value() {
        let text = "1 sort bitvec 3\n2 sort bitvec 1\n3 state 1 c\n4 zero 1\n5 init 1 3 4\n\
                    6 constd 1 5\n7 eq 2 3 6\n8 bad 7\n";
        match run(text) {
            Ic3Outcome::Violated {
                depth,
                initial,
                free_states,
                replay,
                ..
            } => {
                assert_eq!((depth, initial), (1, vec![("c".to_string(), 0)]));
                assert_eq!(free_states, vec![vec![("c".to_string(), 5)]]);
                assert!(matches!(replay, Replay::Confirmed(_)), "{replay:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The free register is the SECOND state: the `#k` block `btor2_witness_with_free` writes
    /// (model order) must name the index the replay reads (file order), or the replay refuses the
    /// trace and the member abstains.
    #[test]
    fn a_free_register_that_is_not_the_first_state_is_indexed_alike_by_writer_and_replay() {
        let text = "1 sort bitvec 1\n2 sort bitvec 3\n3 state 1 a\n4 zero 1\n5 init 1 3 4\n\
                    6 one 1\n7 next 1 3 6\n8 state 2 f\n9 zero 2\n10 init 2 8 9\n\
                    11 constd 2 5\n12 eq 1 8 11\n13 and 1 3 12\n14 bad 13\n";
        match run(text) {
            Ic3Outcome::Violated {
                depth,
                initial,
                free_states,
                replay,
                ..
            } => {
                assert_eq!(depth, 1);
                assert_eq!(initial, vec![("a".to_string(), 0), ("f".to_string(), 0)]);
                assert_eq!(free_states, vec![vec![("f".to_string(), 5)]]);
                assert!(matches!(replay, Replay::Confirmed(_)), "{replay:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_array_design_is_refused_with_its_name() {
        let text = "1 sort bitvec 2\n2 sort bitvec 4\n3 sort array 1 2\n4 state 3 mem\n\
                    5 sort bitvec 1\n6 zero 5\n7 bad 6\n";
        let err = ic3(&parser::parse(text).unwrap(), &Ic3Options::default()).unwrap_err();
        assert!(err.contains("`mem` is an array"), "{err}");
    }

    /// Negative controls for the certificate: each of its three obligations can fail.
    #[test]
    fn the_certificate_check_rejects_each_kind_of_wrong_invariant() {
        let even = parser::parse(EVEN_COUNTER).unwrap();
        // The true invariant passes.
        certify_literal_clauses(&even, &[vec![(0, 0, false)]]).expect("¬c[0] certifies");
        // `c ≠ 7` alone is not inductive (5 → 7).
        let not_seven = vec![(0, 0, false), (0, 1, false), (0, 2, false)];
        let err = certify_literal_clauses(&even, &[not_seven]).unwrap_err();
        assert!(err.contains("not inductive"), "{err}");
        // `true` (no clauses) admits the bad state.
        let err = certify_literal_clauses(&even, &[]).unwrap_err();
        assert!(err.contains("admits a bad state"), "{err}");
        // `c[0]` is contradicted by `init` (c = 0).
        let err = certify_literal_clauses(&even, &[vec![(0, 0, true)]]).unwrap_err();
        assert!(err.contains("Init does not imply"), "{err}");
    }
}
