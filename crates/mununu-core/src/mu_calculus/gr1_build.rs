//! General TLSF-spec → GR(1) game construction.
//!
//! Turns a set of LTL assumptions/guarantees (plus the input/output signal
//! lists) into the monitor-augmented two-player game that [`super::gr1`] solves.
//! The base plant is memoryless — each round the environment picks an input
//! valuation and the controller an output valuation — so the entire game state
//! is the **monitor bits**:
//!   - one `pre_prev` bit per transition-safety clause `G(pre → X post)`;
//!   - one `pending` bit per response clause `G(trig → F resp)`.
//!
//! Supported GR(1) fragment (rejected clauses are reported, not silently
//! dropped — the honest fragment boundary):
//!   - invariant safety `G(prop)`
//!   - transition safety `G(pre → X post)`  (`pre`, `post` propositional)
//!   - input fairness `G F prop` / `GF prop` over inputs  → `env_fair`
//!   - response `G(trig → F resp)`                         → `sys_fair` via `¬pending`
//!
//! `GF` guarantees over outputs, `U`/`R`/`W`, and `FG` are out of scope for now
//! (would need extra monitor bits); they are returned as classification errors.

use std::collections::HashMap;

use bitvec::prelude::{Lsb0, bitvec};

use crate::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, LabelControllability};
use crate::ltl::ast::LtlFormula;

use super::gr1::StateSet;

// ---------------------------------------------------------------------------
// Propositional layer
// ---------------------------------------------------------------------------

/// True when `f` contains no temporal operator (pure Boolean over predicates).
fn is_propositional(f: &LtlFormula) -> bool {
    use LtlFormula::*;
    match f {
        True | False | Predicate(_) => true,
        Not(a) => is_propositional(a),
        And(a, b) | Or(a, b) | Implies(a, b) => is_propositional(a) && is_propositional(b),
        _ => false,
    }
}

/// Evaluate a propositional formula under a signal valuation (absent ⇒ false).
fn eval_prop(f: &LtlFormula, val: &HashMap<String, bool>) -> bool {
    use LtlFormula::*;
    match f {
        True => true,
        False => false,
        Predicate(p) => *val.get(p).unwrap_or(&false),
        Not(a) => !eval_prop(a, val),
        And(a, b) => eval_prop(a, val) && eval_prop(b, val),
        Or(a, b) => eval_prop(a, val) || eval_prop(b, val),
        Implies(a, b) => !eval_prop(a, val) || eval_prop(b, val),
        // Non-propositional nodes never reach here after classification.
        _ => false,
    }
}

/// Collect the predicate names referenced by a formula.
fn predicates_of(f: &LtlFormula, out: &mut Vec<String>) {
    use LtlFormula::*;
    match f {
        Predicate(p) => {
            if !out.contains(p) {
                out.push(p.clone());
            }
        }
        Not(a) => predicates_of(a, out),
        And(a, b) | Or(a, b) | Implies(a, b) => {
            predicates_of(a, out);
            predicates_of(b, out);
        }
        Next(a) | Always(a) | Eventually(a) | Recurrence(a) | Stabilization(a) => {
            predicates_of(a, out)
        }
        Until { left, right } | WeakUntil { left, right } | Release { left, right } => {
            predicates_of(left, out);
            predicates_of(right, out);
        }
        Response { trigger, response } => {
            predicates_of(trigger, out);
            predicates_of(response, out);
        }
        True | False => {}
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

enum SafetyClause {
    Invariant(LtlFormula),
    Transition { pre: LtlFormula, post: LtlFormula },
}

struct ResponseClause {
    trigger: LtlFormula,
    response: LtlFormula,
}

/// A spec classified into GR(1) normal form.
pub struct Gr1Spec {
    inputs: Vec<String>,
    outputs: Vec<String>,
    safety: Vec<SafetyClause>,
    responses: Vec<ResponseClause>,
    /// Propositional recurrence sets from `GF` assumptions (over inputs).
    env_fair: Vec<LtlFormula>,
}

impl Gr1Spec {
    /// Classify LTL assumptions/guarantees into the GR(1) fragment, or return
    /// the first clause that falls outside it.
    pub fn classify(
        assumptions: &[LtlFormula],
        guarantees: &[LtlFormula],
        inputs: &[String],
        outputs: &[String],
    ) -> Result<Gr1Spec, String> {
        let mut spec = Gr1Spec {
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            safety: Vec::new(),
            responses: Vec::new(),
            env_fair: Vec::new(),
        };
        for a in assumptions {
            spec.classify_one(a, false)?;
        }
        for g in guarantees {
            spec.classify_one(g, true)?;
        }
        Ok(spec)
    }

    /// mununu#1121 U2.10 — a clause outside the GR(1) fragment: a limit of the technique, with
    /// the full μ-calculus mode as the way out.
    fn outside_fragment(what: &str) -> String {
        crate::adapter::owner::technique_limit(
            &format!("{what} is outside the GR(1) fragment this mode decides"),
            "`--controller-mode product-game` on `context synth` decides the full μ-calculus",
        )
    }

    fn classify_one(&mut self, f: &LtlFormula, is_guarantee: bool) -> Result<(), String> {
        use LtlFormula::*;
        match f {
            // GF prop
            Recurrence(inner) if is_propositional(inner) => self.push_fair(inner, is_guarantee),
            Always(inner) => self.classify_always(inner, is_guarantee),
            // G(trig -> F resp)
            Response { trigger, response }
                if is_propositional(trigger) && is_propositional(response) =>
            {
                self.push_response(trigger, response, is_guarantee)
            }
            other => Err(Self::outside_fragment(&format!("clause `{other}`"))),
        }
    }

    fn classify_always(&mut self, body: &LtlFormula, is_guarantee: bool) -> Result<(), String> {
        use LtlFormula::*;
        match body {
            // G(F prop) or G(GF prop) → fairness
            Eventually(inner) | Recurrence(inner) if is_propositional(inner) => {
                self.push_fair(inner, is_guarantee)
            }
            // G(pre -> ...) → transition safety or response
            Implies(pre, rhs) if is_propositional(pre) => match &**rhs {
                Next(post) if is_propositional(post) => {
                    self.safety.push(SafetyClause::Transition {
                        pre: (**pre).clone(),
                        post: (**post).clone(),
                    });
                    Ok(())
                }
                Eventually(resp) if is_propositional(resp) => {
                    self.push_response(pre, resp, is_guarantee)
                }
                other => Err(Self::outside_fragment(&format!(
                    "clause `G(pre -> {other})`"
                ))),
            },
            // G(prop) → invariant
            p if is_propositional(p) => {
                self.safety.push(SafetyClause::Invariant(p.clone()));
                Ok(())
            }
            other => Err(Self::outside_fragment(&format!("clause `G({other})`"))),
        }
    }

    fn push_fair(&mut self, prop: &LtlFormula, is_guarantee: bool) -> Result<(), String> {
        if is_guarantee {
            // A GF guarantee over outputs needs a last-output monitor bit (not
            // yet built). Only reject if it actually mentions an output.
            let mut ps = Vec::new();
            predicates_of(prop, &mut ps);
            if ps.iter().any(|p| self.outputs.contains(p)) {
                return Err(crate::adapter::owner::mununu_limitation(
                    &format!(
                        "GF guarantee over outputs (`{prop}`) is inside the fragment but this \
                         mode does not build the last-output monitor it needs"
                    ),
                    Some("`--controller-mode product-game` on `context synth`"),
                ));
            }
            // GF over inputs as a *guarantee* is degenerate but harmless — treat
            // like an env recurrence the controller cannot influence. Reject to
            // stay honest.
            return Err(crate::adapter::owner::technique_limit(
                &format!(
                    "GF guarantee over non-output signals (`{prop}`) is not a controller \
                     obligation, since the controller drives no signal in it"
                ),
                "move it to the assumptions",
            ));
        }
        self.env_fair.push(prop.clone());
        Ok(())
    }

    fn push_response(
        &mut self,
        trigger: &LtlFormula,
        response: &LtlFormula,
        is_guarantee: bool,
    ) -> Result<(), String> {
        if !is_guarantee {
            return Err(Self::outside_fragment(
                "a `G(a -> F b)` response as an assumption",
            ));
        }
        if !is_propositional(trigger) || !is_propositional(response) {
            return Err(Self::outside_fragment(
                "a response whose trigger or response is temporal",
            ));
        }
        self.responses.push(ResponseClause {
            trigger: trigger.clone(),
            response: response.clone(),
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Game construction
// ---------------------------------------------------------------------------

/// The monitor-augmented GR(1) game built from a [`Gr1Spec`], plus the metadata
/// needed to project a strategy back to a Mealy controller.
pub struct Gr1Game {
    pub clts: Clts<DefaultStateIdx, DefaultLabelIdx>,
    pub safe: StateSet,
    pub sys_fair: Vec<StateSet>,
    pub env_fair: Vec<StateSet>,
    pub init: usize,
    /// For a ctrl-turn state index: `(monitor_bits, input_valuation)`.
    pub ctrl_meta: HashMap<usize, (usize, u32)>,
    /// For a ctrl transition target: the output valuation it commits.
    pub move_output: HashMap<(usize, usize), u32>,
    /// For an env-turn state index: its monitor-bit value.
    pub e_meta: HashMap<usize, usize>,
    pub n_monitors: usize,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

impl Gr1Game {
    /// Build the game. Monitor bit layout: `[0..n_ts)` = transition-safety
    /// `pre_prev` bits, `[n_ts..n_ts+n_resp)` = response `pending` bits.
    // `m` is the monitor bit-state (used in bit arithmetic throughout), not a
    // mere collection index, so range loops over it are the correct form.
    #[allow(clippy::needless_range_loop)]
    pub fn build(spec: &Gr1Spec) -> Gr1Game {
        let n_ts = spec
            .safety
            .iter()
            .filter(|c| matches!(c, SafetyClause::Transition { .. }))
            .count();
        let n_resp = spec.responses.len();
        let n_monitors = n_ts + n_resp;
        let n_in = spec.inputs.len();
        let n_out = spec.outputs.len();
        let n_mon_states = 1usize << n_monitors;
        let n_in_vals = 1u32 << n_in;
        let n_out_vals = 1u32 << n_out;

        // Index the transition-safety clauses (for pre_prev bit positions).
        let ts_clauses: Vec<(&LtlFormula, &LtlFormula)> = spec
            .safety
            .iter()
            .filter_map(|c| match c {
                SafetyClause::Transition { pre, post } => Some((pre, post)),
                _ => None,
            })
            .collect();
        let inv_clauses: Vec<&LtlFormula> = spec
            .safety
            .iter()
            .filter_map(|c| match c {
                SafetyClause::Invariant(p) => Some(p),
                _ => None,
            })
            .collect();

        let mut b = Clts::builder();
        // Labels: one env_{k} per input valuation, one ctrl_{k} per output valuation, plus bad.
        let env_labels: Vec<_> = (0..n_in_vals)
            .map(|k| {
                let l = b.labels().intern([format!("env_{k}")]).unwrap();
                b.set_label_controllability(l, LabelControllability::Uncontrollable);
                l
            })
            .collect();
        let ctrl_labels: Vec<_> = (0..n_out_vals)
            .map(|k| {
                let l = b.labels().intern([format!("ctrl_{k}")]).unwrap();
                b.set_label_controllability(l, LabelControllability::Controllable);
                l
            })
            .collect();
        let bad_label = b.labels().intern(["bad"]).unwrap();
        b.set_label_controllability(bad_label, LabelControllability::Uncontrollable);

        // Allocate states.
        let mut e_state = vec![None; n_mon_states];
        let mut c_state: HashMap<(usize, u32), _> = HashMap::new();
        for m in 0..n_mon_states {
            e_state[m] = Some(b.state_with_name(format!("E{m}")).unwrap());
        }
        for m in 0..n_mon_states {
            for iv in 0..n_in_vals {
                c_state.insert((m, iv), b.state_with_name(format!("C{m}_{iv}")).unwrap());
            }
        }
        let bad = b.state_with_name("BAD".into()).unwrap();
        let init_state = e_state[0].unwrap(); // all monitors clear
        b.initial_state_id(init_state);

        // Signal valuation helper for a (input_val, output_val) pair.
        let signal_val = |iv: u32, ov: u32| -> HashMap<String, bool> {
            let mut val = HashMap::new();
            for (bit, name) in spec.inputs.iter().enumerate() {
                val.insert(name.clone(), (iv >> bit) & 1 == 1);
            }
            for (bit, name) in spec.outputs.iter().enumerate() {
                val.insert(name.clone(), (ov >> bit) & 1 == 1);
            }
            val
        };

        let mut ctrl_meta = HashMap::new();
        let mut move_output = HashMap::new();

        // env moves: E[m] -env_iv-> C[m,iv]
        for m in 0..n_mon_states {
            let e = e_state[m].unwrap();
            for iv in 0..n_in_vals {
                let c = c_state[&(m, iv)];
                b.transition_ids(e, &[env_labels[iv as usize]], c);
                ctrl_meta.insert(c.index(), (m, iv));
            }
        }
        // ctrl moves: C[m,iv] -ctrl_ov-> E[m'] or BAD
        for m in 0..n_mon_states {
            for iv in 0..n_in_vals {
                let c = c_state[&(m, iv)];
                for ov in 0..n_out_vals {
                    let val = signal_val(iv, ov);
                    // safety: invariants + transition safeties.
                    let mut unsafe_move = inv_clauses.iter().any(|p| !eval_prop(p, &val));
                    for (i, (pre, post)) in ts_clauses.iter().enumerate() {
                        let pre_prev = (m >> i) & 1 == 1;
                        if pre_prev && !eval_prop(post, &val) {
                            unsafe_move = true;
                        }
                        let _ = pre; // used below for the next-state update
                    }
                    let target = if unsafe_move {
                        bad
                    } else {
                        // next monitor bits
                        let mut m2 = 0usize;
                        for (i, (pre, _post)) in ts_clauses.iter().enumerate() {
                            if eval_prop(pre, &val) {
                                m2 |= 1 << i;
                            }
                        }
                        for (j, resp) in spec.responses.iter().enumerate() {
                            let pending = (m >> (n_ts + j)) & 1 == 1;
                            let trig = eval_prop(&resp.trigger, &val);
                            let done = eval_prop(&resp.response, &val);
                            let pending2 = (pending || trig) && !done;
                            if pending2 {
                                m2 |= 1 << (n_ts + j);
                            }
                        }
                        e_state[m2].unwrap()
                    };
                    b.transition_ids(c, &[ctrl_labels[ov as usize]], target);
                    move_output.insert((c.index(), target.index()), ov);
                }
            }
        }
        // BAD self-loop.
        b.transition_ids(bad, &[bad_label], bad);

        let clts = b.build().unwrap();
        let n = clts.state_count();
        let mut safe = bitvec![usize, Lsb0; 1; n];
        safe.set(bad.index(), false);

        // env_fair: recurrence sets over inputs, observed at C states.
        let env_fair: Vec<StateSet> = spec
            .env_fair
            .iter()
            .map(|prop| {
                let mut s = bitvec![usize, Lsb0; 0; n];
                for m in 0..n_mon_states {
                    for iv in 0..n_in_vals {
                        // inputs only: output bits absent → treated as false.
                        let val = signal_val(iv, 0);
                        if eval_prop(prop, &val) {
                            s.set(c_state[&(m, iv)].index(), true);
                        }
                    }
                }
                s
            })
            .collect();

        // sys_fair: one recurrence set per response = { E[m] : pending_j == 0 }.
        let sys_fair: Vec<StateSet> = (0..n_resp)
            .map(|j| {
                let mut s = bitvec![usize, Lsb0; 0; n];
                for m in 0..n_mon_states {
                    if (m >> (n_ts + j)) & 1 == 0 {
                        s.set(e_state[m].unwrap().index(), true);
                    }
                }
                s
            })
            .collect();

        let e_meta: HashMap<usize, usize> = (0..n_mon_states)
            .map(|m| (e_state[m].unwrap().index(), m))
            .collect();

        Gr1Game {
            clts,
            safe,
            sys_fair,
            env_fair,
            init: init_state.index(),
            ctrl_meta,
            move_output,
            e_meta,
            n_monitors,
            inputs: spec.inputs.clone(),
            outputs: spec.outputs.clone(),
        }
    }

    /// Emit the synthesized controller as a self-contained SystemVerilog Mealy
    /// module, given the winning region `z` and the single-guarantee strategy
    /// `strat` (`ctrl_state_index → chosen target index`). The module has one
    /// `logic` input per input signal, one output per output signal, and a
    /// `[n_monitors]`-bit `mon` state register. For each reachable
    /// `(monitor, inputs)` the strategy fixes the outputs and the next monitor
    /// state; everything else holds. Deterministic and directly BMC-able.
    pub fn emit_mealy_sv(
        &self,
        module: &str,
        z: &StateSet,
        strat: &HashMap<usize, usize>,
    ) -> String {
        use std::fmt::Write as _;
        let n_in = self.inputs.len();
        let n_out = self.outputs.len();
        let n_mon = self.n_monitors.max(1);
        let mut s = String::new();
        writeln!(
            s,
            "// Generated by Mununu GR(1) synthesis — sound-by-construction"
        )
        .unwrap();
        write!(s, "module {module} (input logic clk").unwrap();
        for inp in &self.inputs {
            write!(s, ", input logic {inp}").unwrap();
        }
        for out in &self.outputs {
            write!(s, ", output logic {out}").unwrap();
        }
        writeln!(s, ");").unwrap();
        writeln!(s, "  logic [{}:0] mon = {n_mon}'d0;", n_mon - 1).unwrap();
        writeln!(s, "  logic [{}:0] mon_next;", n_mon - 1).unwrap();
        // Output + next-monitor combinational logic.
        for out in &self.outputs {
            writeln!(s, "  logic {out}_c;").unwrap();
        }
        writeln!(s, "  always_comb begin").unwrap();
        for out in &self.outputs {
            writeln!(s, "    {out}_c = 1'b0;").unwrap();
        }
        writeln!(s, "    mon_next = mon;").unwrap();
        writeln!(s, "    unique case (mon)").unwrap();
        // group ctrl states by monitor value
        let mut by_mon: std::collections::BTreeMap<usize, Vec<(u32, usize)>> = Default::default();
        for (&c, &(m, iv)) in &self.ctrl_meta {
            if z[c] {
                by_mon.entry(m).or_default().push((iv, c));
            }
        }
        for (m, mut ivs) in by_mon {
            ivs.sort_by_key(|(iv, _)| *iv);
            writeln!(s, "      {n_mon}'d{m}: begin").unwrap();
            for (iv, c) in ivs {
                let Some(&t) = strat.get(&c) else { continue };
                let ov = *self.move_output.get(&(c, t)).unwrap_or(&0);
                let mprime = *self.e_meta.get(&t).unwrap_or(&m);
                let cond = self.input_cond(iv);
                writeln!(s, "        if ({cond}) begin").unwrap();
                for (k, out) in self.outputs.iter().enumerate() {
                    let bit = (ov >> k) & 1;
                    writeln!(s, "          {out}_c = 1'b{bit};").unwrap();
                }
                writeln!(s, "          mon_next = {n_mon}'d{mprime};").unwrap();
                writeln!(s, "        end").unwrap();
            }
            writeln!(s, "      end").unwrap();
        }
        writeln!(s, "      default: ;").unwrap();
        writeln!(s, "    endcase").unwrap();
        writeln!(s, "  end").unwrap();
        for out in &self.outputs {
            writeln!(s, "  assign {out} = {out}_c;").unwrap();
        }
        writeln!(s, "  always_ff @(posedge clk) mon <= mon_next;").unwrap();
        writeln!(s, "endmodule").unwrap();
        let _ = (n_in, n_out);
        s
    }

    /// Emit a **multi-guarantee** controller: adds a `mode` register (the
    /// guarantee index the controller is round-robining through) alongside the
    /// monitor bits. The combinational logic keys on `{mode, mon}` and, per
    /// input, sets the output, next monitor state, and next mode from the
    /// memoryful strategy.
    pub fn emit_mealy_sv_multi(
        &self,
        module: &str,
        z: &StateSet,
        strat: &crate::mu_calculus::gr1::Gr1MultiStrategy,
    ) -> String {
        use std::fmt::Write as _;
        let n_mon = self.n_monitors.max(1);
        let m = strat.n_modes.max(1);
        let n_mode = ((m as f64).log2().ceil() as usize).max(1);
        // invert ctrl_meta: (monitor, input) -> ctrl state index
        let mut c_of: HashMap<(usize, u32), usize> = HashMap::new();
        for (&c, &(mv, iv)) in &self.ctrl_meta {
            c_of.insert((mv, iv), c);
        }
        let n_mon_states = 1usize << self.n_monitors;
        let n_in_vals = 1u32 << self.inputs.len();
        let sel_w = n_mode + n_mon;
        let mut s = String::new();
        writeln!(
            s,
            "// Generated by Mununu GR(1) synthesis — sound-by-construction (multi-guarantee)"
        )
        .unwrap();
        write!(s, "module {module} (input logic clk").unwrap();
        for inp in &self.inputs {
            write!(s, ", input logic {inp}").unwrap();
        }
        for out in &self.outputs {
            write!(s, ", output logic {out}").unwrap();
        }
        writeln!(s, ");").unwrap();
        writeln!(s, "  logic [{}:0] mon = {n_mon}'d0;", n_mon - 1).unwrap();
        writeln!(s, "  logic [{}:0] mon_next;", n_mon - 1).unwrap();
        writeln!(s, "  logic [{}:0] mode = {n_mode}'d0;", n_mode - 1).unwrap();
        writeln!(s, "  logic [{}:0] mode_next;", n_mode - 1).unwrap();
        for out in &self.outputs {
            writeln!(s, "  logic {out}_c;").unwrap();
        }
        writeln!(s, "  always_comb begin").unwrap();
        for out in &self.outputs {
            writeln!(s, "    {out}_c = 1'b0;").unwrap();
        }
        writeln!(s, "    mon_next = mon;").unwrap();
        writeln!(s, "    mode_next = mode;").unwrap();
        writeln!(s, "    unique case ({{mode, mon}})").unwrap();
        for j in 0..m {
            for mv in 0..n_mon_states {
                // collect the per-input moves for (mode j, monitor mv)
                let mut branches: Vec<(u32, u32, usize, usize)> = Vec::new(); // (iv, ov, mon', mode')
                for iv in 0..n_in_vals {
                    let Some(&c) = c_of.get(&(mv, iv)) else {
                        continue;
                    };
                    if !z[c] {
                        continue;
                    }
                    let Some(&(t, next_j)) = strat.moves.get(&(c, j)) else {
                        continue;
                    };
                    let ov = *self.move_output.get(&(c, t)).unwrap_or(&0);
                    let mprime = *self.e_meta.get(&t).unwrap_or(&mv);
                    branches.push((iv, ov, mprime, next_j));
                }
                if branches.is_empty() {
                    continue;
                }
                let sel = (j << n_mon) | mv;
                writeln!(s, "      {sel_w}'d{sel}: begin").unwrap();
                for (iv, ov, mprime, next_j) in branches {
                    let cond = self.input_cond(iv);
                    writeln!(s, "        if ({cond}) begin").unwrap();
                    for (k, out) in self.outputs.iter().enumerate() {
                        let bit = (ov >> k) & 1;
                        writeln!(s, "          {out}_c = 1'b{bit};").unwrap();
                    }
                    writeln!(s, "          mon_next = {n_mon}'d{mprime};").unwrap();
                    writeln!(s, "          mode_next = {n_mode}'d{next_j};").unwrap();
                    writeln!(s, "        end").unwrap();
                }
                writeln!(s, "      end").unwrap();
            }
        }
        writeln!(s, "      default: ;").unwrap();
        writeln!(s, "    endcase").unwrap();
        writeln!(s, "  end").unwrap();
        for out in &self.outputs {
            writeln!(s, "  assign {out} = {out}_c;").unwrap();
        }
        writeln!(s, "  always_ff @(posedge clk) begin").unwrap();
        writeln!(s, "    mon <= mon_next;").unwrap();
        writeln!(s, "    mode <= mode_next;").unwrap();
        writeln!(s, "  end").unwrap();
        writeln!(s, "endmodule").unwrap();
        s
    }

    /// SV boolean condition selecting input valuation `iv`.
    fn input_cond(&self, iv: u32) -> String {
        if self.inputs.is_empty() {
            return "1'b1".to_string();
        }
        self.inputs
            .iter()
            .enumerate()
            .map(|(k, name)| {
                if (iv >> k) & 1 == 1 {
                    name.clone()
                } else {
                    format!("!{name}")
                }
            })
            .collect::<Vec<_>>()
            .join(" && ")
    }
}

/// Result of GR(1) controller synthesis from an LTL spec.
pub struct Gr1Synthesis {
    /// Whether the spec is realizable (init in the GR(1) winning region).
    pub realizable: bool,
    /// The synthesized controller as a SystemVerilog Mealy module — present iff
    /// realizable and a strategy could be extracted.
    pub controller_sv: Option<String>,
    /// Number of monitor bits in the game state.
    pub n_monitor_bits: usize,
    /// Number of game states (env + ctrl + BAD).
    pub n_game_states: usize,
    /// Human-readable notes (e.g. unsupported multi-guarantee memory).
    pub notes: Vec<String>,
}

/// Synthesize a sound GR(1) controller from an LTL spec: classify → build the
/// monitor-augmented game → solve the Piterman fixpoint → extract a rank-descent
/// strategy → emit SystemVerilog. Returns the realizability verdict and (when
/// realizable) the controller RTL.
///
/// Handles 0 / 1 guarantees (single positional strategy) and **≥2 guarantees**
/// (generalized Büchi) via a memoryful round-robin strategy with an added `mode`
/// register — so real multi-client arbiters (mutex + per-client no-starvation)
/// are synthesizable.
pub fn synthesise_gr1(
    assumptions: &[LtlFormula],
    guarantees: &[LtlFormula],
    inputs: &[String],
    outputs: &[String],
    module: &str,
) -> Result<Gr1Synthesis, String> {
    use super::gr1::{StateSet, gr1_strategy_single, gr1_win};

    let spec = Gr1Spec::classify(assumptions, guarantees, inputs, outputs)?;
    let game = Gr1Game::build(&spec);
    let z = gr1_win(&game.clts, &game.safe, &game.sys_fair, &game.env_fair);
    let n_game_states = game.clts.state_count();
    let n_monitor_bits = game.n_monitors;
    let mut notes = Vec::new();

    if !z[game.init] {
        return Ok(Gr1Synthesis {
            realizable: false,
            controller_sv: None,
            n_monitor_bits,
            n_game_states,
            notes,
        });
    }

    let controller_sv = match game.sys_fair.len() {
        0 => {
            // Safety-only: every winning state is a goal — any safe move works.
            let goal: StateSet = z.clone();
            let strat = gr1_strategy_single(&game.clts, &z, &goal);
            Some(game.emit_mealy_sv(module, &z, &strat))
        }
        1 => {
            let strat = gr1_strategy_single(&game.clts, &z, &game.sys_fair[0]);
            Some(game.emit_mealy_sv(module, &z, &strat))
        }
        k => {
            // Multiple guarantees (generalized Büchi): memoryful round-robin
            // strategy over a `mode` register.
            notes.push(format!(
                "{k} system guarantees: multi-guarantee memoryful strategy"
            ));
            let strat = super::gr1::gr1_strategy_multi(
                &game.clts,
                &game.safe,
                &z,
                &game.sys_fair,
                &game.env_fair,
            );
            Some(game.emit_mealy_sv_multi(module, &z, &strat))
        }
    };

    Ok(Gr1Synthesis {
        realizable: true,
        controller_sv,
        n_monitor_bits,
        n_game_states,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mu_calculus::gr1::{gr1_strategy_single, gr1_win};

    fn pred(s: &str) -> LtlFormula {
        LtlFormula::Predicate(s.to_string())
    }

    /// request_grant: assume GF req; guarantee G(req -> F grant) and G(grant -> X !grant).
    fn request_grant_spec() -> Gr1Spec {
        let assumptions = vec![LtlFormula::Recurrence(Box::new(pred("req")))];
        let guarantees = vec![
            LtlFormula::Response {
                trigger: Box::new(pred("req")),
                response: Box::new(pred("grant")),
            },
            LtlFormula::Always(Box::new(LtlFormula::Implies(
                Box::new(pred("grant")),
                Box::new(LtlFormula::Next(Box::new(LtlFormula::Not(Box::new(pred(
                    "grant",
                )))))),
            ))),
        ];
        Gr1Spec::classify(
            &assumptions,
            &guarantees,
            &["req".to_string()],
            &["grant".to_string()],
        )
        .expect("request_grant is in the GR(1) fragment")
    }

    #[test]
    fn classify_request_grant_shapes() {
        let spec = request_grant_spec();
        assert_eq!(spec.responses.len(), 1, "one response (req -> F grant)");
        assert_eq!(
            spec.safety
                .iter()
                .filter(|c| matches!(c, SafetyClause::Transition { .. }))
                .count(),
            1,
            "one transition safety (grant -> X !grant)"
        );
        assert_eq!(spec.env_fair.len(), 1, "one input fairness (GF req)");
    }

    #[test]
    fn built_request_grant_is_realizable_and_strategy_serves_safely() {
        let spec = request_grant_spec();
        let game = Gr1Game::build(&spec);
        let z = gr1_win(&game.clts, &game.safe, &game.sys_fair, &game.env_fair);
        assert!(z[game.init], "request_grant must be realizable from init");

        // Decode the strategy into grant(pending, was_grant, req) and check it is
        // SAFE (never grants when was_grant) and SERVING (grants pending+safe).
        // Monitor bit layout: bit0 = was_grant (transition safety), bit1 = pending.
        let strat = gr1_strategy_single(&game.clts, &z, &game.sys_fair[0]);
        let grant_at = |pending: usize, was_grant: usize, req: u32| -> Option<bool> {
            let m = (pending << 1) | was_grant;
            // find the C state for (m, iv=req)
            let c = game
                .ctrl_meta
                .iter()
                .find(|(_, meta)| meta.0 == m && meta.1 == req)
                .map(|(&c, _)| c)?;
            if !z[c] {
                return None;
            }
            let t = *strat.get(&c)?;
            game.move_output.get(&(c, t)).map(|&ov| ov & 1 == 1)
        };
        for pending in 0..2 {
            for req in 0..2 {
                if let Some(g) = grant_at(pending, 1, req) {
                    assert!(
                        !g,
                        "must not grant when was_grant (pending={pending}, req={req})"
                    );
                }
            }
        }
        assert_eq!(
            grant_at(0, 0, 1),
            Some(true),
            "grant a fresh request when safe"
        );
        assert_eq!(
            grant_at(1, 0, 0),
            Some(true),
            "grant a pending request when safe"
        );
    }

    #[test]
    fn emit_request_grant_mealy_sv_structure() {
        let spec = request_grant_spec();
        let game = Gr1Game::build(&spec);
        let z = gr1_win(&game.clts, &game.safe, &game.sys_fair, &game.env_fair);
        let strat = gr1_strategy_single(&game.clts, &z, &game.sys_fair[0]);
        let sv = game.emit_mealy_sv("gr1_ctrl", &z, &strat);
        assert!(
            sv.contains("module gr1_ctrl (input logic clk, input logic req, output logic grant)")
        );
        assert!(
            sv.contains("logic [1:0] mon"),
            "2 monitor bits (was_grant, pending)"
        );
        assert!(
            sv.contains("grant_c = 1'b1"),
            "grants in at least one branch"
        );
        assert!(sv.contains("assign grant = grant_c;"));
        assert!(sv.contains("always_ff @(posedge clk) mon <= mon_next;"));
        // Optional oracle emit.
        if let Ok(p) = std::env::var("GR1_EMIT_SV2") {
            std::fs::write(&p, sv).unwrap();
            eprintln!("wrote emitted GR(1) controller to {p}");
        }
    }

    #[test]
    fn synthesise_gr1_request_grant_realizable_with_controller() {
        let assumptions = vec![LtlFormula::Recurrence(Box::new(pred("req")))];
        let guarantees = vec![
            LtlFormula::Response {
                trigger: Box::new(pred("req")),
                response: Box::new(pred("grant")),
            },
            LtlFormula::Always(Box::new(LtlFormula::Implies(
                Box::new(pred("grant")),
                Box::new(LtlFormula::Next(Box::new(LtlFormula::Not(Box::new(pred(
                    "grant",
                )))))),
            ))),
        ];
        let r = synthesise_gr1(
            &assumptions,
            &guarantees,
            &["req".into()],
            &["grant".into()],
            "gr1_ctrl",
        )
        .expect("request_grant classifies");
        assert!(r.realizable, "request_grant is realizable");
        let sv = r.controller_sv.expect("a controller was emitted");
        assert!(sv.contains("module gr1_ctrl"));
        assert!(sv.contains("grant_c = 1'b1"), "grants somewhere");
        assert_eq!(r.n_monitor_bits, 2, "was_grant + pending");
    }

    #[test]
    fn synthesise_gr1_unrealizable_reports_no_controller() {
        // grant -> X !grant AND grant -> X grant is contradictory when req forces grant.
        let assumptions = vec![LtlFormula::Recurrence(Box::new(pred("req")))];
        let guarantees = vec![
            LtlFormula::Response {
                trigger: Box::new(pred("req")),
                response: Box::new(pred("grant")),
            },
            LtlFormula::Always(Box::new(LtlFormula::Not(Box::new(pred("grant"))))), // G !grant
        ];
        let r = synthesise_gr1(
            &assumptions,
            &guarantees,
            &["req".into()],
            &["grant".into()],
            "c",
        )
        .expect("classifies");
        assert!(
            !r.realizable,
            "serve-every-request while never-grant is UNrealizable"
        );
        assert!(r.controller_sv.is_none());
    }

    #[test]
    fn synthesise_gr1_two_client_arbiter_multi_guarantee() {
        // 2-client bus arbiter: mutex (invariant safety) + no-starvation per
        // client (two response guarantees ⇒ multi-guarantee memoryful strategy).
        let assumptions = vec![
            LtlFormula::Recurrence(Box::new(pred("req_0"))),
            LtlFormula::Recurrence(Box::new(pred("req_1"))),
        ];
        let guarantees = vec![
            // mutex: G !(grant_0 && grant_1)
            LtlFormula::Always(Box::new(LtlFormula::Not(Box::new(LtlFormula::And(
                Box::new(pred("grant_0")),
                Box::new(pred("grant_1")),
            ))))),
            // no-starvation per client
            LtlFormula::Response {
                trigger: Box::new(pred("req_0")),
                response: Box::new(pred("grant_0")),
            },
            LtlFormula::Response {
                trigger: Box::new(pred("req_1")),
                response: Box::new(pred("grant_1")),
            },
        ];
        let r = synthesise_gr1(
            &assumptions,
            &guarantees,
            &["req_0".into(), "req_1".into()],
            &["grant_0".into(), "grant_1".into()],
            "arbiter",
        )
        .expect("arbiter classifies");
        assert!(r.realizable, "2-client arbiter is realizable");
        let sv = r.controller_sv.expect("arbiter controller emitted");
        assert!(sv.contains("module arbiter"));
        assert!(
            sv.contains("logic [0:0] mode"),
            "multi-guarantee mode register"
        );
        assert!(
            sv.contains("grant_0") && sv.contains("grant_1"),
            "both grants driven"
        );
        // oracle-validated separately (scratchpad/demo/arbiter2*): mutex
        // k-induction-proved; no-starvation bounded ≤5 for both clients.
    }

    #[test]
    fn out_of_fragment_clause_is_rejected() {
        // An unbounded Until is outside the supported GR(1) fragment.
        let g = vec![LtlFormula::Until {
            left: Box::new(pred("a")),
            right: Box::new(pred("b")),
        }];
        let r = Gr1Spec::classify(&[], &g, &["a".into()], &["b".into()]);
        assert!(r.is_err(), "Until must be rejected as out-of-fragment");
    }

    /// mununu#1121 — a clause outside the GR(1) fragment is refused quoting the clause in
    /// LTL syntax, never as the Rust `Debug` tree (`Until { left: Predicate("p"), … }`).
    #[test]
    fn x1121_an_unsupported_gr1_clause_is_quoted_in_ltl_syntax() {
        let err = Gr1Spec::classify(
            &[],
            &[LtlFormula::Until {
                left: Box::new(pred("p")),
                right: Box::new(pred("q")),
            }],
            &["p".to_string()],
            &["q".to_string()],
        )
        .err()
        .expect("an until clause is outside the fragment");
        assert!(
            err.starts_with("clause `p U q` is outside the GR(1) fragment")
                && err.contains("a limit of the technique"),
            "{err}"
        );
    }
}
