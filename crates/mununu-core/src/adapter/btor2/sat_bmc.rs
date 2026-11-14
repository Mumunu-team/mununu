//! N5.0 (Track N) — bit-level BMC over the [`super::aig`] model, on CaDiCaL in-process.
//!
//! The first owned engine on the SAT core the owned IC3 will share. It answers the counterexample
//! side only: `Violated { depth }`, `NoCounterexample { depth }` (searched every depth up to it) or
//! `Stopped` (a budget, the deadline or a cancellation, with the depth reached). A bounded search
//! proves nothing about the depths it did not reach, so there is no `Safe` here.
//!
//! # Encoding
//!
//! One SAT solver, unrolled one frame at a time. Every AIG node is Tseitin-encoded lazily, per
//! frame, only when a `bad`, a `constraint`, or a node that reads one asks for it, so the unrolling
//! is the cone of influence by construction. A register bit at frame `k + 1` is the literal of
//! its `next` at frame `k` (no fresh variable); at frame 0 it is a fresh variable tied to its
//! `init` by two clauses, or left free when there is no `init`. A register with no `next` and
//! every input take a fresh variable in each frame. Constraints are unit clauses in every frame;
//! `bad` at depth `k` is solved under an assumption and then asserted false, so depth `k + 1`
//! inherits every refuted depth.
//!
//! The encoder is iterative (an explicit stack): a deep multiplier chain must not overflow the
//! stack.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::adapter::btor2::aig::{AigModel, AigNode, Lit};
use crate::adapter::btor2::ast::Btor2File;

/// How far and how long the search may go.
#[derive(Clone, Debug, Default)]
pub struct SatBmcOptions {
    /// The deepest frame checked (inclusive).
    pub max_depth: u32,
    /// CaDiCaL's conflict limit for each depth's query; `None` = unlimited. Deterministic, unlike
    /// the deadline: the same design and limit stop at the same depth on every host.
    pub conflicts_per_depth: Option<i32>,
    /// A wall-clock deadline for the whole search.
    pub deadline: Option<Instant>,
    /// An external cancellation, polled by the solver.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Item 6's C1: the dynamic step instead of one query per depth (see [`DynamicStep`]).
    pub dynamic_step: Option<DynamicStep>,
}

/// Item 6's C1 — the dynamic step (rIC3's `--bmc-dyn-step` idea, owned).
///
/// Each query asks for ANY not-yet-refuted `bad` up to a target depth, under a conflict budget.
/// UNSAT refutes every pending depth at once. A query that runs out of budget refutes nothing,
/// but is not a failure: its depths stay pending, and the next, deeper query covers them too.
/// The target jumps ahead geometrically. This is the lever for designs whose shallow UNSAT
/// frames are hard and whose counterexample is deep: HWMCC'20 `mul9` stalls plain BMC at depth 2,
/// and its counterexample is at depth 238.
///
/// SOUNDNESS: a SAT answer is a concrete path to a `bad` at the earliest depth whose `bad` is
/// true in the model, so `Violated` stays exact. `NoCounterexample` is reported only once every
/// depth up to `max_depth` was refuted; otherwise the outcome is `Stopped`.
#[derive(Clone, Debug)]
pub struct DynamicStep {
    /// The first query's conflict budget; it doubles after every query that runs out of budget.
    pub first_conflicts: i32,
}

impl Default for DynamicStep {
    fn default() -> Self {
        Self {
            first_conflicts: 20_000,
        }
    }
}

/// What the search established.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SatBmcOutcome {
    /// `bad` number `bad_index` holds at `depth` on a path that meets every constraint.
    /// `inputs[k]` is each input's value at frame `k`, and `initial` each register's value at
    /// frame 0, keyed as the concrete simulator keys them (the symbol, else `st_n<nid>` /
    /// `in_n<nid>`); a bit the solver never encoded reads 0.
    Violated {
        depth: u32,
        bad_index: usize,
        initial: Vec<(String, u128)>,
        inputs: Vec<Vec<(String, u128)>>,
    },
    /// No `bad` holds at any depth `0 ..= depth`.
    NoCounterexample { depth: u32 },
    /// The search stopped before `max_depth`; every depth below `depth` was refuted.
    Stopped { depth: u32, reason: String },
}

struct Stop {
    deadline: Option<Instant>,
    cancel: Option<Arc<AtomicBool>>,
}

impl cadical::Callbacks for Stop {
    fn terminate(&mut self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
            || self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// The unrolled solver.
struct Unroller<'m> {
    model: &'m AigModel,
    solver: cadical::Solver<Stop>,
    next_var: i32,
    true_lit: i32,
    /// `frames[k][node]` = the SAT literal of AIG node `node` at frame `k` (0 = not yet encoded).
    frames: Vec<Vec<i32>>,
    /// Leaf node → (state index, bit).
    state_bit: HashMap<u32, (usize, usize)>,
    /// Frame-0 register bits created but not yet tied to their `init`.
    pending_init: Vec<(usize, usize, i32)>,
}

impl<'m> Unroller<'m> {
    fn new(model: &'m AigModel, stop: Stop) -> Self {
        let mut solver: cadical::Solver<Stop> = cadical::Solver::new();
        solver.set_callbacks(Some(stop));
        solver.add_clause([1]);
        let state_bit = model
            .states
            .iter()
            .enumerate()
            .flat_map(|(si, s)| {
                s.leaves
                    .iter()
                    .enumerate()
                    .map(move |(bi, l)| (l.node(), (si, bi)))
            })
            .collect();
        Self {
            model,
            solver,
            next_var: 2,
            true_lit: 1,
            frames: Vec::new(),
            state_bit,
            pending_init: Vec::new(),
        }
    }

    fn fresh(&mut self) -> i32 {
        let v = self.next_var;
        self.next_var += 1;
        v
    }

    fn ensure_frame(&mut self, f: usize) {
        while self.frames.len() <= f {
            self.frames.push(vec![0; self.model.aig.len()]);
        }
    }

    fn cached(&self, n: u32, f: usize) -> i32 {
        self.frames
            .get(f)
            .and_then(|fr| fr.get(n as usize))
            .copied()
            .unwrap_or(0)
    }

    fn signed(lit: Lit, v: i32) -> i32 {
        if lit.is_complemented() { -v } else { v }
    }

    /// The SAT literal of `lit` at frame `f`, encoding its cone as needed, then tying any
    /// frame-0 register bits it created to their `init`.
    fn encode(&mut self, lit: Lit, f: usize) -> Result<i32, String> {
        let v = self.encode_node(lit.node(), f)?;
        while let Some((si, bi, var)) = self.pending_init.pop() {
            let init = self.model.states[si]
                .init
                .as_ref()
                .and_then(|i| i.get(bi).copied());
            if let Some(il) = init {
                let iv = self.encode_node(il.node(), 0)?;
                let iv = Self::signed(il, iv);
                self.solver.add_clause([-var, iv]);
                self.solver.add_clause([var, -iv]);
            }
        }
        Ok(Self::signed(lit, v))
    }

    fn encode_node(&mut self, root: u32, root_f: usize) -> Result<i32, String> {
        let mut stack: Vec<(u32, usize)> = vec![(root, root_f)];
        while let Some(&(n, f)) = stack.last() {
            self.ensure_frame(f);
            if self.cached(n, f) != 0 {
                stack.pop();
                continue;
            }
            let node = self
                .model
                .aig
                .node(n)
                .ok_or_else(|| format!("sat-bmc: AIG node {n} out of range"))?;
            let value = match node {
                AigNode::False => Some(-self.true_lit),
                AigNode::Leaf => match self.state_bit.get(&n).copied() {
                    Some((si, bi)) => {
                        let next = self.model.states[si]
                            .next
                            .as_ref()
                            .and_then(|nx| nx.get(bi).copied());
                        match (f, next) {
                            (0, _) => {
                                let v = self.fresh();
                                self.pending_init.push((si, bi, v));
                                Some(v)
                            }
                            (_, Some(d)) => {
                                let dv = self.cached(d.node(), f - 1);
                                if dv == 0 {
                                    stack.push((d.node(), f - 1));
                                    None
                                } else {
                                    Some(Self::signed(d, dv))
                                }
                            }
                            // A register with no `next` is free in every frame.
                            (_, None) => Some(self.fresh()),
                        }
                    }
                    None => Some(self.fresh()),
                },
                AigNode::And(a, b) => {
                    let (av, bv) = (self.cached(a.node(), f), self.cached(b.node(), f));
                    if av == 0 || bv == 0 {
                        if av == 0 {
                            stack.push((a.node(), f));
                        }
                        if bv == 0 {
                            stack.push((b.node(), f));
                        }
                        None
                    } else {
                        let (x, y) = (Self::signed(a, av), Self::signed(b, bv));
                        let v = self.fresh();
                        self.solver.add_clause([-v, x]);
                        self.solver.add_clause([-v, y]);
                        self.solver.add_clause([v, -x, -y]);
                        Some(v)
                    }
                }
            };
            if let Some(v) = value {
                self.frames[f][n as usize] = v;
                stack.pop();
            }
        }
        Ok(self.cached(root, root_f))
    }

    /// The value of `leaves` in the solver's model at frame `f` (an unencoded bit reads 0).
    fn word(&self, leaves: &[Lit], f: usize) -> u128 {
        leaves
            .iter()
            .enumerate()
            .take(128)
            .map(|(i, l)| {
                let v = self.cached(l.node(), f);
                let bit = v != 0 && self.solver.value(v) == Some(true);
                u128::from(bit ^ l.is_complemented()) << i
            })
            .sum()
    }

    /// The concrete simulator's step name: the symbol, else `st_n<nid>` / `in_n<nid>`.
    fn name(symbol: &Option<String>, prefix: &str, nid: crate::adapter::btor2::ast::Nid) -> String {
        symbol.clone().unwrap_or_else(|| format!("{prefix}_n{nid}"))
    }
}

/// The counterexample the solver's model holds, for a `bad` that is true at frame `f`.
fn counterexample(u: &Unroller, model: &AigModel, f: usize, bad_index: usize) -> SatBmcOutcome {
    let initial = model
        .states
        .iter()
        .map(|s| (Unroller::name(&s.symbol, "st", s.nid), u.word(&s.leaves, 0)))
        .collect();
    let inputs = (0..=f)
        .map(|fr| {
            model
                .inputs
                .iter()
                .map(|i| {
                    (
                        Unroller::name(&i.symbol, "in", i.nid),
                        u.word(&i.leaves, fr),
                    )
                })
                .collect()
        })
        .collect();
    SatBmcOutcome::Violated {
        depth: f as u32,
        bad_index,
        initial,
        inputs,
    }
}

/// The [`DynamicStep`] search.
fn dynamic_search(
    u: &mut Unroller,
    model: &AigModel,
    opts: &SatBmcOptions,
    step: &DynamicStep,
    halted: &dyn Fn() -> bool,
    stopped: &dyn Fn(u32, &str) -> SatBmcOutcome,
) -> Result<SatBmcOutcome, String> {
    // `(frame, bad index, literal)` for every bad not yet refuted.
    let mut pending: Vec<(usize, usize, i32)> = Vec::new();
    // Every depth below `refuted_below` is refuted; `encoded` frames are in the solver.
    let (mut refuted_below, mut encoded) = (0u32, 0u32);
    let mut jump = 1u32;
    let mut conflicts = step.first_conflicts.max(1);
    loop {
        if halted() {
            return Ok(stopped(refuted_below, "stopped"));
        }
        let target = encoded
            .saturating_add(jump)
            .min(opts.max_depth.saturating_add(1));
        for k in encoded..target {
            let f = k as usize;
            for &c in &model.constraints {
                let cl = u.encode(c, f)?;
                u.solver.add_clause([cl]);
            }
            for (bi, &b) in model.bads.iter().enumerate() {
                let l = u.encode(b, f)?;
                pending.push((f, bi, l));
            }
        }
        encoded = target;
        let any = u.fresh();
        u.solver
            .add_clause(std::iter::once(-any).chain(pending.iter().map(|&(_, _, l)| l)));
        u.solver
            .set_limit("conflicts", conflicts)
            .map_err(|e| format!("sat-bmc: {}", e.msg))?;
        match u.solver.solve_with([any]) {
            Some(true) => {
                let hit = pending
                    .iter()
                    .filter(|&&(_, _, l)| u.solver.value(l) == Some(true))
                    .min_by_key(|&&(f, bi, _)| (f, bi))
                    .copied();
                let (f, bi, _) = hit.ok_or_else(|| {
                    "sat-bmc: the dynamic-step query was SAT with no pending bad true".to_string()
                })?;
                return Ok(counterexample(u, model, f, bi));
            }
            Some(false) => {
                for &(_, _, l) in &pending {
                    u.solver.add_clause([-l]);
                }
                pending.clear();
                refuted_below = encoded;
                if encoded > opts.max_depth {
                    return Ok(SatBmcOutcome::NoCounterexample {
                        depth: opts.max_depth,
                    });
                }
            }
            None => {
                if halted() {
                    return Ok(stopped(refuted_below, "stopped"));
                }
                // Out of budget: nothing refuted, the window grows. At the last depth the only
                // way forward is a bigger budget.
                conflicts = conflicts.saturating_mul(2);
            }
        }
        // The query's `any` stays in the solver, inert: it is only ever assumed true.
        u.solver.add_clause([-any]);
        jump = jump.saturating_mul(2);
    }
}

/// A `Violated` outcome as a BTOR2 witness (the HWMCC format `btorsim -c` checks): `sat`, the
/// bad property `b<i>`, frame 0's state assignments by state position, then each frame's input
/// assignments by input position, MSB-first binary. `None` for any other outcome.
pub fn btor2_witness(model: &AigModel, outcome: &SatBmcOutcome) -> Option<String> {
    let SatBmcOutcome::Violated {
        bad_index,
        initial,
        inputs,
        ..
    } = outcome
    else {
        return None;
    };
    let bits = |v: u128, w: usize| -> String {
        (0..w)
            .rev()
            .map(|i| {
                if i < 128 && (v >> i) & 1 == 1 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect()
    };
    let mut out = format!("sat\nb{bad_index}\n#0\n");
    for (i, (s, (_, v))) in model.states.iter().zip(initial).enumerate() {
        out.push_str(&format!("{i} {}\n", bits(*v, s.leaves.len())));
    }
    for (f, frame) in inputs.iter().enumerate() {
        out.push_str(&format!("@{f}\n"));
        for (i, (inp, (_, v))) in model.inputs.iter().zip(frame).enumerate() {
            out.push_str(&format!("{i} {}\n", bits(*v, inp.leaves.len())));
        }
    }
    out.push_str(".\n");
    Some(out)
}

/// Bit-level BMC on `file` up to `opts.max_depth`. `Err` = the design cannot be modelled (arrays,
/// an operator the AIG backend refuses), with the reason.
pub fn sat_bmc(file: &Btor2File, opts: &SatBmcOptions) -> Result<SatBmcOutcome, String> {
    let model = AigModel::build(file)?;
    sat_bmc_model(&model, opts)
}

/// [`sat_bmc`] over an already-built model.
pub fn sat_bmc_model(model: &AigModel, opts: &SatBmcOptions) -> Result<SatBmcOutcome, String> {
    let halted = || {
        opts.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
            || opts.deadline.is_some_and(|d| Instant::now() >= d)
    };
    let stopped = |depth: u32, why_budget: &str| SatBmcOutcome::Stopped {
        depth,
        reason: if opts
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            "cancelled".to_string()
        } else if opts.deadline.is_some_and(|d| Instant::now() >= d) {
            "deadline reached".to_string()
        } else {
            why_budget.to_string()
        },
    };
    let mut u = Unroller::new(
        model,
        Stop {
            deadline: opts.deadline,
            cancel: opts.cancel.clone(),
        },
    );
    if model.bads.is_empty() {
        return Ok(SatBmcOutcome::NoCounterexample {
            depth: opts.max_depth,
        });
    }
    if let Some(dyn_step) = &opts.dynamic_step {
        return dynamic_search(&mut u, model, opts, dyn_step, &halted, &stopped);
    }
    for k in 0..=opts.max_depth {
        if halted() {
            return Ok(stopped(k, "stopped"));
        }
        let f = k as usize;
        for &c in &model.constraints {
            let cl = u.encode(c, f)?;
            u.solver.add_clause([cl]);
        }
        let mut bad_lits = Vec::with_capacity(model.bads.len());
        for &b in &model.bads {
            bad_lits.push(u.encode(b, f)?);
        }
        let any = if bad_lits.len() == 1 {
            bad_lits[0]
        } else {
            let v = u.fresh();
            u.solver
                .add_clause(std::iter::once(-v).chain(bad_lits.iter().copied()));
            v
        };
        if let Some(limit) = opts.conflicts_per_depth {
            u.solver
                .set_limit("conflicts", limit)
                .map_err(|e| format!("sat-bmc: {}", e.msg))?;
        }
        match u.solver.solve_with([any]) {
            Some(true) => {
                let bad_index = bad_lits
                    .iter()
                    .position(|&l| u.solver.value(l) == Some(true))
                    .unwrap_or(0);
                return Ok(counterexample(&u, model, f, bad_index));
            }
            Some(false) => u.solver.add_clause([-any]),
            None => {
                return Ok(stopped(
                    k,
                    &format!(
                        "the conflict budget ({} per depth) ran out at depth {k}",
                        opts.conflicts_per_depth.unwrap_or(0)
                    ),
                ));
            }
        }
    }
    Ok(SatBmcOutcome::NoCounterexample {
        depth: opts.max_depth,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;

    fn run(text: &str, max_depth: u32) -> SatBmcOutcome {
        sat_bmc(
            &parser::parse(text).unwrap(),
            &SatBmcOptions {
                max_depth,
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// A 3-bit counter from 0: `c = 7` first holds at depth 7.
    const COUNTER: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 one 1\n\
                           6 add 1 3 5\n7 next 1 3 6\n8 ones 1\n9 sort bitvec 1\n\
                           10 eq 9 3 8\n11 bad 10\n";

    #[test]
    fn the_counterexample_is_found_at_its_first_depth() {
        assert_eq!(
            run(COUNTER, 6),
            SatBmcOutcome::NoCounterexample { depth: 6 }
        );
        match run(COUNTER, 20) {
            SatBmcOutcome::Violated { depth, initial, .. } => {
                assert_eq!(depth, 7);
                assert_eq!(initial, vec![("c".to_string(), 0)]);
            }
            other => panic!("{other:?}"),
        }
    }

    /// An input steers the counter: it steps only while `go` holds, and a constraint forbids
    /// `go` at frame… every frame, so `bad` is never reached.
    #[test]
    fn a_constraint_holds_in_every_frame() {
        let text = "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 c\n5 init 1 4 3\n\
                    6 input 2 go\n7 one 1\n8 add 1 4 7\n9 ite 1 6 8 4\n10 next 1 4 9\n\
                    11 ones 1\n12 eq 2 4 11\n13 bad 12\n14 not 2 6\n15 constraint 14\n";
        assert_eq!(run(text, 12), SatBmcOutcome::NoCounterexample { depth: 12 });
        // Without the constraint, `go` every step reaches 7 at depth 7, and the trace says so.
        let free = text.replace("15 constraint 14\n", "");
        match run(&free, 12) {
            SatBmcOutcome::Violated { depth, inputs, .. } => {
                assert_eq!(depth, 7);
                assert!(
                    inputs[..7]
                        .iter()
                        .all(|fr| fr == &vec![("go".to_string(), 1)]),
                    "{inputs:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// A register with no `init` starts anywhere: `bad` (c = 5) holds at depth 0.
    #[test]
    fn a_register_without_init_is_free_at_frame_zero() {
        let text = "1 sort bitvec 3\n2 sort bitvec 1\n3 state 1 c\n4 next 1 3 3\n5 constd 1 5\n\
                    6 eq 2 3 5\n7 bad 6\n";
        match run(text, 3) {
            SatBmcOutcome::Violated { depth, initial, .. } => {
                assert_eq!((depth, initial), (0, vec![("c".to_string(), 5)]));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_cancelled_search_stops_and_says_so() {
        let cancel = Arc::new(AtomicBool::new(true));
        let out = sat_bmc(
            &parser::parse(COUNTER).unwrap(),
            &SatBmcOptions {
                max_depth: 20,
                cancel: Some(cancel),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            out,
            SatBmcOutcome::Stopped {
                depth: 0,
                reason: "cancelled".into()
            }
        );
    }

    /// A deep chain of frames must not recurse: 3,000 frames of a 16-bit counter.
    #[test]
    fn a_deep_unrolling_does_not_overflow_the_stack() {
        let text = "1 sort bitvec 16\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 one 1\n\
                    6 add 1 3 5\n7 next 1 3 6\n8 constd 1 3000\n9 sort bitvec 1\n\
                    10 eq 9 3 8\n11 bad 10\n";
        // Encode the deepest frame first: the encoder walks all 3,000 frames from one root.
        let model = AigModel::build(&parser::parse(text).unwrap()).unwrap();
        let mut u = Unroller::new(
            &model,
            Stop {
                deadline: None,
                cancel: None,
            },
        );
        let bad = u.encode(model.bads[0], 3000).unwrap();
        assert_eq!(u.solver.solve_with([bad]), Some(true));
    }

    fn run_dyn(text: &str, max_depth: u32, first_conflicts: i32) -> SatBmcOutcome {
        sat_bmc(
            &parser::parse(text).unwrap(),
            &SatBmcOptions {
                max_depth,
                dynamic_step: Some(DynamicStep { first_conflicts }),
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// The dynamic step finds the same first-depth counterexample: a window that covers depth 7
    /// reports the earliest pending bad true in the model, not the window's end.
    #[test]
    fn the_dynamic_step_reports_the_earliest_true_depth_in_its_window() {
        // A saturating counter: once 7, it stays 7, so every depth ≥ 7 is a model of `bad`.
        let sat_counter = "1 sort bitvec 3\n2 zero 1\n3 state 1 c\n4 init 1 3 2\n5 one 1\n\
                           6 add 1 3 5\n8 ones 1\n9 sort bitvec 1\n10 eq 9 3 8\n\
                           11 ite 1 10 3 6\n12 next 1 3 11\n13 bad 10\n";
        match run_dyn(sat_counter, 100, 1_000) {
            SatBmcOutcome::Violated { depth, .. } => assert_eq!(depth, 7),
            other => panic!("{other:?}"),
        }
        match run_dyn(COUNTER, 100, 1_000) {
            SatBmcOutcome::Violated { depth, .. } => assert_eq!(depth % 8, 7),
            other => panic!("{other:?}"),
        }
    }

    /// Refuting every window up to the bound is the only way to `NoCounterexample`.
    #[test]
    fn the_dynamic_step_refutes_to_the_bound_or_says_it_stopped() {
        assert_eq!(
            run_dyn(COUNTER, 6, 1_000),
            SatBmcOutcome::NoCounterexample { depth: 6 }
        );
        let cancel = Arc::new(AtomicBool::new(true));
        let out = sat_bmc(
            &parser::parse(COUNTER).unwrap(),
            &SatBmcOptions {
                max_depth: 100,
                cancel: Some(cancel),
                dynamic_step: Some(DynamicStep::default()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(matches!(out, SatBmcOutcome::Stopped { .. }), "{out:?}");
    }

    /// The witness is the standard format: the repo's own witness parser reads it and its
    /// concrete replay reaches the (named) `bad`.
    #[test]
    fn the_witness_parses_and_replays_to_bad() {
        use crate::adapter::btor2::witness::{parse_btor2_witness, trace_from_witness};
        let named = COUNTER.replace(
            "10 eq 9 3 8\n11 bad 10\n",
            "10 eq 9 3 8 at_seven\n11 bad 10\n",
        );
        let file = parser::parse(&named).unwrap();
        let model = AigModel::build(&file).unwrap();
        let out = sat_bmc_model(
            &model,
            &SatBmcOptions {
                max_depth: 20,
                ..Default::default()
            },
        )
        .unwrap();
        let w = btor2_witness(&model, &out).expect("violated has a witness");
        let parsed = parse_btor2_witness(&w).expect("parses");
        trace_from_witness(&file, &parsed).expect("replays to bad");
        assert!(btor2_witness(&model, &SatBmcOutcome::NoCounterexample { depth: 3 }).is_none());
    }
}
