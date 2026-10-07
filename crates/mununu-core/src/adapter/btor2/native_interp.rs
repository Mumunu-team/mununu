//! Owned interpolation-based safety engine (McMillan, TACAS 2003) — Phase 1 of
//! the IC3ia-quality invariant-discovery track (`.claude/plans/cube-ic3ia-invariant-discovery.md`).
//!
//! # Why this exists
//!
//! The reachability portfolio decides `AG ¬bad` with native BMC + native
//! k-induction ([`super::native_bmc`]) plus the external SPACER / btormc / Pono
//! members. On real HWMCC bv cases (e.g. `vis_arrays_am2910_p2`) *only* SPACER's
//! IC3/PDR decides safe — native k-induction, BMC, btormc, Pono **and** the KMTS
//! predicate cube all abstain. The gap is a single missing capability: an
//! inductive invariant that is **not** k-inductive and **cannot** be enumerated
//! as a syntactic predicate. IC3/PDR *synthesises* it; interpolation is the
//! word-level route to the same predicates.
//!
//! This module is the owned analogue: forward reachability by Craig
//! interpolation. It reuses the exact BTOR2→SMT encoding
//! ([`crate::adapter::sidecar::predicate_image::btor2_encode::encode_design`])
//! and cvc5's `(get-interpolant …)` front end
//! ([`crate::adapter::cvc5`]). No new SMT theory — the interpolant is computed
//! over the *exact* transition relation (bit-precise `bvadd`/`bvmul`/…), so any
//! definite verdict it returns is sound for the concrete design.
//!
//! # Status
//!
//! Phase 1 complete: the serialize → cvc5 → parse-back machinery, the one-step
//! interpolant, AND the full forward-reachability **k-schedule** convergence loop
//! ([`verify_safety_interp`]). Gate met — the engine decides the real HWMCC case
//! `vis_arrays_am2910_p2` **Safe** (`iterations: 4`, ~4s) where native
//! k-induction / BMC / btormc / Pono and the KMTS cube all abstain and only
//! external SPACER decided. Next: broaden the HWMCC differential-soundness sweep,
//! then wire into the reachability portfolio ([`super::super::reach_portfolio`]).

use std::collections::BTreeMap;

use z3::ast::{Ast, BV, Bool};

use crate::adapter::btor2::ast::{Btor2File, Nid, Node};
use crate::adapter::sidecar::predicate_image::btor2_encode::{Btor2SmtView, encode_design};

/// Controlled-name interface variables for a one-step interpolation query.
///
/// z3 interns named constants by `(name, sort)`, so building the query's shared
/// vocabulary with [`BV::new_const`] (never `fresh_const`) means the cvc5
/// interpolant — which comes back over those same names — round-trips to the
/// **same** z3 AST nodes when re-parsed via [`z3::Solver::from_string`]. That is
/// what makes the `next → curr` remap (and the eventual fixpoint check) a plain
/// [`Ast::substitute`] rather than a name-matching dance.
struct Interface {
    /// Current-step value of each state cell (`mc_c<nid>`).
    cur: BTreeMap<Nid, BV>,
    /// Next-step value of each state cell (`mc_n<nid>`) — the interpolation
    /// interface (shared vocabulary between `A` and `B`).
    nx: BTreeMap<Nid, BV>,
    /// Step-0 input value of each input (`mc_i<nid>`).
    inp: BTreeMap<Nid, BV>,
    /// Step-1 input value of each input (`mc_j<nid>`) — used only if `bad`
    /// reads an input, so those inputs stay *out* of the shared vocabulary.
    inp1: BTreeMap<Nid, BV>,
}

impl Interface {
    fn build(view: &Btor2SmtView) -> Self {
        let mk = |src: &std::collections::HashMap<Nid, BV>, tag: char| -> BTreeMap<Nid, BV> {
            src.iter()
                .map(|(nid, bv)| (*nid, BV::new_const(format!("mc_{tag}{nid}"), bv.get_size())))
                .collect()
        };
        Interface {
            cur: mk(&view.state_curr, 'c'),
            nx: mk(&view.state_next, 'n'),
            inp: mk(&view.inputs, 'i'),
            inp1: mk(&view.inputs, 'j'),
        }
    }

    /// `state_curr → cur`, `state_next → nx`, `inputs → inp` — instantiates the
    /// design's transition relation over the controlled interface names.
    fn transition_subs<'a>(&'a self, view: &'a Btor2SmtView) -> Vec<(&'a BV, &'a BV)> {
        let mut pairs = Vec::new();
        for (nid, bv) in &view.state_curr {
            if let Some(c) = self.cur.get(nid) {
                pairs.push((bv, c));
            }
        }
        for (nid, bv) in &view.state_next {
            if let Some(n) = self.nx.get(nid) {
                pairs.push((bv, n));
            }
        }
        for (nid, bv) in &view.inputs {
            if let Some(i) = self.inp.get(nid) {
                pairs.push((bv, i));
            }
        }
        pairs
    }

    /// `state_curr → nx`, `inputs → inp1` — evaluates a current-cycle combinational
    /// term (e.g. `bad`) at the **next** state `s1`.
    fn at_next_subs<'a>(&'a self, view: &'a Btor2SmtView) -> Vec<(&'a BV, &'a BV)> {
        let mut pairs = Vec::new();
        for (nid, bv) in &view.state_curr {
            if let Some(n) = self.nx.get(nid) {
                pairs.push((bv, n));
            }
        }
        for (nid, bv) in &view.inputs {
            if let Some(j) = self.inp1.get(nid) {
                pairs.push((bv, j));
            }
        }
        pairs
    }
}

/// The design's `bad` / `init` / `constraint` operands (BTOR2 NIDs).
struct Props {
    bad: Vec<Nid>,
    init: Vec<(Nid, Nid)>,
    constraint: Vec<Nid>,
}

fn extract_props(file: &Btor2File) -> Props {
    let mut props = Props {
        bad: Vec::new(),
        init: Vec::new(),
        constraint: Vec::new(),
    };
    for l in &file.lines {
        match &l.node {
            Node::Bad { signal } => props.bad.push(signal.nid()),
            Node::Init { state, value, .. } => props.init.push((*state, value.nid())),
            Node::Constraint { signal } => props.constraint.push(signal.nid()),
            _ => {}
        }
    }
    props
}

/// A design signal's current-cycle BV, falling back through `state_curr`/`inputs`
/// so a property/init that references a state cell or input directly is not lost.
fn curr_bv<'a>(view: &'a Btor2SmtView, nid: &Nid) -> Option<&'a BV> {
    view.signal_bvs
        .get(nid)
        .or_else(|| view.state_curr.get(nid))
        .or_else(|| view.inputs.get(nid))
}

/// `bad` as a z3 `Bool` over a chosen substitution: OR over all `bad` operands of
/// `(operand == 1)`. `subs` maps the design's curr/input BVs to the interface
/// frame (either `cur` or `nx`).
fn bad_bool(view: &Btor2SmtView, props: &Props, subs: &[(&BV, &BV)]) -> Bool {
    let one1 = BV::from_u64(1, 1);
    let disj: Vec<Bool> = props
        .bad
        .iter()
        .filter_map(|op| curr_bv(view, op).map(|bv| bv.substitute(subs).eq(&one1)))
        .collect();
    if disj.is_empty() {
        Bool::from_bool(false)
    } else {
        Bool::or(&disj.iter().collect::<Vec<_>>())
    }
}

/// Each `constraint` as `(operand == 1)`, over a chosen substitution.
fn constraint_bools(view: &Btor2SmtView, props: &Props, subs: &[(&BV, &BV)]) -> Vec<Bool> {
    let one1 = BV::from_u64(1, 1);
    props
        .constraint
        .iter()
        .filter_map(|op| curr_bv(view, op).map(|bv| bv.substitute(subs).eq(&one1)))
        .collect()
}

/// `Init(cur)` as a z3 `Bool`: `⋀ (cur[state] == init_value)`. Init-less cells stay
/// free (BTOR2's nondeterministic-init semantics).
fn init_bool(view: &Btor2SmtView, props: &Props, iface: &Interface) -> Bool {
    let subs = {
        let mut pairs = Vec::new();
        for (nid, bv) in &view.state_curr {
            if let Some(c) = iface.cur.get(nid) {
                pairs.push((bv, c));
            }
        }
        pairs
    };
    let conj: Vec<Bool> = props
        .init
        .iter()
        .filter_map(|(state, value_nid)| {
            let cur = iface.cur.get(state)?;
            let vbv = curr_bv(view, value_nid)?.substitute(&subs);
            Some(cur.eq(&vbv))
        })
        .collect();
    if conj.is_empty() {
        Bool::from_bool(true)
    } else {
        Bool::and(&conj.iter().collect::<Vec<_>>())
    }
}

/// Serialize a z3 `Bool` to `(declare-fun …)` lines + the bare assert *body* term,
/// via [`z3::Solver::to_smt2`]. Because the interface vars were built with
/// [`BV::new_const`], the rendered names are the controlled `mc_*` names — stable
/// across `A` and `B`, so their shared vocabulary lines up in the cvc5 query.
pub(crate) fn serialize_term(b: &Bool) -> Option<(Vec<String>, String)> {
    let solver = z3::Solver::new();
    solver.assert(b);
    let smt2 = solver.to_smt2();
    let mut declares = Vec::new();
    for line in smt2.lines() {
        let t = line.trim_start();
        if t.starts_with("(declare-fun ") || t.starts_with("(declare-const ") {
            declares.push(t.to_string());
        }
    }
    let body = extract_assert_body(&smt2)?;
    Some((declares, body))
}

/// Extract the balanced s-expression that is the argument of the (last) `(assert …)`
/// in a `to_smt2` dump. Returns the body term without the `(assert` wrapper.
fn extract_assert_body(smt2: &str) -> Option<String> {
    let start = smt2.rfind("(assert")?;
    let after = &smt2[start + "(assert".len()..];
    // Scan to the matching close paren of the assert (depth starts at 1 for the
    // already-consumed `(assert`).
    let mut depth = 1i32;
    let mut in_pipe = false; // z3 quotes odd identifiers as |...|
    let mut end = None;
    for (i, ch) in after.char_indices() {
        match ch {
            '|' => in_pipe = !in_pipe,
            '(' if !in_pipe => depth += 1,
            ')' if !in_pipe => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = after[..end?].trim();
    if body.is_empty() {
        None
    } else {
        Some(body.to_string())
    }
}

/// Result of a one-step interpolation attempt. Deliberately z3-free so it can
/// escape the [`z3::with_z3_config`] closure (z3 `Bool` is `!Send`).
pub enum OneStepInterp {
    /// cvc5 returned an interpolant; `raw` is its s-expression term (over the
    /// `mc_n<nid>` next-state names). `parsed_ok` records whether that term
    /// round-tripped back to a z3 `Bool` over the interface's `nx` constants via
    /// [`z3::Solver::from_string`] — the check the convergence loop depends on
    /// for the `next → curr` remap.
    Interpolant { raw: String, parsed_ok: bool },
    /// `A ∧ B` is satisfiable — `bad` is reachable in one step from `Init` (a real
    /// 1-step counterexample, since `A` here starts at `Init`).
    Reachable,
    /// cvc5 is absent, timed out, or produced no usable reply.
    Unavailable(String),
}

/// Compute the one-step Craig interpolant of `A = Init(cur) ∧ T(cur→nx)` against
/// `B = bad(nx)`, over the exact transition relation.
///
/// The returned interpolant `I(nx)` satisfies `A ⟹ I` and `I ⟹ ¬bad`: it
/// over-approximates the one-step image of `Init` while excluding `bad`. This is
/// the atom the convergence loop will remap (`nx → cur`) and union into the
/// growing reachable set `R`.
pub fn one_step_interpolant(file: &Btor2File) -> OneStepInterp {
    let props = extract_props(file);
    if props.bad.is_empty() {
        return OneStepInterp::Unavailable("design has no `bad` property".into());
    }
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = match encode_design(file) {
            Ok(v) => v,
            Err(e) => return OneStepInterp::Unavailable(format!("encode failed: {e:?}")),
        };
        let iface = Interface::build(&view);

        // A = Init(cur) ∧ T(cur→nx) ∧ constraints(cur) ∧ constraints(nx-as-curr…)
        let init = init_bool(&view, &props, &iface);
        let t_subs = iface.transition_subs(&view);
        let transition = view.transition.substitute(&t_subs);
        // constraints hold at the current step (over cur).
        let cur_subs: Vec<(&BV, &BV)> = view
            .state_curr
            .iter()
            .filter_map(|(nid, bv)| iface.cur.get(nid).map(|c| (bv, c)))
            .chain(
                view.inputs
                    .iter()
                    .filter_map(|(nid, bv)| iface.inp.get(nid).map(|i| (bv, i))),
            )
            .collect();
        // cvc5's `(get-interpolant I B)` convention: with assertions `A`, returns
        // `I` s.t. `A ⟹ I` and `I ⟹ B`, over the shared vocabulary — PROVIDED
        // `A ⟹ B`. So the *safe* region `¬bad(nx)` is `B`, and the constraints
        // (legal-state assumptions at both ends of the step) live in `A`.
        let next_subs = iface.at_next_subs(&view);
        let mut a_terms = vec![init, transition];
        a_terms.extend(constraint_bools(&view, &props, &cur_subs));
        a_terms.extend(constraint_bools(&view, &props, &next_subs));
        let a_refs: Vec<&Bool> = a_terms.iter().collect();
        let a = Bool::and(&a_refs);

        // B = ¬bad(nx). If `A ⊭ B` (bad reachable in one step from Init) cvc5
        // finds no interpolant → classified `Reachable`.
        let b = bad_bool(&view, &props, &next_subs).not();

        match interpolate_bool(&a, &b, &iface.nx, 30_000) {
            InterpStep::Interpolant(bv) => {
                let raw = format!("{bv}");
                OneStepInterp::Interpolant {
                    raw,
                    parsed_ok: true,
                }
            }
            InterpStep::NoInterpolant => OneStepInterp::Reachable,
            InterpStep::Unavailable(e) => OneStepInterp::Unavailable(e),
        }
    })
}

/// Verdict of the interpolation-based safety engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterpSafetyVerdict {
    /// Proved `AG ¬bad`: the reachable over-approximation `R` reached a fixpoint
    /// (`post(R) ⊆ R`) while every disjunct implies `¬bad`. `iterations` counts
    /// the interpolants unioned into the inductive invariant.
    Safe { iterations: u32 },
    /// `bad` is reachable — `depth` is the step at which it was hit (0 = holds at
    /// `Init`, 1 = one-step reachable from `Init`).
    Unsafe { depth: u32 },
    /// The engine abstained (never a wrong verdict): the one-step forward
    /// interpolation did not converge within the iteration cap, a spurious
    /// one-step-to-bad appeared (the suffix depth `k=1` is too shallow — a larger
    /// `k` schedule is the next increment), cvc5 was unavailable, or a solver
    /// query timed out.
    Undecided { reason: String },
}

/// Multi-frame unrolling for the k-step suffix: fresh controlled-name state/input
/// vars for frames `0..=k`, and the design's `transition`/`bad`/`constraint`/`init`
/// instantiated at any frame. Frame 1's state cells are the interpolation
/// interface (shared vocabulary) — the interpolant comes back over them and is
/// remapped `frame1 → frame0`.
struct Frames<'a> {
    view: &'a Btor2SmtView,
    props: &'a Props,
    /// `state[frame][nid]` — state cell `nid` at frame `frame`.
    state: Vec<BTreeMap<Nid, BV>>,
    /// `input[frame][nid]` — input `nid` at frame `frame`.
    input: Vec<BTreeMap<Nid, BV>>,
}

impl<'a> Frames<'a> {
    fn build(view: &'a Btor2SmtView, props: &'a Props, k: usize) -> Self {
        let mk =
            |p: char, j: usize, src: &std::collections::HashMap<Nid, BV>| -> BTreeMap<Nid, BV> {
                src.iter()
                    .map(|(nid, bv)| {
                        (
                            *nid,
                            BV::new_const(format!("mc_{p}{j}_{nid}"), bv.get_size()),
                        )
                    })
                    .collect()
            };
        Frames {
            view,
            props,
            state: (0..=k).map(|j| mk('s', j, &view.state_curr)).collect(),
            input: (0..=k).map(|j| mk('i', j, &view.inputs)).collect(),
        }
    }

    /// `state_curr → state[j]`, `inputs → input[j]` — a current-cycle term at frame `j`.
    fn subs_curr(&self, j: usize) -> Vec<(&BV, &BV)> {
        let mut pairs = Vec::new();
        for (nid, bv) in &self.view.state_curr {
            if let Some(s) = self.state[j].get(nid) {
                pairs.push((bv, s));
            }
        }
        for (nid, bv) in &self.view.inputs {
            if let Some(i) = self.input[j].get(nid) {
                pairs.push((bv, i));
            }
        }
        pairs
    }

    /// The transition relation `T(frame j → frame j+1)`.
    fn transition_at(&self, j: usize) -> Bool {
        let mut pairs = self.subs_curr(j);
        for (nid, bv) in &self.view.state_next {
            if let Some(s) = self.state[j + 1].get(nid) {
                pairs.push((bv, s));
            }
        }
        self.view.transition.substitute(&pairs)
    }

    fn bad_at(&self, j: usize) -> Bool {
        bad_bool(self.view, self.props, &self.subs_curr(j))
    }

    fn constr_at(&self, j: usize) -> Vec<Bool> {
        constraint_bools(self.view, self.props, &self.subs_curr(j))
    }

    /// `Init` over frame 0.
    fn init_at0(&self) -> Bool {
        let subs = self.subs_curr(0);
        let conj: Vec<Bool> = self
            .props
            .init
            .iter()
            .filter_map(|(state, value_nid)| {
                let s0 = self.state[0].get(state)?;
                let vbv = curr_bv(self.view, value_nid)?.substitute(&subs);
                Some(s0.eq(&vbv))
            })
            .collect();
        if conj.is_empty() {
            Bool::from_bool(true)
        } else {
            Bool::and(&conj.iter().collect::<Vec<_>>())
        }
    }
}

/// Owned interpolation-based safety engine (McMillan, TACAS 2003 — forward
/// reachability with an interpolation **k-schedule**). Proves or refutes
/// `AG ¬bad` by growing an over-approximation `R` of the reachable states from
/// Craig interpolants until it is inductive, deepening the suffix depth `k` when
/// a spurious counterexample shows the current suffix is too shallow.
///
/// **The loop.** For suffix depth `k = 1, 2, …`: restart `R = Init`; repeatedly
/// interpolate `A = R(s0) ∧ T(s0,s1)` against `B = ¬(bad reachable within the
/// k-step suffix from s1)`. cvc5's `A ⟹ I ⟹ B` gives `I(s1)` over-approximating
/// `post(R)` while excluding states that reach `bad` within `k`; remap `s1 → s0`
/// and union into `R`. When `I ⊆ R` the fixpoint is an inductive invariant
/// (`SAFE`); when no interpolant exists and `R = Init` a concrete
/// counterexample is confirmed (`UNSAFE`); when no interpolant exists but `R`
/// has grown the CTI is spurious → increase `k`.
///
/// **Soundness.** Every `Safe`/`Unsafe` is over the *exact* transition relation:
/// `Safe` only when `R ⊇ Init`, `post(R) ⊆ R`, and `R ⟹ ¬bad` (each `Iⱼ ⟹ ¬bad`
/// by cvc5's contract; `Init ⟹ ¬bad` checked) — sound at *any* `k`, since the
/// suffix depth affects only precision, not the inductive-invariant check.
/// `Unsafe` only when z3 confirms a concrete violation. Everything else is
/// `Undecided` — the engine never guesses.
///
/// `max_suffix` bounds the outer `k`; `max_iters` the inner refinement; `timeout_ms`
/// bounds each z3/cvc5 query; `overall_timeout_ms` is the whole-run wall-clock cap
/// (checked between queries — a design that would burn `max_suffix × max_iters`
/// queries abstains with `Undecided` instead, which is what makes the engine safe
/// to run as a bounded portfolio member).
pub fn verify_safety_interp(
    file: &Btor2File,
    max_suffix: u32,
    max_iters: u32,
    timeout_ms: u32,
    overall_timeout_ms: u64,
) -> InterpSafetyVerdict {
    verify_safety_interp_cancellable(
        file,
        max_suffix,
        max_iters,
        timeout_ms,
        overall_timeout_ms,
        &std::sync::atomic::AtomicBool::new(false),
    )
}

/// Like [`verify_safety_interp`] but abandons the search early — returning
/// `Undecided` — once `cancel` is set. The portfolio sets it when a *faster* member
/// produces a definite verdict, so this engine can run **concurrently** (not only as a
/// last resort) without its pathological interpolation query (cvc5 SyGuS can take tens
/// of seconds on a deep-grammar interpolant) dominating the wall-clock after another
/// engine has already decided. On the cases where this engine is the *unique* decider
/// (nothing else fires), `cancel` stays clear and the full budget is used.
pub(crate) fn verify_safety_interp_cancellable(
    file: &Btor2File,
    max_suffix: u32,
    max_iters: u32,
    timeout_ms: u32,
    overall_timeout_ms: u64,
    cancel: &std::sync::atomic::AtomicBool,
) -> InterpSafetyVerdict {
    let props = extract_props(file);
    if props.bad.is_empty() {
        return InterpSafetyVerdict::Undecided {
            reason: "design has no `bad` property".into(),
        };
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(overall_timeout_ms);
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = match encode_design(file) {
            Ok(v) => v,
            Err(e) => {
                return InterpSafetyVerdict::Undecided {
                    reason: format!("encode failed: {e:?}"),
                };
            }
        };

        let mk_solver = || {
            let s = z3::Solver::new();
            let mut p = z3::Params::new();
            p.set_u32("timeout", timeout_ms);
            s.set_params(&p);
            s
        };
        // `x ⟹ y` ⟺ `x ∧ ¬y` UNSAT (Unknown/timeout ⇒ "not proven" — the fixpoint
        // test is conservative, never a false `Safe`).
        let implies = |x: &Bool, y: &Bool| -> bool {
            let s = mk_solver();
            s.assert(x);
            s.assert(y.not());
            matches!(s.check(), z3::SatResult::Unsat)
        };
        let sat = |x: &Bool| -> z3::SatResult {
            let s = mk_solver();
            s.assert(x);
            s.check()
        };

        // `bad` already holds at `Init`? (length-0 violation — suffix-independent.)
        {
            let f = Frames::build(&view, &props, 1);
            let mut t = vec![f.init_at0(), f.bad_at(0)];
            t.extend(f.constr_at(0));
            if sat(&Bool::and(&t.iter().collect::<Vec<_>>())) == z3::SatResult::Sat {
                return InterpSafetyVerdict::Unsafe { depth: 0 };
            }
        }

        // Outer k-schedule: deepen the suffix until a fixpoint or a real CEX.
        for ksfx in 1..=max_suffix as usize {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return InterpSafetyVerdict::Undecided {
                    reason: "cancelled — another portfolio member decided first".into(),
                };
            }
            if std::time::Instant::now() >= deadline {
                return InterpSafetyVerdict::Undecided {
                    reason: format!(
                        "overall timeout ({overall_timeout_ms}ms) at suffix depth {ksfx}"
                    ),
                };
            }
            let f = Frames::build(&view, &props, ksfx);
            let f1_to_f0: Vec<(&BV, &BV)> = f.state[1]
                .iter()
                .filter_map(|(nid, s1)| f.state[0].get(nid).map(|s0| (s1, s0)))
                .collect();

            let init0 = f.init_at0();
            let constr0 = f.constr_at(0);
            let constr1 = f.constr_at(1);
            let t01 = f.transition_at(0);

            // reach = ⋁_{j=1}^{k} [ (⋀_{i=1}^{j-1} T(i,i+1)) ∧ (⋀_{i=2}^{j} constr(i)) ∧ bad(j) ]
            // — "bad is reachable within k constraint-respecting steps from frame 1".
            //   bad(1) is an UNCONDITIONAL disjunct: a state that is bad *now* is a
            //   violation regardless of whether it has a valid successor. Gating bad(1)
            //   behind the full suffix T-chain (as a single big conjunction would) is
            //   unsound on constrained designs — a bad dead-end whose only successors
            //   violate a constraint would escape `reach` and be admitted into R.
            //   `B = ¬reach` is the get-interpolant target; `I ⟹ ¬reach ⟹ ¬bad(1)`.
            let mut reach_disj: Vec<Bool> = Vec::new();
            for j in 1..=ksfx {
                let mut path: Vec<Bool> = Vec::new();
                for i in 1..j {
                    path.push(f.transition_at(i));
                }
                for i in 2..=j {
                    path.extend(f.constr_at(i));
                }
                path.push(f.bad_at(j));
                reach_disj.push(Bool::and(&path.iter().collect::<Vec<_>>()));
            }
            let reach = Bool::or(&reach_disj.iter().collect::<Vec<_>>());
            let safe_suffix = reach.not();

            // Inner forward-reachability fixpoint at this suffix depth. A spurious
            // CTI (`grown` && bad reachable) `break`s the inner loop; the outer
            // loop then deepens the suffix.
            let mut r = init0.clone();
            let mut grown = false;
            for iter in 0..max_iters {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return InterpSafetyVerdict::Undecided {
                        reason: "cancelled — another portfolio member decided first".into(),
                    };
                }
                if std::time::Instant::now() >= deadline {
                    return InterpSafetyVerdict::Undecided {
                        reason: format!("overall timeout ({overall_timeout_ms}ms)"),
                    };
                }
                let mut a_terms = vec![r.clone()];
                a_terms.extend(constr0.clone());
                a_terms.push(t01.clone());
                a_terms.extend(constr1.clone());
                let a = Bool::and(&a_terms.iter().collect::<Vec<_>>());

                match interpolate_bool_cancellable(
                    &a,
                    &safe_suffix,
                    &f.state[1],
                    timeout_ms,
                    Some(cancel),
                ) {
                    InterpStep::Interpolant(i_f1) => {
                        let i_f0 = i_f1.substitute(&f1_to_f0);
                        if implies(&i_f0, &r) {
                            return InterpSafetyVerdict::Safe {
                                iterations: iter + 1,
                            };
                        }
                        r = Bool::or(&[&r, &i_f0]);
                        grown = true;
                    }
                    InterpStep::NoInterpolant => {
                        // `A ⊭ ¬reach`: from R, bad reachable within k. Confirm concretely.
                        let mut ab = a_terms.clone();
                        ab.push(reach.clone());
                        let a_and_reach = Bool::and(&ab.iter().collect::<Vec<_>>());
                        match sat(&a_and_reach) {
                            z3::SatResult::Sat if !grown => {
                                // R is still Init → a genuine ≤k-step counterexample.
                                return InterpSafetyVerdict::Unsafe { depth: ksfx as u32 };
                            }
                            z3::SatResult::Sat => {
                                // Spurious at this depth → fall through to deepen the suffix.
                            }
                            z3::SatResult::Unsat => {
                                return InterpSafetyVerdict::Undecided {
                                    reason: "cvc5 found no interpolant but A∧reach is UNSAT (interpolation gap)".into(),
                                };
                            }
                            z3::SatResult::Unknown => {
                                return InterpSafetyVerdict::Undecided {
                                    reason: "solver timeout confirming reachability".into(),
                                };
                            }
                        }
                        break;
                    }
                    InterpStep::Unavailable(e) => {
                        return InterpSafetyVerdict::Undecided {
                            reason: format!("interpolation unavailable: {e}"),
                        };
                    }
                }
            }
            // Inner loop ended (spurious-CTI break, or max_iters without a
            // fixpoint) → the outer loop deepens the suffix; the outer bound caps
            // total work.
        }
        InterpSafetyVerdict::Undecided {
            reason: format!(
                "no fixpoint within suffix depth {max_suffix} × {max_iters} iterations"
            ),
        }
    })
}

/// Outcome of a single interpolation query, kept z3-typed for the convergence
/// loop (this stays *inside* the [`z3::with_z3_config`] scope).
enum InterpStep {
    /// cvc5 returned an interpolant `I` s.t. `A ⟹ I ⟹ B`, re-parsed to a z3
    /// `Bool` over the interface's `nx` constants.
    Interpolant(Bool),
    /// `A ⊭ B` — no interpolant exists (cvc5 `fail` / `(error …)` / empty).
    NoInterpolant,
    /// cvc5 absent, or a reply we could not parse back into z3.
    Unavailable(String),
}

/// Serialize `A`/`B`, invoke cvc5 `get-interpolant`, and re-parse the reply into a
/// z3 `Bool` over `shared` — the interpolation interface variables (the shared
/// vocabulary; frame 1's state cells).
fn interpolate_bool(a: &Bool, b: &Bool, shared: &BTreeMap<Nid, BV>, timeout_ms: u32) -> InterpStep {
    interpolate_bool_cancellable(a, b, shared, timeout_ms, None)
}

/// [`interpolate_bool`] whose cvc5 child honours the portfolio's cancel flag (S1): once a
/// peer member has decided, the query is killed after the member grace and reads as
/// `Unavailable` — a sound abstain, exactly like a timed-out query.
fn interpolate_bool_cancellable(
    a: &Bool,
    b: &Bool,
    shared: &BTreeMap<Nid, BV>,
    timeout_ms: u32,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> InterpStep {
    let (a_decls, a_body) = match serialize_term(a) {
        Some(x) => x,
        None => return InterpStep::Unavailable("failed to serialize A".into()),
    };
    let (b_decls, b_body) = match serialize_term(b) {
        Some(x) => x,
        None => return InterpStep::Unavailable("failed to serialize B".into()),
    };

    // Union the declares (BTreeSet for deterministic, dedup'd output).
    let mut decls: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    decls.extend(a_decls);
    decls.extend(b_decls);

    let mut query = String::new();
    query.push_str("(set-logic QF_BV)\n");
    query.push_str("(set-option :produce-interpolants true)\n");
    for d in &decls {
        query.push_str(d);
        query.push('\n');
    }
    query.push_str(&format!("(assert {a_body})\n"));
    query.push_str(&format!("(get-interpolant I {b_body})\n"));
    query.push_str("(exit)\n");

    let stdout = match run_cvc5_raw_cancellable(&query, timeout_ms, cancel) {
        Ok(s) => s,
        Err(e) => return InterpStep::Unavailable(e),
    };
    if stdout.trim().is_empty()
        || stdout.contains("(error")
        || stdout
            .lines()
            .any(|l| l.trim() == "fail" || l.trim() == "none")
    {
        // Diagnostic: dump the failing query so we can distinguish cvc5's
        // interpolation incompleteness from a serialization bug (env-gated).
        if let Ok(path) = std::env::var("MUNUNU_INTERP_DUMP") {
            let _ = std::fs::write(&path, &query);
        }
        return InterpStep::NoInterpolant;
    }
    let raw = match extract_interpolant_body(&stdout) {
        Some(r) => r,
        None => return InterpStep::Unavailable(format!("unparseable cvc5 reply: {stdout:?}")),
    };
    match reparse_over_shared(&raw, shared) {
        Some(bv) => InterpStep::Interpolant(bv),
        None => InterpStep::Unavailable(format!("interpolant did not re-parse: {raw:?}")),
    }
}

/// Run cvc5 on an interpolation query, returning raw stdout.
pub(crate) fn run_cvc5_raw(query: &str, timeout_ms: u32) -> Result<String, String> {
    run_cvc5_raw_cancellable(query, timeout_ms, None)
}

/// [`run_cvc5_raw`] with the portfolio's cancel flag (S1): polled with the wall deadline;
/// once set, cvc5 is killed after [`crate::adapter::MEMBER_CANCEL_GRACE`] and the query
/// reads as `Err` — the same sound abstain as a timed-out one. Measured before this: the
/// interpolation member bailed 350 ms after the first decision on a small liveness monitor,
/// because the in-flight cvc5 query ran to completion before the flag was checked.
pub(crate) fn run_cvc5_raw_cancellable(
    query: &str,
    timeout_ms: u32,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let bin =
        crate::adapter::cvc5::locate_cvc5().map_err(|e| format!("cvc5 unavailable: {e:?}"))?;
    let mut child = Command::new(&bin.path)
        // `--tlimit` is cvc5's own per-run budget; the wall-clock kill below is the
        // hard backstop if it overruns. A timed-out query returns `Err` →
        // `Unavailable` → `Undecided` (sound abstain), never a spurious verdict.
        .args([
            "--lang=smt2",
            "--produce-interpolants",
            &format!("--tlimit={timeout_ms}"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn cvc5: {e}"))?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(query.as_bytes())
        .map_err(|e| format!("write cvc5 stdin: {e}"))?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64 + 2_000);
    let mut cancel_seen: Option<Instant> = None;
    let mut killed = None;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                let cancelled = cancel
                    .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                    && cancel_seen.get_or_insert_with(Instant::now).elapsed()
                        >= crate::adapter::MEMBER_CANCEL_GRACE;
                if cancelled || Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    killed = Some(if cancelled {
                        "cvc5 cancelled — another portfolio member decided first".to_string()
                    } else {
                        format!("cvc5 exceeded {timeout_ms}ms")
                    });
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("wait cvc5: {e}")),
        }
    }
    if let Some(why) = killed {
        return Err(why);
    }
    let mut buf = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_end(&mut buf);
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

/// Locate the MathSAT 5 binary: `MUNUNU_MATHSAT_PATH` or `mathsat` on PATH. Unlike cvc5's
/// `locate_tool` (which invokes `--version`), MathSAT uses `-version` (single dash), so this does
/// a light `-version` probe purely to confirm the binary is invokable; the actual interpolation
/// query surfaces any real problem as an `Err`.
/// Is MathSAT invokable? The optional-tool gate for tests that need it: MathSAT
/// ships in the `mununu-sva-pono` image, not `mununu-sva`, so a test that requires
/// it must SKIP rather than fail when it is absent — an absent optional tool is not
/// a regression, and a red result masks the genuine failures in the `--ignored`
/// sweep (mununu#498 follow-up).
#[cfg(test)]
pub(crate) fn mathsat_available() -> Result<(), String> {
    locate_mathsat().map(|_| ())
}

fn locate_mathsat() -> Result<std::path::PathBuf, String> {
    use std::path::PathBuf;
    let path = std::env::var("MUNUNU_MATHSAT_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("mathsat"));
    match std::process::Command::new(&path).arg("-version").output() {
        Ok(_) => Ok(path),
        Err(e) => Err(format!(
            "mathsat unavailable ({e}); set MUNUNU_MATHSAT_PATH or use the `mununu-sva` image \
             (proof-based lazy-BV interpolation — the must-precondition query cvc5's SyGuS \
             interpolation cannot synthesize)."
        )),
    }
}

/// Run MathSAT 5 on an interpolation query, returning raw stdout. Forces the LAZY BV solver
/// (`-theory.bv.eager=false`) — the eager (bit-blast) solver cannot produce interpolation proofs
/// (`"eager bv solver does not support proof generation"`). The query must use MathSAT's
/// `(! φ :interpolation-group g)` + `(get-interpolant (g))` API (distinct from cvc5's
/// `(get-interpolant I B)`). Output is `<sat-result>\n<interpolant s-expr>`.
pub(crate) fn run_mathsat_raw(query: &str, timeout_ms: u32) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let bin = locate_mathsat()?;
    let mut child = Command::new(&bin)
        .arg("-theory.bv.eager=false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn mathsat: {e}"))?;
    // The taken stdin is a temporary dropped at the end of this statement → EOF is sent.
    child
        .stdin
        .take()
        .unwrap()
        .write_all(query.as_bytes())
        .map_err(|e| format!("write mathsat stdin: {e}"))?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64 + 2_000);
    let mut killed = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    killed = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("wait mathsat: {e}")),
        }
    }
    if killed {
        return Err(format!("mathsat exceeded {timeout_ms}ms"));
    }
    let mut buf = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        let _ = so.read_to_end(&mut buf);
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

/// Extract the interpolant term from cvc5's `(define-fun I () Bool <body>)` reply
/// (cvc5 ≥ 1.x). Falls back to a bare top-level s-expression if `define-fun` is
/// absent.
pub(crate) fn extract_interpolant_body(stdout: &str) -> Option<String> {
    if let Some(p) = stdout.find("(define-fun I () Bool") {
        let after = &stdout[p + "(define-fun I () Bool".len()..];
        return balanced_prefix(after.trim_start()).map(|s| s.to_string());
    }
    // Bare term: take the first non-empty, non-`success` line as a whole.
    for line in stdout.lines() {
        let t = line.trim();
        if !t.is_empty() && t != "success" && !t.starts_with("(error") {
            return Some(t.to_string());
        }
    }
    None
}

/// The leading balanced s-expression (or a bare token) at the start of `s`.
fn balanced_prefix(s: &str) -> Option<&str> {
    let s = s.trim_start();
    if !s.starts_with('(') {
        // bare token up to whitespace or ')'
        let end = s.find([' ', '\n', '\t', ')']).unwrap_or(s.len());
        return if end == 0 { None } else { Some(&s[..end]) };
    }
    let mut depth = 0i32;
    let mut in_pipe = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '|' => in_pipe = !in_pipe,
            '(' if !in_pipe => depth += 1,
            ')' if !in_pipe => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Re-parse a cvc5 interpolant term (over the shared-vocabulary names) back into a
/// z3 `Bool` over the `shared` constants, via [`z3::Solver::from_string`]. Because
/// those constants were interned by name, the parsed AST shares nodes with
/// `shared`, so a later `substitute(frame1 → frame0)` remaps cleanly.
fn reparse_over_shared(raw: &str, shared: &BTreeMap<Nid, BV>) -> Option<Bool> {
    let mut decl = String::new();
    decl.push_str("(set-logic QF_BV)\n");
    for bv in shared.values() {
        // Recover the declared name/width from the const's own rendering.
        decl.push_str(&format!(
            "(declare-fun {} () (_ BitVec {}))\n",
            bv, // Display renders the const's name
            bv.get_size()
        ));
    }
    decl.push_str(&format!("(assert {raw})\n"));
    let solver = z3::Solver::new();
    solver.from_string(decl);
    let asserts = solver.get_assertions();
    asserts.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;

    /// S1 (engine-performance roadmap) — the cvc5 runner honours the portfolio's cancel flag:
    /// a query cvc5 cannot finish quickly (a 64-bit multiplication interpolant under a 30 s
    /// budget) is killed after the member grace once the flag is set, and the runner reports
    /// the cancellation rather than waiting for the budget.
    #[test]
    fn cvc5_runner_kills_the_query_after_the_grace_when_cancelled() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        if crate::adapter::cvc5::locate_cvc5().is_err() {
            eprintln!("SKIP: cvc5 unavailable");
            return;
        }
        const HARD: &str = "(set-logic QF_BV)\n(declare-const a (_ BitVec 64))\n\
            (declare-const b (_ BitVec 64))\n(declare-const c (_ BitVec 64))\n\
            (assert (= c (bvmul a b)))\n(assert (= c #x0000000100000001))\n\
            (get-interpolant I (not (and (= a #x0000000000000001) (= b #x0000000100000001))))\n";
        let cancel = AtomicBool::new(false);
        let t0 = std::time::Instant::now();
        let out = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(100));
                cancel.store(true, Relaxed);
            });
            run_cvc5_raw_cancellable(HARD, 30_000, Some(&cancel))
        });
        let took = t0.elapsed();
        match out {
            Err(why) => assert!(why.contains("cancelled"), "reason names the cancel: {why}"),
            // cvc5 finished the query inside the grace — then nothing was cancelled, and the
            // test cannot say more on this host; it is not a failure of the mechanism.
            Ok(_) => assert!(
                took < crate::adapter::MEMBER_CANCEL_GRACE * 2,
                "finished: {took:?}"
            ),
        }
        assert!(
            took < std::time::Duration::from_secs(10),
            "the 30 s budget was not what ended the query: {took:?}"
        );
    }

    /// Round-trip machinery smoke test: a 4-bit `+2` counter whose `bad = (x==5)`
    /// is unreachable (x stays even) but **not** 1-inductive (x=3 → 5). cvc5 must
    /// return a one-step interpolant that (a) parses, and (b) is non-trivial.
    /// This validates serialize → cvc5 → parse-back end to end.
    #[test]
    fn one_step_interpolant_roundtrips_on_even_counter() {
        // x init 0; next x = x + 2; bad = (x == 5).
        const EVEN: &str = "1 sort bitvec 4\n2 zero 1\n3 constd 1 2\n4 state 1 x\n5 init 1 4 2\n\
                            6 add 1 4 3\n7 next 1 4 6\n8 constd 1 5\n9 sort bitvec 1\n\
                            10 eq 9 4 8\n11 bad 10\n";
        let file = parser::parse(EVEN).expect("parse btor2");
        match one_step_interpolant(&file) {
            OneStepInterp::Interpolant { raw, parsed_ok } => {
                assert!(!raw.is_empty(), "interpolant term should be non-empty");
                // Non-trivial: not literally `true`/`false`.
                assert!(
                    raw != "true" && raw != "false",
                    "expected a non-trivial interpolant, got {raw:?}"
                );
                assert!(
                    parsed_ok,
                    "interpolant {raw:?} must round-trip back to a z3 Bool"
                );
            }
            OneStepInterp::Reachable => panic!("bad=(x==5) is NOT 1-step reachable from x=0"),
            OneStepInterp::Unavailable(why) => {
                // cvc5 may be absent in CI — treat as a skip, not a failure.
                eprintln!("SKIP: cvc5 unavailable: {why}");
            }
        }
    }

    /// The convergence loop reaches a fixpoint and returns `Safe`: `x` holds at 0,
    /// `bad = (x==3)` is unreachable. Exercises the forward-reachability fixpoint
    /// (R grows by interpolants, then `post(R) ⊆ R`).
    #[test]
    fn interp_loop_proves_constant_hold_safe() {
        // x init 0 (2-bit); next x = x (hold); bad = (x == 3).
        const HOLD: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n5 next 1 3 3\n\
                            6 constd 1 3\n7 sort bitvec 1\n8 eq 7 3 6\n9 bad 8\n";
        let file = parser::parse(HOLD).expect("parse btor2");
        match verify_safety_interp(&file, 8, 32, 5_000, 30_000) {
            InterpSafetyVerdict::Safe { iterations } => {
                assert!(iterations >= 1, "should take ≥1 refinement");
            }
            InterpSafetyVerdict::Undecided { reason } if reason.contains("unavailable") => {
                eprintln!("SKIP: cvc5 unavailable")
            }
            other => panic!("expected Safe, got {other:?}"),
        }
    }

    /// The k-schedule proves the `+2` counter safe — the flagship case. `bad =
    /// (x==5)` is unreachable (x stays even) but NOT k-inductive, so native
    /// k-induction abstains; the one-step (k=1) suffix also fails (R grows to
    /// x=3, which one-steps to 5 — a spurious CTI). Deepening the suffix excludes
    /// the odd pre-images until R converges to the even states — an inductive
    /// invariant interpolation *synthesised* and enumeration could not.
    #[test]
    fn interp_loop_proves_even_counter_safe_via_kschedule() {
        const EVEN: &str = "1 sort bitvec 4\n2 zero 1\n3 constd 1 2\n4 state 1 x\n5 init 1 4 2\n\
                            6 add 1 4 3\n7 next 1 4 6\n8 constd 1 5\n9 sort bitvec 1\n\
                            10 eq 9 4 8\n11 bad 10\n";
        let file = parser::parse(EVEN).expect("parse btor2");
        match verify_safety_interp(&file, 16, 32, 5_000, 30_000) {
            InterpSafetyVerdict::Safe { iterations } => assert!(iterations >= 1),
            InterpSafetyVerdict::Undecided { reason } if reason.contains("unavailable") => {
                eprintln!("SKIP: cvc5 unavailable")
            }
            // NEVER Unsafe (bad is genuinely unreachable) — that would be unsound.
            other => panic!("expected Safe via k-schedule, got {other:?}"),
        }
    }

    /// A genuinely reachable `bad` must be `Unsafe`, never `Safe`.
    #[test]
    fn interp_loop_refutes_reachable_bad() {
        // x init 0; next x = 1; bad = (x == 1). Reachable at depth 1.
        const REACH: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 x\n5 init 1 4 2\n\
                             6 next 1 4 3\n7 bad 4\n";
        let file = parser::parse(REACH).expect("parse btor2");
        match verify_safety_interp(&file, 8, 32, 5_000, 30_000) {
            InterpSafetyVerdict::Unsafe { depth } => assert_eq!(depth, 1),
            InterpSafetyVerdict::Undecided { reason } if reason.contains("unavailable") => {
                eprintln!("SKIP: cvc5 unavailable")
            }
            other => panic!("expected Unsafe, got {other:?}"),
        }
    }

    /// `bad` true at `Init` → `Unsafe { depth: 0 }`.
    #[test]
    fn interp_loop_detects_initial_violation() {
        // x init 1; bad = (x == 1). Violated at step 0.
        const INIT_BAD: &str = "1 sort bitvec 1\n2 one 1\n3 state 1 x\n4 init 1 3 2\n\
                                5 next 1 3 2\n6 bad 3\n";
        let file = parser::parse(INIT_BAD).expect("parse btor2");
        match verify_safety_interp(&file, 8, 32, 5_000, 30_000) {
            InterpSafetyVerdict::Unsafe { depth } => assert_eq!(depth, 0),
            InterpSafetyVerdict::Undecided { reason } if reason.contains("unavailable") => {
                eprintln!("SKIP: cvc5 unavailable")
            }
            other => panic!("expected Unsafe depth 0, got {other:?}"),
        }
    }

    /// Differential soundness + coverage sweep over labeled HWMCC bv cases.
    /// `MUNUNU_HWMCC_GT` names a `basename safe|unsafe` file; `MUNUNU_HWMCC_FLAT`
    /// the directory of `*.btor2`. Runs the engine on each (small-first, size-capped
    /// by `MUNUNU_HWMCC_MAXBYTES`), and ASSERTS the soundness invariant: no `Safe`
    /// on a truly-unsafe design and no `Unsafe` on a truly-safe one. Prints the
    /// decide-rate. `#[ignore]`d — needs cvc5 + the external corpus.
    #[test]
    #[ignore = "sweeps the external HWMCC corpus named by MUNUNU_HWMCC_GT/FLAT"]
    fn differential_soundness_hwmcc() {
        let (Ok(gt_path), Ok(flat)) = (
            std::env::var("MUNUNU_HWMCC_GT"),
            std::env::var("MUNUNU_HWMCC_FLAT"),
        ) else {
            eprintln!(
                "SKIPPED: external-corpus differential sweep — set MUNUNU_HWMCC_GT and \
                 MUNUNU_HWMCC_FLAT to run it."
            );
            return;
        };
        let max_bytes: u64 = std::env::var("MUNUNU_HWMCC_MAXBYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30_000);
        // (basename, label) sorted small-file-first.
        let gt = std::fs::read_to_string(&gt_path).expect("read GT");
        let mut cases: Vec<(String, String, u64)> = gt
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let b = it.next()?.to_string();
                let label = it.next()?.to_string();
                let p = format!("{flat}/{b}.btor2");
                let sz = std::fs::metadata(&p).ok()?.len();
                Some((b, label, sz))
            })
            .filter(|(_, _, sz)| *sz <= max_bytes)
            .collect();
        cases.sort_by_key(|(_, _, sz)| *sz);

        let mut decided = 0u32;
        let mut violations: Vec<String> = Vec::new();
        for (b, label, sz) in &cases {
            let content = std::fs::read_to_string(format!("{flat}/{b}.btor2")).unwrap();
            let file = match parser::parse(&content) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let verdict = verify_safety_interp(&file, 16, 24, 4_000, 25_000);
            let v = match &verdict {
                InterpSafetyVerdict::Safe { .. } => "safe",
                InterpSafetyVerdict::Unsafe { .. } => "unsafe",
                InterpSafetyVerdict::Undecided { .. } => "undecided",
            };
            if v != "undecided" {
                decided += 1;
                let sound = v == label;
                eprintln!(
                    "  {} {:>6}B  gt={:<7} engine={:<9} {}",
                    if sound { "✓" } else { "✗✗✗" },
                    sz,
                    label,
                    v,
                    b
                );
                if !sound {
                    violations.push(format!("{b}: gt={label} engine={v}"));
                }
            }
        }
        eprintln!(
            "\nSWEEP: {} cases (≤{}B), decided {}, soundness violations {}",
            cases.len(),
            max_bytes,
            decided,
            violations.len()
        );
        assert!(
            violations.is_empty(),
            "SOUNDNESS VIOLATIONS: {violations:?}"
        );
    }

    /// Probe the full safety verdict on a real HWMCC design (path in
    /// `MUNUNU_INTERP_PROBE`) — the Phase-1 make-or-break. `#[ignore]`d.
    #[test]
    #[ignore = "reads an external BTOR2 file named by MUNUNU_INTERP_PROBE"]
    fn probe_safety_verdict_on_external_design() {
        let Ok(path) = std::env::var("MUNUNU_INTERP_PROBE") else {
            eprintln!(
                "SKIPPED: opt-in probe — set MUNUNU_INTERP_PROBE=/path/to/design.btor2 to run it."
            );
            return;
        };
        let content = std::fs::read_to_string(&path).expect("read design");
        let file = parser::parse(&content).expect("parse btor2");
        let env_u = |k: &str, d: u32| {
            std::env::var(k)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(d)
        };
        let env_u64 = |k: &str, d: u64| {
            std::env::var(k)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(d)
        };
        let t0 = std::time::Instant::now();
        let verdict = verify_safety_interp(
            &file,
            env_u("MUNUNU_INTERP_SUFFIX", 24),
            env_u("MUNUNU_INTERP_ITERS", 64),
            env_u("MUNUNU_INTERP_QTO", 10_000),
            env_u64("MUNUNU_INTERP_OVERALL", 60_000),
        );
        eprintln!(
            "VERDICT on {path} [{}ms]:\n{verdict:?}",
            t0.elapsed().as_millis()
        );
    }

    /// Probe the one-step interpolant on a real HWMCC design (path in
    /// `MUNUNU_INTERP_PROBE`). Prints the raw interpolant so we can eyeball whether
    /// cvc5 synthesises a useful-shaped predicate over the design's registers — the
    /// Phase-1 make-or-break signal. `#[ignore]`d: needs cvc5 + an external file.
    #[test]
    #[ignore = "reads an external BTOR2 file named by MUNUNU_INTERP_PROBE"]
    fn probe_one_step_interpolant_on_external_design() {
        let Ok(path) = std::env::var("MUNUNU_INTERP_PROBE") else {
            eprintln!(
                "SKIPPED: opt-in probe — set MUNUNU_INTERP_PROBE=/path/to/design.btor2 to run it."
            );
            return;
        };
        let content = std::fs::read_to_string(&path).expect("read design");
        let file = parser::parse(&content).expect("parse btor2");
        match one_step_interpolant(&file) {
            OneStepInterp::Interpolant { raw, parsed_ok } => {
                eprintln!("INTERPOLANT (parsed_ok={parsed_ok}):\n{raw}");
            }
            OneStepInterp::Reachable => eprintln!("REACHABLE: bad is 1-step reachable from Init"),
            OneStepInterp::Unavailable(why) => eprintln!("UNAVAILABLE: {why}"),
        }
    }

    /// A genuinely 1-step-reachable `bad` must be classified `Reachable`, not
    /// handed a spurious interpolant.
    #[test]
    fn one_step_reachable_bad_is_detected() {
        // x init 0; next x = 1; bad = (x == 1). Reachable at step 1.
        const REACH: &str = "1 sort bitvec 1\n2 zero 1\n3 one 1\n4 state 1 x\n5 init 1 4 2\n\
                             6 next 1 4 3\n7 bad 4\n";
        let file = parser::parse(REACH).expect("parse btor2");
        match one_step_interpolant(&file) {
            OneStepInterp::Reachable => {}
            OneStepInterp::Interpolant { raw, .. } => {
                // bad(x1) with x1==1 forced by the transition: A ∧ B is SAT, so
                // there is no interpolant. If cvc5 returns one it must be
                // unsatisfiable-vacuous; flag the surprise.
                panic!("expected Reachable (A∧B SAT), got interpolant {raw:?}");
            }
            OneStepInterp::Unavailable(why) => eprintln!("SKIP: cvc5 unavailable: {why}"),
        }
    }
}
