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
use std::collections::BinaryHeap;
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
    /// N5.6 — per register bit (indexed like `cur`), its `init` when that is a CONSTANT; `None`
    /// for a register without `init` or with an expression init. [`Self::initial_syntactic`]
    /// reads it.
    init_const: Vec<Option<bool>>,
    /// N5.6 — whether `Init ∧ constraints` has a state (the solver's answer to the empty cube),
    /// asked once, the first time the syntactic check decides a cube `Init` implies.
    init_nonempty: Option<bool>,
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
    /// N5.2 — the widths profile: lifted CTIs (literals, count) and learned clauses (literals,
    /// count).
    cti_lits: u64,
    ctis: u64,
    learned_lits: u64,
    learned: u64,
    /// N5.2 — the lifted-width profile split by obligation kind: root lifts (from `bad`) and
    /// descendant lifts (into an obligation cube), plus the descendant lifts' target widths.
    root_lits: u64,
    roots: u64,
    desc_lits: u64,
    descs: u64,
    target_lits: u64,
    /// N5.2 — how many `initial` checks the search asked (N5.6: the syntactic ones included).
    init_queries: u64,
    /// … and the solver time they took (nanoseconds); the same for `repair_initiation`.
    init_ns: u64,
    /// N5.6 — how many of the `initial` checks the syntactic path answered without the solver.
    init_syntactic: u64,
    repair_queries: u64,
    repair_ns: u64,
    /// N5.2 — the width of the cube entering MIC's drop loop (after the relative core and the
    /// initiation repair, before any drop), summed, and the count.
    mic_in_lits: u64,
    mic_ins: u64,
    /// mununu#1056 — how many dead states the concretisation blocked and re-solved around.
    concretize_retries: u64,
}

/// mununu#1056 — the bound on dead states one concretisation blocks before it abstains; each
/// one is a distinct state of a chain cube, so the walk ends, but not within a budget on its own.
const MAX_CONCRETIZE_RETRIES: u64 = 4_096;

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
            "latches={} trans_clauses={} vars={} init_queries={} init_syntactic={} init_ms={} repair_queries={} repair_ms={} mic_in_width={:.1}/{} cti_width={:.1} root_width={:.1}/{} desc_width={:.1}/{} target_width={:.1} clause_width={:.1} concretize_retries={}",
            self.cur.len(),
            self.trans.len(),
            self.next_var,
            self.init_queries,
            self.init_syntactic,
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
            self.concretize_retries,
        ));
        parts.join(" ")
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
        let mut init_const = vec![None; cur.len()];
        for (si, s) in model.states.iter().enumerate() {
            if let Some(init) = &s.init {
                for (bi, &il) in init.iter().enumerate() {
                    let iv = enc.encode(il);
                    if let Some(&c) = cur.get(first[si] + bi) {
                        init_clauses.push(vec![-act_init, -c, iv]);
                        init_clauses.push(vec![-act_init, c, -iv]);
                    }
                    if let Some(slot) = init_const.get_mut(first[si] + bi) {
                        // The AIG is structurally hashed: an `init` that folds to a constant
                        // IS `Lit::TRUE` / `Lit::FALSE` here.
                        *slot = if il == Lit::FALSE {
                            Some(false)
                        } else if il == Lit::TRUE {
                            Some(true)
                        } else {
                            None
                        };
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
            init_const,
            init_nonempty: None,
            frames: vec![Frame {
                act: act_init,
                clauses: Vec::new(),
            }],
            dead: 0,
            seq: 0,
            queries: 0,
            phase: Phase::Other,
            by_phase: [(0, 0); Phase::COUNT],
            cti_lits: 0,
            ctis: 0,
            learned_lits: 0,
            learned: 0,
            root_lits: 0,
            roots: 0,
            desc_lits: 0,
            descs: 0,
            target_lits: 0,
            init_queries: 0,
            init_ns: 0,
            init_syntactic: 0,
            repair_queries: 0,
            repair_ns: 0,
            mic_in_lits: 0,
            mic_ins: 0,
            concretize_retries: 0,
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

    /// Does `cube` contain an initial state? N5.6 — answered without the solver when the cube
    /// decides it syntactically ([`Self::initial_syntactic`]); by the solver otherwise. On the
    /// OpenTitan edn sec_cm monitors (every register with a constant init) the solver form took
    /// 99.7 % of a 300 s slice (N5.5 gate, 100,280 calls at ~3 ms in an 82k-clause solver).
    fn initial(&mut self, cube: &[i32]) -> Result<bool, Halted> {
        self.init_queries += 1;
        match self.initial_syntactic(cube) {
            Some(false) => {
                self.init_syntactic += 1;
                return Ok(false);
            }
            Some(true) => {
                self.init_syntactic += 1;
                return self.init_nonempty();
            }
            None => {}
        }
        let mut a = vec![self.act_init];
        a.extend_from_slice(cube);
        let t0 = Instant::now();
        let r = self.solve(&a);
        self.init_ns += u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        r
    }

    /// N5.6 — the three-way syntactic initiation check. A literal that contradicts its
    /// register's CONSTANT init: `Init ∧ cube` is UNSAT, `Some(false)`. Every literal on a
    /// constant-init register and none contradicts: `Init` implies `cube`, so `Init ∧ cube ≡
    /// Init`, `Some(true)` — and the caller asks once whether `Init` itself has a state under the
    /// constraints ([`Self::init_nonempty`]). Any literal on a register without `init` or with an
    /// expression init: `None`, the solver decides. Both decided answers equal the solver's by
    /// construction; the differential test
    /// `the_syntactic_initiation_check_agrees_with_the_solver` pins it on a mixed design.
    fn initial_syntactic(&self, cube: &[i32]) -> Option<bool> {
        let mut all_constant = true;
        for &l in cube {
            match self.init_const_of(l) {
                Some(v) if v != (l > 0) => return Some(false),
                Some(_) => {}
                None => all_constant = false,
            }
        }
        all_constant.then_some(true)
    }

    /// The constant init of the register bit `lit` is on; `None` when that bit's init is not a
    /// constant, or `lit` is not a register bit.
    fn init_const_of(&self, lit: i32) -> Option<bool> {
        let bit = lit.unsigned_abs().checked_sub(2)? as usize;
        self.init_const.get(bit).copied().flatten()
    }

    /// `Init ∧ constraints` has a state: the solver's answer to the empty cube, asked once and
    /// kept (a `rebuild` keeps `Init`, `T` and the constraints, so the answer stands).
    fn init_nonempty(&mut self) -> Result<bool, Halted> {
        if let Some(v) = self.init_nonempty {
            return Ok(v);
        }
        let t0 = Instant::now();
        let r = self.solve(&[self.act_init]);
        self.init_ns += u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if let Ok(v) = r {
            self.init_nonempty = Some(v);
        }
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
        let mut a = vec![t];
        a.extend_from_slice(&w.state);
        a.extend_from_slice(&w.inputs);
        if succ.is_some() {
            a.extend_from_slice(&w.free_next);
        }
        let r = match self.solve(&a) {
            Ok(false) => {
                let lifted: Vec<i32> = w
                    .state
                    .iter()
                    .copied()
                    .filter(|&l| self.solver.failed(l))
                    .collect();
                Ok(lifted)
            }
            // Cannot happen (the assignment is a model that steps there); keep the full state.
            Ok(true) => Ok(w.state.clone()),
            Err(h) => Err(h),
        };
        if let Ok(lifted) = &r {
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
        }
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
        // The add-back is in the cube's order, one `initial` check per literal: on a constant-init
        // design every check is syntactic (N5.6), so the loop costs nothing — before N5.6 it was
        // ~170 solver calls per repair on the edn sec_cm monitors, 99,311 of a slice's 101,502
        // queries. N5.6 also tried adding ONLY the first literal that contradicts its constant
        // init (a narrower core, no walk): no row gained on the M2 68 and no time won on the edn
        // monitors, so the cores stay exactly what the solver-based walk returned.
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
    /// of `Init`. Bounded in attempts; the order is the cube's. N5.2 measured the alternatives
    /// (activity order with a seeded tie-break, rIC3-style decayed activity, Bradley's `down`
    /// join, iterated lifting) on the HWMCC'20 rows the owned member loses and none moved the
    /// learned-clause width or the frame count outside seed noise; the one-pass MIC stays, the
    /// variants live in the N5.2 diagnostic commit (see the PR) and the private measurement.
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

    /// Add `¬g` at the highest frame it is inductive up to (from `level`); returns that frame.
    fn add_blocked(&mut self, g: Vec<i32>, level: usize) -> Result<usize, Halted> {
        let mut n = level;
        while n < self.k() {
            match self.in_phase(Phase::AddBlocked, |me| me.relative(n, &g))? {
                Rel::Inductive(_) => n += 1,
                Rel::Predecessor(_) => break,
            }
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
        let stopped = |step: usize, retries: u64| Ic3Outcome::Stopped {
            frames: 0,
            reason: if retries >= MAX_CONCRETIZE_RETRIES {
                format!(
                    "the counterexample chain did not concretise: {retries} dead states were \
                     blocked and re-solved around (the cap) and step {step} of {depth} still has \
                     no constraint-satisfying input"
                )
            } else {
                format!(
                    "the counterexample chain did not concretise at step {step} of {depth} (a \
                     cube kept a state with no constraint-satisfying input; {retries} dead states \
                     blocked and re-solved around)"
                )
            },
        };
        // mununu#1056 — a lifted cube is a MAY set: with input constraints it keeps states whose
        // constraint-satisfying inputs never reach the next cube (the lift proves
        // `state ∧ inputs ∧ T → next'` with the constraints as hard units, so a state those
        // inputs are forbidden in satisfies it vacuously). Step 0 starts from an initial state
        // in chain[0]; step `s` from the concrete state the previous model stepped into. When a
        // step has no model, the state it starts from is such a dead state: it is blocked as a
        // successor of the previous step (a temporary clause over the primed literals) and that
        // step is re-solved, backing up as far as needed; only a dry step 0, or the cap, abstains.
        // Every shipped step is still one SAT model of `T`, and the trace is replayed before it
        // ships. HWMCC'20 `shift_register_top_w16_d8_e0` (pop forbidden when empty, push when
        // full) is the measured case: a depth-11 chain died at step 10.
        let assignment = |me: &Self, vars: &[i32]| -> Vec<i32> {
            vars.iter()
                .map(|&v| if me.is_true(v) { v } else { -v })
                .collect()
        };
        let next_state = |me: &Self| -> Vec<i32> {
            me.cur
                .iter()
                .zip(&me.nxt)
                .map(|(&c, &n)| if me.is_true(n) { c } else { -c })
                .collect()
        };
        // `blocks[s]`: the activation literals of the clauses blocking dead states at step `s`,
        // assumed by the query that produces step `s` (the one at `s - 1`).
        let mut blocks: Vec<Vec<i32>> = vec![Vec::new(); depth + 1];
        let mut states: Vec<Vec<i32>> = Vec::with_capacity(depth + 1);
        let mut initial = Vec::new();
        let mut inputs: Vec<Vec<(String, u128)>> = Vec::with_capacity(depth + 1);
        let mut free_states: Vec<Vec<(String, u128)>> = Vec::with_capacity(depth);
        let mut step = 0usize;
        let mut retries = 0u64;
        let failed = loop {
            let mut a: Vec<i32> = if step == 0 {
                let mut v = vec![self.act_init];
                v.extend_from_slice(chain[0]);
                v
            } else {
                states[step].clone()
            };
            match chain.get(step + 1) {
                Some(next) => a.extend(next.iter().map(|&l| self.prime(l))),
                None => a.push(self.bad),
            }
            if let Some(b) = blocks.get(step + 1) {
                a.extend_from_slice(b);
            }
            if self.solve(&a)? {
                if step == 0 {
                    initial = self.state_words(&self.cur);
                    states.clear();
                    states.push(assignment(self, &self.cur));
                }
                inputs.truncate(step);
                inputs.push(self.input_words());
                if step == depth {
                    break None;
                }
                free_states.truncate(step);
                free_states.push(self.free_words());
                states.truncate(step + 1);
                states.push(next_state(self));
                step += 1;
            } else if step == 0 || retries >= MAX_CONCRETIZE_RETRIES {
                break Some(stopped(step, retries));
            } else {
                retries += 1;
                let dead: Vec<i32> = states[step].iter().map(|&l| -self.prime(l)).collect();
                let t = self.temp_clause(dead);
                blocks[step].push(t);
                step -= 1;
            }
        };
        // Read off the last model before any clause is added (a `release` invalidates it), and
        // only after a SAT answer: a stopped loop ended on an UNSAT solve, and reading a value
        // then is a CaDiCaL API violation that aborts the process.
        let bad_index = failed.is_none().then(|| {
            self.bad_lits
                .iter()
                .position(|&b| self.is_true(b))
                .unwrap_or(0)
        });
        self.concretize_retries += retries;
        for t in blocks.into_iter().flatten() {
            self.release(t);
        }
        if let Some(outcome) = failed {
            return Ok(outcome);
        }
        // `Some` whenever the loop reached `bad` (it is computed exactly then).
        let bad_index = bad_index.unwrap_or(0);
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
    let Some(path) = crate::adapter::env_var::read("MUNUNU_IC3_PROFILE_FILE") else {
        eprintln!("MUNUNU_IC3_PROFILE_FILE unset — skipped");
        return;
    };
    let ms: u64 = crate::adapter::env_var::read("MUNUNU_IC3_PROFILE_MS")
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

    /// REPRODUCER (mununu#1055) — a counterexample through `srem` ships. The concrete simulator
    /// evaluates the signed-division family, so the replay confirms the trace instead of the
    /// engine abstaining with "operator Srem unsupported in Phase 1 bit-blaster" (HWMCC'20
    /// `anderson.3`: a real counterexample at frame 3, dropped). A 4-bit input divided by 3
    /// (signed) leaves −1 or −2 for a negative dividend; `bad` is the remainder being −1.
    #[test]
    fn a_counterexample_through_srem_replays_and_ships() {
        let text = "1 sort bitvec 4\n2 sort bitvec 1\n3 zero 1\n4 state 1 r\n5 init 1 4 3\n\
                    6 input 1 x\n7 constd 1 3\n8 srem 1 6 7\n9 next 1 4 8\n10 ones 1\n\
                    11 eq 2 4 10\n12 bad 11\n";
        match run(text) {
            Ic3Outcome::Violated { depth, replay, .. } => {
                assert_eq!(depth, 1);
                assert!(matches!(replay, Replay::Confirmed(_)), "{replay:?}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// A concretisation that stops (here a dry step 0: the chain's only cube contradicts `Init`)
    /// returns `Stopped` with its reason. It used to read the bad index from the solver after that
    /// last UNSAT solve, and CaDiCaL aborts the process on a model read in an unsatisfied state:
    /// every stopped concretisation crashed the run instead of abstaining.
    #[test]
    fn a_concretisation_that_stops_abstains_instead_of_reading_a_model() {
        let file = parser::parse(COUNTER).unwrap();
        let model = AigModel::build(&file).unwrap();
        let mut search = Ic3::new(
            &model,
            Stop {
                deadline: None,
                cancel: None,
            },
        );
        // `c` starts at 0, so bit 0 set is no initial state.
        let arena = vec![Obligation {
            cube: vec![search.cur[0]],
            level: 0,
            parent: None,
        }];
        match search.concretize(&arena, 0) {
            Ok(Ic3Outcome::Stopped { reason, .. }) => {
                assert!(reason.contains("step 0"), "{reason}");
            }
            Ok(other) => panic!("a cube outside Init must not concretise: {other:?}"),
            Err(_) => panic!("no deadline or cancel was set"),
        }
    }

    /// mununu#1056 — a chain whose cube keeps a dead state still concretises. Three-bit `s`
    /// from `000`: on `i ∧ j` to the DEAD state `011`, on `i ∧ ¬j` to the ALIVE state `010`;
    /// `010` reaches `bad` (`111`) on `i ∧ j`; a constraint forbids `i ∧ j` in `011`, which on
    /// `i ∧ ¬j` goes to `010`; every other state stays. The lifted predecessor of `bad` under the
    /// witness inputs `i ∧ j` is `01x`: `010` steps to `bad`, `011` only vacuously (the inputs are
    /// forbidden there), and the lift drops bit 0 because nothing distinguishes them to the
    /// solver. Step 0 of the concretisation may land in `011`, and step 1 then has no
    /// constraint-satisfying input into `bad`: the concretisation backs up, blocks `011` as a
    /// successor of step 0 and re-solves to `010`. The shipped path is a real one (two steps
    /// through `010`, or three through `011` then `010`) and replays. The mirror swaps which
    /// inputs lead where, so a solver phase preference cannot pick the alive state by luck in
    /// both.
    #[test]
    fn a_chain_through_a_dead_state_concretises_by_backing_up() {
        let dead_on_ij = "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 s\n5 init 1 4 3\n\
                          6 input 2 i\n7 input 2 j\n8 constd 1 2\n9 constd 1 3\n10 constd 1 7\n\
                          11 eq 2 4 3\n12 eq 2 4 8\n13 eq 2 4 9\n14 not 2 7\n15 and 2 6 7\n\
                          16 and 2 6 14\n17 ite 1 15 9 4\n18 ite 1 16 8 17\n19 ite 1 15 10 4\n\
                          20 ite 1 16 8 4\n21 ite 1 11 18 4\n22 ite 1 12 19 21\n\
                          23 ite 1 13 20 22\n24 next 1 4 23\n25 eq 2 4 10\n26 bad 25\n\
                          27 and 2 13 15\n28 not 2 27\n29 constraint 28\n";
        // The mirror: `i ∧ ¬j` leads to the dead state and is the forbidden / bad-reaching input.
        let dead_on_inj = dead_on_ij
            .replace("17 ite 1 15 9 4\n", "17 ite 1 16 9 4\n")
            .replace("18 ite 1 16 8 17\n", "18 ite 1 15 8 17\n")
            .replace("19 ite 1 15 10 4\n", "19 ite 1 16 10 4\n")
            .replace("20 ite 1 16 8 4\n", "20 ite 1 15 8 4\n")
            .replace("27 and 2 13 15\n", "27 and 2 13 16\n");
        for (name, text) in [("ij", dead_on_ij), ("inj", dead_on_inj.as_str())] {
            match run(text) {
                Ic3Outcome::Violated {
                    depth,
                    inputs,
                    replay,
                    ..
                } => {
                    assert!((2..=3).contains(&depth), "{name}: depth {depth} {inputs:?}");
                    assert!(matches!(replay, Replay::Confirmed(_)), "{name}: {replay:?}");
                }
                other => panic!("{name}: {other:?}"),
            }
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

    /// N5.6 — four 4-bit registers of every init kind: `a` constant 0, `b` without `init`, `c`
    /// with an expression init (`c = a`), `d` constant 5. Bits `cur[0..4]`, `[4..8]`, `[8..12]`,
    /// `[12..16]`.
    const MIXED_INIT: &str = "1 sort bitvec 4\n2 sort bitvec 1\n3 zero 1\n4 state 1 a\n5 init 1 4 3\n\
                              6 state 1 b\n7 state 1 c\n8 init 1 7 4\n9 constd 1 5\n10 state 1 d\n\
                              11 init 1 10 9\n12 one 1\n13 add 1 4 12\n14 next 1 4 13\n\
                              15 next 1 6 6\n16 add 1 7 12\n17 next 1 7 16\n18 next 1 10 10\n\
                              19 ones 1\n20 eq 2 4 19\n21 bad 20\n";

    fn search_of(text: &str) -> (AigModel, Stop) {
        let model = AigModel::build(&parser::parse(text).unwrap()).unwrap();
        (
            model,
            Stop {
                deadline: None,
                cancel: None,
            },
        )
    }

    /// The solver's own answer to `Init ∧ cube`, as [`Ic3::initial`] asked it before N5.6.
    fn initial_by_solver(me: &mut Ic3<'_>, cube: &[i32]) -> bool {
        let mut a = vec![me.act_init];
        a.extend_from_slice(cube);
        me.solve(&a).unwrap()
    }

    /// N5.6 — the three-way check on the mixed design: every answer it gives equals the solver's
    /// (3,000 random cubes of width 1–8 over the 16 bits), it decides every cube that lies on the
    /// constant-init registers only, and it declines every cube that touches `b` or `c`.
    #[test]
    fn the_syntactic_initiation_check_agrees_with_the_solver() {
        let (model, stop) = search_of(MIXED_INIT);
        let mut me = Ic3::new(&model, stop);
        assert_eq!(me.cur.len(), 16);
        let expect: Vec<Option<bool>> = (0..16)
            .map(|i| match i {
                0..=3 => Some(false),
                12 => Some(true),
                13 => Some(false),
                14 => Some(true),
                15 => Some(false),
                _ => None,
            })
            .collect();
        assert_eq!(me.init_const, expect);
        let mut seed: u64 = 0x5eed_1234_abcd_0001;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut unsat, mut sat, mut declined) = (0, 0, 0);
        for _ in 0..3_000 {
            let width = (next() % 8 + 1) as usize;
            let mut bits: Vec<usize> = Vec::new();
            while bits.len() < width {
                let b = (next() % 16) as usize;
                if !bits.contains(&b) {
                    bits.push(b);
                }
            }
            let cube: Vec<i32> = bits
                .iter()
                .map(|&b| {
                    if next() % 2 == 0 {
                        me.cur[b]
                    } else {
                        -me.cur[b]
                    }
                })
                .collect();
            // The expectation from the table alone: a literal against a constant init decides
            // UNSAT whatever else the cube holds; a cube on constant-init bits only, SAT;
            // anything else is the solver's.
            let contradicts = bits
                .iter()
                .zip(&cube)
                .any(|(&b, &l)| expect[b].is_some_and(|v| v != (l > 0)));
            let on_constants_only = bits.iter().all(|&b| expect[b].is_some());
            let syntactic = me.initial_syntactic(&cube);
            let expected = if contradicts {
                Some(false)
            } else if on_constants_only {
                Some(true)
            } else {
                None
            };
            assert_eq!(syntactic, expected, "cube {cube:?} bits {bits:?}");
            match syntactic {
                Some(v) => {
                    assert_eq!(v, initial_by_solver(&mut me, &cube), "cube {cube:?}");
                    if v {
                        sat += 1;
                    } else {
                        unsat += 1;
                    }
                }
                None => declined += 1,
            }
            // The shipped entry point agrees with the solver on every cube, decided or not.
            assert_eq!(
                me.initial(&cube).unwrap(),
                initial_by_solver(&mut me, &cube),
                "cube {cube:?}"
            );
        }
        assert!(
            unsat > 1_000 && sat > 20 && declined > 300,
            "unsat {unsat} sat {sat} declined {declined}"
        );
        assert_eq!(me.init_syntactic, (unsat + sat) as u64);
    }

    /// N5.6 — a cube `Init` implies is still not initial when the constraints leave `Init` no
    /// state: the syntactic SAT answer defers to one solver call on the empty cube, kept after.
    #[test]
    fn a_cube_init_implies_is_not_initial_when_the_constraints_empty_init() {
        let text = format!("{MIXED_INIT}22 neq 2 10 9\n23 constraint 22\n");
        let (model, stop) = search_of(&text);
        let mut me = Ic3::new(&model, stop);
        let cube = vec![-me.cur[0], me.cur[12]];
        assert_eq!(me.initial_syntactic(&cube), Some(true));
        assert!(!me.initial(&cube).unwrap());
        assert_eq!(me.init_nonempty, Some(false));
        let queries = me.queries;
        assert!(!me.initial(&cube).unwrap());
        assert_eq!(me.queries, queries, "the empty-cube answer is kept");
    }

    /// N5.6 — the initiation repair keeps its add-back order (the core it returns is the one the
    /// solver-based repair returned) and makes no solver call on constant-init literals; a
    /// literal on a register without a constant init still costs one call.
    #[test]
    fn the_initiation_repair_keeps_its_order_and_makes_no_call_on_constant_init_literals() {
        let (model, stop) = search_of(MIXED_INIT);
        let mut me = Ic3::new(&model, stop);
        // All on constant-init registers: `d[0]` agrees with `d = 5`, `a[0]` with `a = 0`,
        // `a[1]` contradicts. The walk keeps `a[0]` (it passes it before `a[1]`), as before N5.6.
        let cube = vec![me.cur[12], -me.cur[0], me.cur[1], me.cur[2]];
        let queries = me.queries;
        let core = me.repair_initiation_in(vec![me.cur[12]], &cube).unwrap();
        assert_eq!(core, vec![me.cur[12], -me.cur[0], me.cur[1]]);
        assert_eq!(
            me.queries - queries,
            1,
            "one solver call: whether Init has a state at all, asked once"
        );
        assert!(!initial_by_solver(&mut me, &core));
        let queries = me.queries;
        let core = me.repair_initiation_in(vec![-me.cur[3]], &cube).unwrap();
        assert_eq!(core, vec![-me.cur[3], me.cur[12], -me.cur[0], me.cur[1]]);
        assert_eq!(
            me.queries, queries,
            "no solver call on constant-init literals"
        );
        // `b[0] ∧ c[0]`, on the non-constant registers only, is the solver's: `c = a = 0`
        // initially, so the core grows to `b[0] ∧ c[0]` after the core's check and one add-back.
        let cube = vec![me.cur[4], me.cur[8]];
        let queries = me.queries;
        let core = me.repair_initiation_in(vec![me.cur[4]], &cube).unwrap();
        assert_eq!(core, vec![me.cur[4], me.cur[8]]);
        assert_eq!(
            me.queries - queries,
            2,
            "two solver calls: the core, then one add-back"
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
