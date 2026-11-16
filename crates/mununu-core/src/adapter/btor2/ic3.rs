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
    /// The simulator reached the falsified `bad` on the trace.
    Confirmed,
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
}

impl<'m> Ic3<'m> {
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
        match self.solver.solve_with(assumptions.iter().copied()) {
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
        let mut a = vec![self.act_init];
        a.extend_from_slice(cube);
        self.solve(&a)
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
    fn repair_initiation(&mut self, mut core: Vec<i32>, cube: &[i32]) -> Result<Vec<i32>, Halted> {
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
    /// of `Init`. Bounded in attempts; the order is the cube's.
    fn generalize(
        &mut self,
        core: Vec<i32>,
        cube: &[i32],
        level: usize,
    ) -> Result<Vec<i32>, Halted> {
        let mut g = self.repair_initiation(core, cube)?;
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
            match self.relative(n, &g)? {
                Rel::Inductive(_) => n += 1,
                Rel::Predecessor(_) => break,
            }
        }
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
            replay: Replay::Confirmed,
        })
    }

    /// Propagate every frame's clauses forward; `Some(inv)` when a frame empties (`F_i == F_{i+1}`).
    fn propagate(&mut self) -> Result<Option<Vec<Vec<i32>>>, Halted> {
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
        Ok(_) => Replay::Confirmed,
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
        outcome = kind,
        "ic3 search"
    );
    out
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
                assert_eq!(replay, Replay::Confirmed);
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
                assert_eq!(replay, Replay::Confirmed);
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
                assert_eq!(replay, Replay::Confirmed);
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
                assert_eq!(replay, Replay::Confirmed);
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
