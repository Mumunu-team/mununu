//! Phase 2 · P2.1 — the **shared lazy-refinement core**'s refinement primitive.
//!
//! This is the ONE capability that unifies KMTS-based verification (plan
//! `cube-ic3ia-invariant-discovery.md` §9): given a spurious abstract step — a
//! source region that the abstraction lets reach a target region, but the EXACT
//! transition forbids — synthesise a new predicate that separates them. Both
//! Phase-2 drivers call it: the safety IC3ia frame-ladder (block a spurious CTI)
//! and the 3-valued μ-calculus evaluator (refine a spurious `⊥` obligation),
//! replacing the eager-cube + CEGAR-predicate-split with lazy, interpolation-driven
//! discovery.
//!
//! **Mechanism.** Build `A = source(cur) ∧ T(cur→nx)` and `B = ¬target(nx)` over
//! the *exact* transition ([`encode_design`]). If the step is realizable
//! (`A ∧ target(nx)` SAT) there is nothing to refine. Otherwise cvc5's
//! `get-interpolant` (convention `A ⟹ I ⟹ B`) yields `I(nx)` — an
//! over-approximation of the concrete image of `source` that excludes `target`.
//! The next-state variables are named by their **register name** (current-state
//! ones get a `__cur` suffix), so `I` comes back over register names and parses
//! directly to a [`PredicateExpr`] (via `#318`) — a ready cube dimension.
//!
//! **Soundness.** The returned predicate is only a *candidate cube dimension*.
//! Adding any well-formed predicate refines the abstraction monotonically
//! (Shoham–Grumberg) and can never flip a definite verdict — so a weak/mis-parsed
//! interpolant costs refinement power, never soundness.

use std::collections::{BTreeSet, HashMap};

use z3::ast::{Ast, BV, Bool};

use crate::adapter::btor2::ast::{Btor2File, Nid};
use crate::adapter::btor2::native_interp::{
    QueryError, QueryStop, extract_interpolant_body, run_cvc5_raw, serialize_term,
};
use crate::adapter::btor2::predicate_expr::PredicateExpr;
use crate::adapter::cvc5::parse_interpolant_to_predicate_expr;
use crate::adapter::owner::{mununu_defect, mununu_limitation};
use crate::adapter::sidecar::predicate_image::btor2_encode::{SignalKind, encode_design};

/// Outcome of a refinement query.
#[derive(Debug)]
pub enum RefineOutcome {
    /// The abstract step is spurious; this predicate separates it — a new cube
    /// dimension for the abstraction.
    Predicate(PredicateExpr),
    /// The step is concretely realizable under the exact transition — not
    /// spurious, so no refinement is needed (the driver must look elsewhere).
    Realizable,
    /// No predicate, and why ([`RefineGap`]): the query could not be posed, the solver did
    /// not answer it, or it answered with nothing usable. The caller falls back (a different
    /// predicate source, or abstains) — and says which of these it was (mununu#1188).
    Unavailable(RefineGap),
}

/// Why a refinement query gave no predicate (mununu#1188). The kind is what a caller branches
/// on; before it was typed, every caller read every gap as "the solver had nothing to say" and
/// reported a grammar ceiling for a query that had stopped at its limit, or fell back without
/// a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GapKind {
    /// The query was never sent: the design could not be encoded, an atom names a register
    /// the query cannot address, a term did not serialize, the design has no `bad`.
    NotPosed,
    /// The solver binary (cvc5, MathSAT) was not found.
    SolverMissing,
    /// A solver stopped at the query's limit without an answer: z3 on the realizability or
    /// reachability check, cvc5 or MathSAT on the interpolation query.
    SolverStopped,
    /// The solver died or could not be started.
    SolverFailed,
    /// The run that asked was cancelled.
    Cancelled,
    /// The solver answered: there is no interpolant.
    NoInterpolant,
    /// The solver answered with an interpolant the predicate grammar does not take.
    OutsideGrammar,
}

impl GapKind {
    /// Every kind, for the tests that enumerate them.
    pub const ALL: [GapKind; 7] = [
        GapKind::NotPosed,
        GapKind::SolverMissing,
        GapKind::SolverStopped,
        GapKind::SolverFailed,
        GapKind::Cancelled,
        GapKind::NoInterpolant,
        GapKind::OutsideGrammar,
    ];

    /// Did the solver answer the query? Only then is "no predicate" a statement about the
    /// interpolant (none exists, or the grammar does not take it). On every other kind the
    /// question is still open, and a report that says "grammar ceiling" is false.
    pub fn solver_answered(self) -> bool {
        match self {
            GapKind::NoInterpolant | GapKind::OutsideGrammar => true,
            GapKind::NotPosed
            | GapKind::SolverMissing
            | GapKind::SolverStopped
            | GapKind::SolverFailed
            | GapKind::Cancelled => false,
        }
    }

    /// Which unanswered gap a report names when a step collected several: the one the reader
    /// can least afford to miss. A cancelled run explains everything after it; a missing
    /// solver is the one the user can fix; a stop at the limit says the answer may exist; a
    /// failure is a tool problem; a query never posed is the design's shape. `None` for the
    /// answered kinds, which [`telling_gap`] never returns.
    fn precedence(self) -> Option<u8> {
        match self {
            GapKind::Cancelled => Some(4),
            GapKind::SolverMissing => Some(3),
            GapKind::SolverStopped => Some(2),
            GapKind::SolverFailed => Some(1),
            GapKind::NotPosed => Some(0),
            GapKind::NoInterpolant | GapKind::OutsideGrammar => None,
        }
    }
}

/// A refinement query that gave no predicate: the kind, and the fact as its producer words
/// it. `what` names no remedy for a query limit — the caller knows which surface it reports
/// on and whether anything there changes the limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefineGap {
    pub kind: GapKind,
    pub what: String,
}

impl RefineGap {
    pub(crate) fn new(kind: GapKind, what: impl Into<String>) -> Self {
        RefineGap {
            kind,
            what: what.into(),
        }
    }

    /// What a report says of a refinement step this gap cut short; `None` when the solver
    /// answered the gap's query, because the step was then not cut short (mununu#1188).
    ///
    /// One wording per kind, for every caller: `step` is the subject in the caller's words,
    /// and `instead` what the caller's surface offers in the step's place. The limit of a
    /// refinement query is a literal at its call site — no flag, variable or sidecar key
    /// reaches it — so a stop at it is said to be fixed, and the reader is never told to
    /// raise a budget that does not govern it.
    pub(crate) fn cut_short(&self, step: &str, instead: Option<&str>) -> Option<String> {
        let what = &self.what;
        Some(match self.kind {
            GapKind::NoInterpolant | GapKind::OutsideGrammar => return None,
            GapKind::SolverStopped => mununu_limitation(
                &format!("{step} did not finish: {what}; the query limit is fixed on this engine"),
                instead,
            ),
            GapKind::SolverMissing => format!("{step} did not run: {what}"),
            GapKind::Cancelled => format!("{step} did not finish: {what}"),
            GapKind::SolverFailed => mununu_defect(&format!("{step} did not finish: {what}")),
            GapKind::NotPosed => {
                mununu_limitation(&format!("{step} could not be posed: {what}"), instead)
            }
        })
    }

    /// The gap a solver query that gave no reply leaves.
    fn of_query(e: QueryError) -> Self {
        let kind = match e.stop {
            QueryStop::Missing => GapKind::SolverMissing,
            QueryStop::Limit => GapKind::SolverStopped,
            QueryStop::Cancelled => GapKind::Cancelled,
            QueryStop::Failed => GapKind::SolverFailed,
        };
        RefineGap::new(kind, e.what)
    }

    /// z3 returned `unknown` on one of the checks that decide whether to interpolate at all.
    /// Its only limit here is the query's, so this is a stop at that limit.
    fn z3_stopped(check: &str, timeout_ms: u32) -> Self {
        RefineGap::new(
            GapKind::SolverStopped,
            format!(
                "z3 gave no answer on the {check} check within its {timeout_ms} ms query limit \
                 — a budget stop, not an interpolation gap"
            ),
        )
    }

    fn not_serialized(side: &str) -> Self {
        RefineGap::new(
            GapKind::NotPosed,
            format!("side {side} of the interpolation query did not serialize"),
        )
    }

    /// The design did not encode, so no query over it exists.
    fn not_encoded(e: &impl std::fmt::Display) -> Self {
        RefineGap::new(GapKind::NotPosed, format!("encode: {e}"))
    }

    /// The predicate in cvc5's reply, or the gap the reply leaves.
    fn of_cvc5_reply(stdout: &str) -> Result<PredicateExpr, RefineGap> {
        if cvc5_reply_has_no_interpolant(stdout) {
            return Err(RefineGap::of_unusable_cvc5_reply(stdout));
        }
        parse_interpolant_to_predicate_expr(stdout)
            .ok_or_else(|| RefineGap::of_unusable_cvc5_reply(stdout))
    }

    /// The gap a cvc5 reply that gave no predicate leaves: it said there is no interpolant,
    /// or it gave one the grammar does not take. Both are answers.
    fn of_unusable_cvc5_reply(stdout: &str) -> Self {
        if cvc5_reply_has_no_interpolant(stdout) {
            return RefineGap::new(GapKind::NoInterpolant, "cvc5 produced no interpolant");
        }
        RefineGap::new(
            GapKind::OutsideGrammar,
            format!(
                "interpolant outside the parseable grammar: {}",
                extract_interpolant_body(stdout)
                    .as_deref()
                    .unwrap_or("<none>")
            ),
        )
    }
}

/// Does cvc5's reply to a `get-interpolant` say it has none (nothing, an error, `fail`,
/// `none`)?
fn cvc5_reply_has_no_interpolant(stdout: &str) -> bool {
    stdout.trim().is_empty()
        || stdout.contains("(error")
        || stdout
            .lines()
            .any(|l| l.trim() == "fail" || l.trim() == "none")
}

impl std::fmt::Display for RefineGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.what)
    }
}

/// The gap a report names for a step that asked several queries and got no predicate: the
/// highest-ranked gap among the queries the solver did NOT answer (cancelled, then a missing
/// solver, a stop at the limit, a failure, a query never posed). `None` means every query was
/// answered — only then did the step meet the interpolant itself, and only then may its
/// report say "grammar ceiling".
pub fn telling_gap(gaps: &[RefineGap]) -> Option<&RefineGap> {
    gaps.iter()
        .filter_map(|g| g.kind.precedence().map(|rank| (rank, g)))
        // `max_by_key` keeps the last of equals; the first is the one that happened first.
        .fold(
            None,
            |best: Option<(u8, &RefineGap)>, (rank, g)| match best {
                Some((top, _)) if top >= rank => best,
                _ => Some((rank, g)),
            },
        )
        .map(|(_, g)| g)
}

/// P2.1 — synthesise a separating predicate for a (possibly spurious) one-step
/// abstract transition `source(cur) → target(nx)` over the exact transition of
/// `file`. See the module docs.
pub fn synthesize_refinement_predicate(
    file: &Btor2File,
    source: &[PredicateExpr],
    target: &[PredicateExpr],
    timeout_ms: u32,
) -> RefineOutcome {
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = match encode_design(file) {
            Ok(v) => v,
            Err(e) => return RefineOutcome::Unavailable(RefineGap::not_encoded(&e)),
        };

        // Register symbol → NID (only named state cells are addressable predicates).
        let name_to_nid: HashMap<String, Nid> = view
            .signals
            .iter()
            .filter(|s| s.kind == SignalKind::State)
            .filter_map(|s| s.symbol.clone().map(|sym| (sym, s.nid)))
            .collect();
        let nid_to_name: HashMap<Nid, String> =
            name_to_nid.iter().map(|(n, &d)| (d, n.clone())).collect();

        // Controlled-name interface: next-state cells keep the register NAME (so the
        // interpolant, over the shared vocabulary, parses straight to a PredicateExpr);
        // current-state cells get a `__cur` suffix; inputs an `__in` suffix.
        let named = |suffix: &str| -> HashMap<Nid, BV> {
            view.state_curr
                .iter()
                .filter_map(|(nid, bv)| {
                    let sym = nid_to_name.get(nid)?;
                    Some((*nid, BV::new_const(format!("{sym}{suffix}"), bv.get_size())))
                })
                .collect()
        };
        let cur = named("__cur");
        let nx = named("");
        let inp: HashMap<Nid, BV> = view
            .inputs
            .iter()
            .map(|(nid, bv)| (*nid, BV::new_const(format!("in{nid}__in"), bv.get_size())))
            .collect();

        // Instantiate the transition over the interface names.
        let mut subs: Vec<(&BV, &BV)> = Vec::new();
        for (nid, bv) in &view.state_curr {
            if let Some(c) = cur.get(nid) {
                subs.push((bv, c));
            }
        }
        for (nid, bv) in &view.state_next {
            if let Some(n) = nx.get(nid) {
                subs.push((bv, n));
            }
        }
        for (nid, bv) in &view.inputs {
            if let Some(i) = inp.get(nid) {
                subs.push((bv, i));
            }
        }
        let transition = view.transition.substitute(&subs);

        // Build the source region over `cur` and the target region over `nx`.
        let build = |ps: &[PredicateExpr], frame: &HashMap<Nid, BV>| -> Option<Bool> {
            let lookup = |name: &str| -> Option<BV> {
                name_to_nid
                    .get(name)
                    .and_then(|nid| frame.get(nid))
                    .cloned()
            };
            let mut conj = Vec::new();
            for p in ps {
                conj.push(p.build_constraint(&lookup)?);
            }
            Some(if conj.is_empty() {
                Bool::from_bool(true)
            } else {
                Bool::and(&conj.iter().collect::<Vec<_>>())
            })
        };
        let source_bool = match build(source, &cur) {
            Some(b) => b,
            None => {
                return RefineOutcome::Unavailable(RefineGap::new(
                    GapKind::NotPosed,
                    PredicateExpr::unknown_register_reason(
                        "source references an unknown register",
                        source,
                        name_to_nid.keys().map(String::as_str),
                    ),
                ));
            }
        };
        let target_bool = match build(target, &nx) {
            Some(b) => b,
            None => {
                return RefineOutcome::Unavailable(RefineGap::new(
                    GapKind::NotPosed,
                    PredicateExpr::unknown_register_reason(
                        "target references an unknown register",
                        target,
                        name_to_nid.keys().map(String::as_str),
                    ),
                ));
            }
        };

        let mk_solver = || {
            let s = z3::Solver::new();
            let mut p = z3::Params::new();
            p.set_u32("timeout", timeout_ms);
            s.set_params(&p);
            s
        };

        // A = source(cur) ∧ T. Realizable ⇔ A ∧ target(nx) SAT.
        let a = Bool::and(&[&source_bool, &transition]);
        {
            let s = mk_solver();
            s.assert(&a);
            s.assert(&target_bool);
            match s.check() {
                z3::SatResult::Sat => return RefineOutcome::Realizable,
                z3::SatResult::Unknown => {
                    return RefineOutcome::Unavailable(RefineGap::z3_stopped(
                        "realizability",
                        timeout_ms,
                    ));
                }
                z3::SatResult::Unsat => {} // spurious → interpolate
            }
        }

        // Spurious: interpolate A vs B = ¬target(nx). Shared vocabulary = the
        // next-state register names ⇒ I is a state predicate over them.
        let b = target_bool.not();
        let (a_decls, a_body) = match serialize_term(&a) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable(RefineGap::not_serialized("A")),
        };
        let (b_decls, b_body) = match serialize_term(&b) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable(RefineGap::not_serialized("B")),
        };
        let mut decls: BTreeSet<String> = BTreeSet::new();
        decls.extend(a_decls);
        decls.extend(b_decls);
        let mut query =
            String::from("(set-logic QF_BV)\n(set-option :produce-interpolants true)\n");
        for d in &decls {
            query.push_str(d);
            query.push('\n');
        }
        query.push_str(&format!(
            "(assert {a_body})\n(get-interpolant I {b_body})\n(exit)\n"
        ));

        let stdout = match run_cvc5_raw(&query, timeout_ms) {
            Ok(s) => s,
            Err(e) => return RefineOutcome::Unavailable(RefineGap::of_query(e)),
        };
        match RefineGap::of_cvc5_reply(&stdout) {
            Ok(pe) => RefineOutcome::Predicate(pe),
            Err(gap) => RefineOutcome::Unavailable(gap),
        }
    })
}

/// F.1 must-precondition interpolant (2026-07-24) — the RECOVERABILITY-correct query that the
/// safety [`synthesize_refinement_predicate`] cannot pose. For a ⊥ cube `source` and the `good`
/// target, interpolate the MUST-PRECONDITION of `good`: `A = source ∧ T_a ∧ good(next_a)` (current
/// states whose transition CAN reach good) vs `B = source ∧ T_b ∧ ¬good(next_b)` (those whose
/// transition can reach ¬good), over TWO independent next-state frames (`__a` / `__b`) so the only
/// shared vocabulary is the CURRENT state — the interpolant `I(cur)` is then the invariant
/// separating good-reaching from ¬good-reaching current states, i.e. the WP relation the ⊥ needs.
///
/// Solved by **MathSAT** proof-based lazy-BV interpolation ([`crate::adapter::btor2::native_interp::run_mathsat_raw`]);
/// cvc5's SyGuS BV interpolation returns `fail` on this shape. MathSAT emits the WP as
/// `(= K (ite <cond> K …))`; `<cond>` (e.g. `(= data target)`) is the discovered relational
/// invariant, extracted below. `Unavailable` when: mathsat absent / no interpolant (a
/// nondeterministic cur reaching BOTH good and ¬good — the `A∧B` SAT case) / a `good`/`source` atom
/// over a non-state register / an interpolant outside the parseable grammar.
pub fn must_precondition_interpolant(
    file: &Btor2File,
    source: &[PredicateExpr],
    good: &[PredicateExpr],
    timeout_ms: u32,
) -> RefineOutcome {
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = match encode_design(file) {
            Ok(v) => v,
            Err(e) => return RefineOutcome::Unavailable(RefineGap::not_encoded(&e)),
        };
        let name_to_nid: HashMap<String, Nid> = view
            .signals
            .iter()
            .filter(|s| s.kind == SignalKind::State)
            .filter_map(|s| s.symbol.clone().map(|sym| (sym, s.nid)))
            .collect();
        let nid_to_name: HashMap<Nid, String> =
            name_to_nid.iter().map(|(n, &d)| (d, n.clone())).collect();
        // H.E — named COMBINATIONAL outputs (`cs_n = f(state)`): their z3 value is `signal_bvs[nid]`
        // over the canonical `state_curr` + `inputs`. A recoverability `good` over a combinational
        // output (the corpus case — `cs_n==15`, `scl_padoen_o==1`) resolves through this, not the
        // state-cell map.
        let comb_name_to_nid: HashMap<String, Nid> = view
            .signals
            .iter()
            .filter(|s| s.kind == SignalKind::Combinational)
            .filter_map(|s| s.symbol.clone().map(|sym| (sym, s.nid)))
            .collect();
        // `cur` is BARE-named so the interpolant — over the SHARED cur vocabulary — parses to a
        // clean state predicate; the two next frames get distinct suffixes.
        let frame = |suffix: &str| -> HashMap<Nid, BV> {
            view.state_curr
                .iter()
                .filter_map(|(nid, bv)| {
                    let sym = nid_to_name.get(nid)?;
                    Some((*nid, BV::new_const(format!("{sym}{suffix}"), bv.get_size())))
                })
                .collect()
        };
        let cur = frame("");
        let nx_a = frame("__a");
        let nx_b = frame("__b");
        let inp = |suffix: &str| -> HashMap<Nid, BV> {
            view.inputs
                .iter()
                .map(|(nid, bv)| {
                    (
                        *nid,
                        BV::new_const(format!("in{nid}{suffix}"), bv.get_size()),
                    )
                })
                .collect()
        };
        let in_a = inp("__ina");
        let in_b = inp("__inb");
        let in_cur = inp("__incur");
        let instantiate = |nx: &HashMap<Nid, BV>, ip: &HashMap<Nid, BV>| -> Bool {
            let mut subs: Vec<(&BV, &BV)> = Vec::new();
            for (nid, bv) in &view.state_curr {
                if let Some(c) = cur.get(nid) {
                    subs.push((bv, c));
                }
            }
            for (nid, bv) in &view.state_next {
                if let Some(n) = nx.get(nid) {
                    subs.push((bv, n));
                }
            }
            for (nid, bv) in &view.inputs {
                if let Some(i) = ip.get(nid) {
                    subs.push((bv, i));
                }
            }
            view.transition.substitute(&subs)
        };
        let t_a = instantiate(&nx_a, &in_a);
        let t_b = instantiate(&nx_b, &in_b);
        let build =
            |ps: &[PredicateExpr], sf: &HashMap<Nid, BV>, inf: &HashMap<Nid, BV>| -> Option<Bool> {
                let lookup = |name: &str| -> Option<BV> {
                    if let Some(nid) = name_to_nid.get(name) {
                        return sf.get(nid).cloned();
                    }
                    // A combinational output: substitute its `signal_bvs` value (over the canonical
                    // state_curr + inputs) INTO this frame (state_curr→sf, inputs→inf). Sound for a
                    // STATE-ONLY combinational (the input sub is a no-op); an input-dependent one is
                    // resolved against this frame's inputs (recoverability goods are state-only in
                    // practice — `cs_n` is a function of the `cfg_tgt_sel` / `cs_int_n` registers).
                    let &nid = comb_name_to_nid.get(name)?;
                    let bv = view.signal_bvs.get(&nid)?;
                    let mut subs: Vec<(&BV, &BV)> = Vec::new();
                    for (n, b) in &view.state_curr {
                        if let Some(x) = sf.get(n) {
                            subs.push((b, x));
                        }
                    }
                    for (n, b) in &view.inputs {
                        if let Some(x) = inf.get(n) {
                            subs.push((b, x));
                        }
                    }
                    Some(bv.substitute(&subs))
                };
                let mut conj = Vec::new();
                for p in ps {
                    conj.push(p.build_constraint(&lookup)?);
                }
                Some(if conj.is_empty() {
                    Bool::from_bool(true)
                } else {
                    Bool::and(&conj.iter().collect::<Vec<_>>())
                })
            };
        let source_bool = match build(source, &cur, &in_cur) {
            Some(b) => b,
            None => {
                return RefineOutcome::Unavailable(RefineGap::new(
                    GapKind::NotPosed,
                    "source over an unresolvable register",
                ));
            }
        };
        let good_a = match build(good, &nx_a, &in_a) {
            Some(b) => b,
            None => {
                return RefineOutcome::Unavailable(RefineGap::new(
                    GapKind::NotPosed,
                    "good over an unresolvable register",
                ));
            }
        };
        let good_b = match build(good, &nx_b, &in_b) {
            Some(b) => b,
            None => {
                return RefineOutcome::Unavailable(RefineGap::new(
                    GapKind::NotPosed,
                    "good over a non-state register",
                ));
            }
        };
        let a = Bool::and(&[&source_bool, &t_a, &good_a]);
        let b = Bool::and(&[&source_bool, &t_b, &good_b.not()]);
        let (a_decls, a_body) = match serialize_term(&a) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable(RefineGap::not_serialized("A")),
        };
        let (b_decls, b_body) = match serialize_term(&b) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable(RefineGap::not_serialized("B")),
        };
        let mut decls: BTreeSet<String> = BTreeSet::new();
        decls.extend(a_decls);
        decls.extend(b_decls);
        let mut query = String::from("(set-option :produce-interpolants true)\n");
        for d in &decls {
            query.push_str(d);
            query.push('\n');
        }
        query.push_str(&format!(
            "(assert (! {a_body} :interpolation-group g1))\n\
             (assert (! {b_body} :interpolation-group g2))\n\
             (check-sat)\n(get-interpolant (g1))\n"
        ));
        let stdout = match crate::adapter::btor2::native_interp::run_mathsat_raw(&query, timeout_ms)
        {
            Ok(s) => s,
            Err(e) => return RefineOutcome::Unavailable(RefineGap::of_query(e)),
        };
        parse_mathsat_interpolant(&stdout)
    })
}

/// Parse MathSAT's `get-interpolant` reply (`<sat-result>\n<interpolant s-expr>`) into a
/// [`PredicateExpr`]. The must-precondition WP is emitted as `(= K (ite <cond> K <else>))` — the
/// `ite` CONDITION is the relational invariant, and the rich #318 parser rejects `ite` directly,
/// so it is peeled off first. Falls back to parsing the whole term (a plain comparison).
fn parse_mathsat_interpolant(stdout: &str) -> RefineOutcome {
    let Some(term) = stdout
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with('(') && !l.starts_with("(error"))
    else {
        let head = stdout.lines().next().unwrap_or("").trim();
        return RefineOutcome::Unavailable(RefineGap::new(
            GapKind::NoInterpolant,
            format!("mathsat: no interpolant (status `{head}`)"),
        ));
    };
    let candidate = ite_condition(term).unwrap_or_else(|| term.to_string());
    // Reuse the rich #318 parser by wrapping the candidate in cvc5's `define-fun` envelope.
    let wrapped = format!("(define-fun I () Bool {candidate})");
    match parse_interpolant_to_predicate_expr(&wrapped) {
        Some(pe) => RefineOutcome::Predicate(pe),
        None => RefineOutcome::Unavailable(RefineGap::new(
            GapKind::OutsideGrammar,
            format!("mathsat interpolant outside grammar: {term}"),
        )),
    }
}

/// The first `(ite <cond> …)` condition s-expression in `term` (the WP relation), if present.
fn ite_condition(term: &str) -> Option<String> {
    let p = term.find("(ite ")?;
    let after = term[p + "(ite ".len()..].trim_start();
    if !after.starts_with('(') {
        return after.split_whitespace().next().map(str::to_string);
    }
    let mut depth = 0usize;
    for (i, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(after[..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// P2.3b — the Craig interpolant AT CUT `cut` of the depth-`depth` BMC unrolling
/// `Init(s0) ∧ T(s0,s1) ∧ … ∧ T(s_{depth-1}, s_depth)` with `bad` at any step
/// `≥ cut`: `A = Init ∧ (prefix to s_cut)` vs `B = ¬(bad reachable from s_cut
/// within the suffix)`. `I(s_cut)` — over-approximates the states reachable in
/// `cut` steps AND excludes states that reach `bad` in the remaining suffix — is a
/// **reachability-constraining** predicate. Sweeping `cut` (see
/// [`sequence_interpolant_predicates`]) is the targeted per-trace refinement IC3ia
/// needs; a single cut is what [`synthesize_reachability_predicate`] uses.
#[allow(clippy::needless_range_loop)] // `m` indexes both `state(m)` and `inp[m]`
fn interpolant_at_cut(
    file: &Btor2File,
    cut: usize,
    depth: usize,
    timeout_ms: u32,
) -> RefineOutcome {
    let depth = depth.max(1);
    let cut = cut.clamp(1, depth);
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = match encode_design(file) {
            Ok(v) => v,
            Err(e) => return RefineOutcome::Unavailable(RefineGap::not_encoded(&e)),
        };
        let nid_to_name: HashMap<Nid, String> = view
            .signals
            .iter()
            .filter(|s| s.kind == SignalKind::State)
            .filter_map(|s| s.symbol.clone().map(|sym| (s.nid, sym)))
            .collect();

        // s0 = `<reg>__cur`; the cut state s_cut = `<reg>` (interpolation vocabulary,
        // so `I` parses to a predicate); every other step = `<reg>__f<j>`.
        let mk_state = |tag: &dyn Fn(&str) -> String| -> HashMap<Nid, BV> {
            view.state_curr
                .iter()
                .filter_map(|(nid, bv)| {
                    nid_to_name
                        .get(nid)
                        .map(|sym| (*nid, BV::new_const(tag(sym), bv.get_size())))
                })
                .collect()
        };
        let s0 = mk_state(&|sym| format!("{sym}__cur"));
        let mut frames: Vec<HashMap<Nid, BV>> = Vec::new(); // frames[m-1] = state at step m (1..=depth)
        for m in 1..=depth {
            if m == cut {
                frames.push(mk_state(&|sym| sym.to_string()));
            } else {
                frames.push(mk_state(&|sym| format!("{sym}__f{m}")));
            }
        }
        let inp: Vec<HashMap<Nid, BV>> = (0..=depth)
            .map(|t| {
                view.inputs
                    .iter()
                    .map(|(nid, bv)| (*nid, BV::new_const(format!("in{nid}__t{t}"), bv.get_size())))
                    .collect()
            })
            .collect();
        // State at step m: s0 for m == 0, else frames[m-1].
        let state = |m: usize| -> &HashMap<Nid, BV> { if m == 0 { &s0 } else { &frames[m - 1] } };

        let trans =
            |src: &HashMap<Nid, BV>, dst: &HashMap<Nid, BV>, iv: &HashMap<Nid, BV>| -> Bool {
                let mut subs: Vec<(&BV, &BV)> = Vec::new();
                for (nid, bv) in &view.state_curr {
                    if let Some(s) = src.get(nid) {
                        subs.push((bv, s));
                    }
                }
                for (nid, bv) in &view.state_next {
                    if let Some(d) = dst.get(nid) {
                        subs.push((bv, d));
                    }
                }
                for (nid, bv) in &view.inputs {
                    if let Some(i) = iv.get(nid) {
                        subs.push((bv, i));
                    }
                }
                view.transition.substitute(&subs)
            };
        let one1 = BV::from_u64(1, 1);
        let bad_ops: Vec<Nid> = file
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                crate::adapter::btor2::ast::Node::Bad { signal } => Some(signal.nid()),
                _ => None,
            })
            .collect();
        if bad_ops.is_empty() {
            return RefineOutcome::Unavailable(RefineGap::new(
                GapKind::NotPosed,
                "design has no `bad`",
            ));
        }
        let bad_over = |st: &HashMap<Nid, BV>, iv: &HashMap<Nid, BV>| -> Bool {
            let mut subs: Vec<(&BV, &BV)> = Vec::new();
            for (nid, bv) in &view.state_curr {
                if let Some(s) = st.get(nid) {
                    subs.push((bv, s));
                }
            }
            for (nid, bv) in &view.inputs {
                if let Some(i) = iv.get(nid) {
                    subs.push((bv, i));
                }
            }
            let disj: Vec<Bool> = bad_ops
                .iter()
                .filter_map(|op| {
                    view.signal_bvs
                        .get(op)
                        .or_else(|| view.state_curr.get(op))
                        .map(|bv| bv.substitute(&subs).eq(&one1))
                })
                .collect();
            if disj.is_empty() {
                Bool::from_bool(false)
            } else {
                Bool::or(&disj.iter().collect::<Vec<_>>())
            }
        };

        let init = {
            let subs: Vec<(&BV, &BV)> = view
                .state_curr
                .iter()
                .filter_map(|(nid, bv)| s0.get(nid).map(|c| (bv, c)))
                .collect();
            let conj: Vec<Bool> = file
                .lines
                .iter()
                .filter_map(|l| match &l.node {
                    crate::adapter::btor2::ast::Node::Init { state, value, .. } => {
                        let c = s0.get(state)?;
                        let vbv = view
                            .signal_bvs
                            .get(&value.nid())
                            .or_else(|| view.state_curr.get(&value.nid()))?
                            .substitute(&subs);
                        Some(c.eq(&vbv))
                    }
                    _ => None,
                })
                .collect();
            if conj.is_empty() {
                Bool::from_bool(true)
            } else {
                Bool::and(&conj.iter().collect::<Vec<_>>())
            }
        };

        // A = Init(s0) ∧ prefix to s_cut  (steps 0→1 … (cut-1)→cut).
        let mut a_terms = vec![init];
        for m in 0..cut {
            a_terms.push(trans(state(m), state(m + 1), &inp[m]));
        }
        let a = Bool::and(&a_terms.iter().collect::<Vec<_>>());

        // reach = bad reachable from s_cut within the suffix:
        //   ⋁_{j=cut}^{depth} [ (⋀_{m=cut}^{j-1} T(s_m→s_{m+1})) ∧ bad(s_j) ].
        let mut reach_disj: Vec<Bool> = Vec::new();
        for j in cut..=depth {
            let mut path: Vec<Bool> = Vec::new();
            for m in cut..j {
                path.push(trans(state(m), state(m + 1), &inp[m]));
            }
            path.push(bad_over(state(j), &inp[j]));
            reach_disj.push(Bool::and(&path.iter().collect::<Vec<_>>()));
        }
        let reach = Bool::or(&reach_disj.iter().collect::<Vec<_>>());
        let b = reach.not();

        // Realizable (bad reachable from Init's prefix through the suffix) ⇒ no interpolant.
        {
            let solver = z3::Solver::new();
            let mut params = z3::Params::new();
            params.set_u32("timeout", timeout_ms);
            solver.set_params(&params);
            solver.assert(&a);
            solver.assert(&reach);
            match solver.check() {
                z3::SatResult::Sat => return RefineOutcome::Realizable,
                z3::SatResult::Unknown => {
                    return RefineOutcome::Unavailable(RefineGap::z3_stopped(
                        "reachability",
                        timeout_ms,
                    ));
                }
                z3::SatResult::Unsat => {}
            }
        }

        let (a_decls, a_body) = match serialize_term(&a) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable(RefineGap::not_serialized("A")),
        };
        let (b_decls, b_body) = match serialize_term(&b) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable(RefineGap::not_serialized("B")),
        };
        let mut decls: BTreeSet<String> = BTreeSet::new();
        decls.extend(a_decls);
        decls.extend(b_decls);
        let mut query =
            String::from("(set-logic QF_BV)\n(set-option :produce-interpolants true)\n");
        for d in &decls {
            query.push_str(d);
            query.push('\n');
        }
        query.push_str(&format!(
            "(assert {a_body})\n(get-interpolant I {b_body})\n(exit)\n"
        ));
        let stdout = match run_cvc5_raw(&query, timeout_ms) {
            Ok(s) => s,
            Err(e) => return RefineOutcome::Unavailable(RefineGap::of_query(e)),
        };
        match RefineGap::of_cvc5_reply(&stdout) {
            Ok(pe) => RefineOutcome::Predicate(pe),
            Err(gap) => RefineOutcome::Unavailable(gap),
        }
    })
}

/// P2.3 — a single reachability predicate: the interpolant at cut 1 of a
/// depth-`suffix_depth` unrolling. See [`interpolant_at_cut`].
pub fn synthesize_reachability_predicate(
    file: &Btor2File,
    suffix_depth: usize,
    timeout_ms: u32,
) -> RefineOutcome {
    interpolant_at_cut(file, 1, suffix_depth, timeout_ms)
}

/// What [`discover_relational_predicates`] found, and why it stopped when the reason was not
/// its own (mununu#1188).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// The discovered predicates, most-general last.
    pub predicates: Vec<PredicateExpr>,
    /// Why the iteration ended, when it did not end at a fixpoint, at its iteration cap or on
    /// `bad` being one step from the region. A gap whose kind is not
    /// [`GapKind::solver_answered`] means the discovery was cut short, not exhausted.
    pub stopped: Option<RefineGap>,
}

/// **Emergent-K** — discover RELATIONAL invariants by ITERATIVE forward
/// interpolation. Starting from `R = Init`, repeatedly interpolate
/// `A = R(s0) ∧ T(s0,s1)` against `B = ¬bad(s1)`; the interpolant `I(s1)` (a
/// reachable-state over-approximation excluding `bad`) is parsed to a
/// [`PredicateExpr`] and `R` grows by it. The key (de-risked 2026-07-13): from
/// concrete `Init` the first interpolant is concrete values, but as `R` grows to
/// span a relational region cvc5 GENERALISES the interpolant to the relation
/// (e.g. `data == target`) — an "emergent" predicate that no *syntactic* extractor
/// finds. Returns the discovered predicates (the relational ones are the payoff),
/// most-general last. Stops at a fixpoint, at `max_iters`, when `bad` is one-step
/// reachable from `R` (the invariant fails) — or on a gap, which [`Discovery::stopped`]
/// carries: an interpolant the grammar does not take (the grammar ceiling), a solver that
/// stopped at `timeout_ms` or was not there, a query that could not be posed.
pub fn discover_relational_predicates(
    file: &Btor2File,
    max_iters: usize,
    timeout_ms: u32,
) -> Discovery {
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = match encode_design(file) {
            Ok(v) => v,
            Err(e) => {
                return Discovery {
                    predicates: Vec::new(),
                    stopped: Some(RefineGap::not_encoded(&e)),
                };
            }
        };
        let nid_to_name: HashMap<Nid, String> = view
            .signals
            .iter()
            .filter(|s| s.kind == SignalKind::State)
            .filter_map(|s| s.symbol.clone().map(|sym| (s.nid, sym)))
            .collect();
        let mk_state = |tag: &dyn Fn(&str) -> String| -> HashMap<Nid, BV> {
            view.state_curr
                .iter()
                .filter_map(|(nid, bv)| {
                    nid_to_name
                        .get(nid)
                        .map(|sym| (*nid, BV::new_const(tag(sym), bv.get_size())))
                })
                .collect()
        };
        let s0 = mk_state(&|sym| format!("{sym}__cur")); // current state (R's vocabulary)
        let s1 = mk_state(&|sym| sym.to_string()); // next state (interpolant vocabulary)
        let inp0: HashMap<Nid, BV> = view
            .inputs
            .iter()
            .map(|(nid, bv)| (*nid, BV::new_const(format!("in{nid}"), bv.get_size())))
            .collect();
        // T(s0 → s1).
        let transition = {
            let mut subs: Vec<(&BV, &BV)> = Vec::new();
            for (nid, bv) in &view.state_curr {
                if let Some(s) = s0.get(nid) {
                    subs.push((bv, s));
                }
            }
            for (nid, bv) in &view.state_next {
                if let Some(d) = s1.get(nid) {
                    subs.push((bv, d));
                }
            }
            for (nid, bv) in &view.inputs {
                if let Some(i) = inp0.get(nid) {
                    subs.push((bv, i));
                }
            }
            view.transition.substitute(&subs)
        };
        let one1 = BV::from_u64(1, 1);
        let bad_ops: Vec<Nid> = file
            .lines
            .iter()
            .filter_map(|l| match &l.node {
                crate::adapter::btor2::ast::Node::Bad { signal } => Some(signal.nid()),
                _ => None,
            })
            .collect();
        // `bad` over `s1`.
        let bad_s1 = {
            let subs: Vec<(&BV, &BV)> = view
                .state_curr
                .iter()
                .filter_map(|(nid, bv)| s1.get(nid).map(|s| (bv, s)))
                .collect();
            let disj: Vec<Bool> = bad_ops
                .iter()
                .filter_map(|op| {
                    view.signal_bvs
                        .get(op)
                        .or_else(|| view.state_curr.get(op))
                        .map(|bv| bv.substitute(&subs).eq(&one1))
                })
                .collect();
            if disj.is_empty() {
                Bool::from_bool(false)
            } else {
                Bool::or(&disj.iter().collect::<Vec<_>>())
            }
        };
        // Init(s0).
        let init = {
            let subs: Vec<(&BV, &BV)> = view
                .state_curr
                .iter()
                .filter_map(|(nid, bv)| s0.get(nid).map(|c| (bv, c)))
                .collect();
            let conj: Vec<Bool> = file
                .lines
                .iter()
                .filter_map(|l| match &l.node {
                    crate::adapter::btor2::ast::Node::Init { state, value, .. } => {
                        let c = s0.get(state)?;
                        let vbv = view
                            .signal_bvs
                            .get(&value.nid())
                            .or_else(|| view.state_curr.get(&value.nid()))?
                            .substitute(&subs);
                        Some(c.eq(&vbv))
                    }
                    _ => None,
                })
                .collect();
            if conj.is_empty() {
                Bool::from_bool(true)
            } else {
                Bool::and(&conj.iter().collect::<Vec<_>>())
            }
        };
        // Build a PredicateExpr as a z3 Bool over `s0` (to grow `R`).
        let over_s0 = |p: &PredicateExpr| -> Option<Bool> {
            let name_to_nid: HashMap<&str, Nid> =
                nid_to_name.iter().map(|(n, s)| (s.as_str(), *n)).collect();
            let lookup = |name: &str| name_to_nid.get(name).and_then(|nid| s0.get(nid)).cloned();
            p.build_constraint(&lookup)
        };

        let mk = || {
            let s = z3::Solver::new();
            let mut pr = z3::Params::new();
            pr.set_u32("timeout", timeout_ms);
            s.set_params(&pr);
            s
        };

        let mut r = init.clone();
        let mut discovered: Vec<PredicateExpr> = Vec::new();
        let mut stopped: Option<RefineGap> = None;
        for _ in 0..max_iters.max(1) {
            // If `bad` is one-step reachable from R, the invariant fails — stop. That is the
            // discovery's own end; z3 giving no answer is not.
            {
                let s = mk();
                s.assert(Bool::and(&[&r, &transition, &bad_s1]));
                match s.check() {
                    z3::SatResult::Unsat => {}
                    z3::SatResult::Sat => break,
                    z3::SatResult::Unknown => {
                        stopped = Some(RefineGap::z3_stopped("reachability", timeout_ms));
                        break;
                    }
                }
            }
            // Interpolate A = R ∧ T  vs  B = ¬bad(s1).  I over s1 (register names).
            let a = Bool::and(&[&r, &transition]);
            let b = bad_s1.not();
            let (a_decls, a_body) = match serialize_term(&a) {
                Some(x) => x,
                None => {
                    stopped = Some(RefineGap::not_serialized("A"));
                    break;
                }
            };
            let (b_decls, b_body) = match serialize_term(&b) {
                Some(x) => x,
                None => {
                    stopped = Some(RefineGap::not_serialized("B"));
                    break;
                }
            };
            let mut decls: BTreeSet<String> = BTreeSet::new();
            decls.extend(a_decls);
            decls.extend(b_decls);
            let mut query =
                String::from("(set-logic QF_BV)\n(set-option :produce-interpolants true)\n");
            for d in &decls {
                query.push_str(d);
                query.push('\n');
            }
            query.push_str(&format!(
                "(assert {a_body})\n(get-interpolant I {b_body})\n(exit)\n"
            ));
            let stdout = match run_cvc5_raw(&query, timeout_ms) {
                Ok(s) => s,
                Err(e) => {
                    stopped = Some(RefineGap::of_query(e));
                    break;
                }
            };
            let p = match parse_interpolant_to_predicate_expr(&stdout) {
                Some(p) => p,
                None => {
                    // The grammar ceiling, or no interpolant: cvc5 answered either way.
                    stopped = Some(RefineGap::of_unusable_cvc5_reply(&stdout));
                    break;
                }
            };
            // Grow R by the new predicate; stop at a fixpoint (nothing new).
            let grown = match over_s0(&p) {
                Some(pb) => pb,
                None => {
                    stopped = Some(RefineGap::new(
                        GapKind::NotPosed,
                        "an interpolant names a register the next query cannot address",
                    ));
                    break;
                }
            };
            let is_new = !discovered.contains(&p);
            if is_new {
                discovered.push(p);
            }
            let new_r = Bool::or(&[&r, &grown]);
            // Fixpoint: R didn't grow (new_r ⟹ r) AND no new predicate.
            let fixed = {
                let s = mk();
                s.assert(Bool::and(&[&new_r, &r.not()]));
                s.check() == z3::SatResult::Unsat
            };
            r = new_r;
            if fixed && !is_new {
                break;
            }
        }
        Discovery {
            predicates: discovered,
            stopped,
        }
    })
}

/// What [`sequence_interpolant_predicates`] gave: the predicates, and the gap of every cut
/// that gave none (mununu#1188). With no predicate, [`telling_gap`] over `gaps` says whether
/// the sequence met the grammar or was cut short.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SequencePredicates {
    pub predicates: Vec<PredicateExpr>,
    pub gaps: Vec<RefineGap>,
}

/// P2.3b — the SEQUENCE of predicates from interpolating the depth-`depth` BMC
/// unrolling at every cut `1..=depth` (the targeted per-trace refinement IC3ia
/// needs). Returns the parseable, deduplicated interpolant predicates — a SET that
/// collectively rules out the spurious depth-`depth` counterexample, rather than
/// the single (often redundant) predicate a one-cut interpolation gives. No predicate
/// when `bad` is reachable within `depth` (a real CEX — the caller confirms elsewhere:
/// no gap either), when every interpolant is outside the `#318` grammar, or when the
/// queries were not answered — the gaps say which.
pub fn sequence_interpolant_predicates(
    file: &Btor2File,
    depth: usize,
    timeout_ms: u32,
) -> SequencePredicates {
    let depth = depth.clamp(1, 24);
    let mut out = SequencePredicates::default();
    for cut in 1..=depth {
        match interpolant_at_cut(file, cut, depth, timeout_ms) {
            RefineOutcome::Predicate(p) => {
                if !out.predicates.contains(&p) {
                    out.predicates.push(p);
                }
            }
            RefineOutcome::Realizable => {}
            RefineOutcome::Unavailable(gap) => out.gaps.push(gap),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::parser;
    use crate::adapter::btor2::predicate_expr::CmpOp;

    // 2-bit `x` counter: init 0, next x = x + 1. No `bad` needed — refinement is
    // about a source→target STEP, not reachability.
    const COUNTER: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n\
                           5 one 1\n6 add 1 3 5\n7 next 1 3 6\n";

    fn cmp(reg: &str, value: u64) -> PredicateExpr {
        PredicateExpr::Cmp {
            register: reg.to_string(),
            op: CmpOp::Eq,
            value,
        }
    }

    /// A spurious step (`x==0 → x==2` is impossible: `0+1=1≠2`) must yield a
    /// separating predicate over `x`.
    #[test]
    fn spurious_step_yields_separating_predicate() {
        let file = parser::parse(COUNTER).expect("parse");
        match synthesize_refinement_predicate(&file, &[cmp("x", 0)], &[cmp("x", 2)], 5_000) {
            RefineOutcome::Predicate(pe) => {
                let s = format!("{pe:?}");
                assert!(s.contains('x'), "predicate should mention x: {s}");
            }
            RefineOutcome::Realizable => panic!("x==0 → x==2 is NOT realizable (0+1=1)"),
            RefineOutcome::Unavailable(w) => eprintln!("SKIP (cvc5/grammar): {w}"),
        }
    }

    /// P2.3 — path interpolation synthesises a *reachability* predicate: `a` holds at
    /// 0, `b = a+1`, `bad = (b==2)`. `bad` is not reachable, so the multi-step suffix
    /// interpolant over-approximates the reachable states — a predicate single-step
    /// refinement cannot produce.
    #[test]
    fn path_interpolation_yields_reachability_predicate() {
        const D: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 a\n4 init 1 3 2\n5 next 1 3 3\n\
                         6 state 1 b\n7 init 1 6 2\n8 one 1\n9 add 1 3 8\n10 next 1 6 9\n\
                         11 constd 1 2\n12 sort bitvec 1\n13 eq 12 6 11\n14 bad 13\n";
        let file = parser::parse(D).expect("parse");
        match synthesize_reachability_predicate(&file, 4, 5_000) {
            RefineOutcome::Predicate(pe) => {
                // A genuine over-approximation of the reachable states.
                assert!(!format!("{pe:?}").is_empty());
            }
            // Grammar ceiling (interpolant not parseable) or cvc5 absent — a sound
            // fall-through, not a failure.
            RefineOutcome::Unavailable(w) => eprintln!("SKIP (grammar/cvc5): {w}"),
            RefineOutcome::Realizable => {
                panic!("bad (b==2) is NOT reachable — cannot be Realizable")
            }
        }
    }

    /// Emergent-K: the iterative forward interpolation DISCOVERS the relational
    /// invariant `data == target` — a predicate with NO syntactic `eq` node in the
    /// design (the invariant is only implied by the two counters incrementing
    /// together). This is the emergent-K payoff: a relation no syntactic extractor
    /// finds, synthesised from interpolation.
    #[test]
    fn discovers_emergent_relational_invariant() {
        // data, target: 8-bit, both init 0, both += 1 ⇒ data==target invariant.
        // bad = (data != target) — never holds. NO `eq` node anywhere.
        const D: &str = "1 sort bitvec 8\n2 sort bitvec 1\n3 zero 1\n4 one 1\n\
                         5 state 1 data\n6 init 1 5 3\n7 state 1 target\n8 init 1 7 3\n\
                         9 add 1 5 4\n10 next 1 5 9\n11 add 1 7 4\n12 next 1 7 11\n\
                         13 neq 2 5 7\n14 bad 13\n";
        let file = parser::parse(D).expect("parse");
        let preds = discover_relational_predicates(&file, 8, 5_000).predicates;
        if preds.is_empty() {
            eprintln!("SKIP (cvc5 absent / grammar): no predicates discovered");
            return;
        }
        // A relational `data == target` (CmpReg) must be among the discovered preds.
        let found_relation = preds.iter().any(|p| {
            matches!(p, PredicateExpr::CmpReg { lhs, op, rhs }
                if *op == CmpOp::Eq
                    && ((lhs == "data" && rhs == "target") || (lhs == "target" && rhs == "data")))
        });
        assert!(
            found_relation,
            "expected to discover the emergent relation data==target; got {preds:?}"
        );
    }

    /// Emergent-K on an INEQUALITY bound: a saturating counter (`cnt' = cnt>=5 ? 5 :
    /// cnt+1`) has the invariant `cnt <= 5`. No `eq` node; the *syntactic* extractors
    /// are equality-only, so this bound is exactly the kind of invariant interpolation
    /// can uniquely supply. Expect the discovery to find `cnt <= 5` (`Cmp{Le}`).
    #[test]
    fn discovers_inequality_bound_invariant() {
        const D: &str = "1 sort bitvec 8\n2 sort bitvec 1\n3 constd 1 5\n4 one 1\n5 zero 1\n\
                         6 state 1 cnt\n7 init 1 6 5\n8 ugte 2 6 3\n9 add 1 6 4\n\
                         10 ite 1 8 3 9\n11 next 1 6 10\n12 ugt 2 6 3\n13 bad 12\n";
        let file = parser::parse(D).expect("parse");
        let preds = discover_relational_predicates(&file, 8, 5_000).predicates;
        if preds.is_empty() {
            // mununu#981: under MUNUNU_REQUIRE_TOOLS=1 (cvc5 is in the image) an empty discovery
            // fails instead of skipping.
            crate::adapter::require_tool("cvc5 (an interpolant for cnt <= 5)", false);
            eprintln!("SKIP (cvc5 absent / grammar): none discovered");
            return;
        }
        // `cnt <= 5` in either spelling: cvc5 returns the interpolant as `¬(cnt > 5)` (measured
        // in `mununu-sva`, mununu#981, cvc5 1.0.1-dev), the same predicate over bit-vectors — and
        // the same split of the state space for the abstraction.
        let found_bound = preds.iter().any(|p| match p {
            PredicateExpr::Cmp {
                register,
                op,
                value,
            } => register == "cnt" && matches!(op, CmpOp::Le | CmpOp::Lt) && *value >= 5,
            PredicateExpr::Not(inner) => {
                matches!(&**inner, PredicateExpr::Cmp { register, op, value }
                if register == "cnt" && matches!(op, CmpOp::Gt | CmpOp::Ge) && *value >= 5)
            }
            _ => false,
        });
        assert!(
            found_bound,
            "expected to discover the bound invariant cnt<=5; got {preds:?}"
        );
    }

    #[test]
    fn mathsat_interpolant_parse_extracts_ite_condition_and_bare_comparison() {
        use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};
        // The WP form MathSAT emits for the must-precondition query — the `ite` CONDITION is the
        // relational invariant `data == target`; the sat-status line is skipped.
        let out = "unsat\n(= (_ bv0 1) (ite (= data target) (_ bv0 1) busy))\n";
        match parse_mathsat_interpolant(out) {
            RefineOutcome::Predicate(PredicateExpr::CmpReg { lhs, op, rhs }) => {
                assert_eq!(op, CmpOp::Eq);
                assert!(
                    (lhs == "data" && rhs == "target") || (lhs == "target" && rhs == "data"),
                    "extracted the wrong relation: {lhs} {op:?} {rhs}"
                );
            }
            other => panic!("expected CmpReg data==target from the ite condition; got {other:?}"),
        }
        // A bare comparison (no ite) parses directly.
        assert!(matches!(
            parse_mathsat_interpolant("unsat\n(bvule cnt (_ bv5 8))\n"),
            RefineOutcome::Predicate(PredicateExpr::Cmp { op: CmpOp::Le, .. })
        ));
        // No interpolant (SAT / empty) → Unavailable, never a fabricated predicate: MathSAT
        // answered, so the gap is the interpolant's, not a stop.
        assert!(matches!(
            parse_mathsat_interpolant("sat\n"),
            RefineOutcome::Unavailable(gap) if gap.kind == GapKind::NoInterpolant
        ));
        assert!(matches!(
            parse_mathsat_interpolant("unsat\n(bvslt cnt (_ bv5 8))\n"),
            RefineOutcome::Unavailable(gap) if gap.kind == GapKind::OutsideGrammar
        ));
        // `ite_condition` peels the condition; a term with no ite returns None.
        assert_eq!(
            ite_condition("(= (_ bv0 1) (ite (= data target) (_ bv0 1) busy))").as_deref(),
            Some("(= data target)")
        );
        assert_eq!(ite_condition("(= data target)"), None);
    }

    /// Combinational-`good` resolution — the corpus recoverability targets a COMBINATIONAL output
    /// (`cs_n==15`), not a state cell. Here `idle = !busy` is a named combinational node; the
    /// must-precondition of `good = (idle==1)` must resolve `idle` via `signal_bvs` (⟺ `busy==0`)
    /// and still discover `data==target` — the same invariant as the state-register case. `#[ignore]`d
    /// (needs MathSAT; run in `mununu-sva` with `MUNUNU_MATHSAT_PATH`).
    #[test]
    #[ignore = "manual: requires MathSAT, which the mununu-sva image lacks; run with --ignored and MUNUNU_MATHSAT_PATH=<binary>"]
    fn must_precondition_resolves_combinational_good() {
        // MathSAT is not in the `mununu-sva` image (it ships in `mununu-sva-pono`).
        // Skip rather than fail: an absent optional tool is not a regression, and a
        // red result here masks the genuine failures in the `--ignored` sweep.
        if crate::adapter::btor2::native_interp::mathsat_available().is_err() {
            eprintln!(
                "SKIPPED: MathSAT unavailable — run in `mununu-sva-pono` or set \
                 MUNUNU_MATHSAT_PATH."
            );
            return;
        }
        use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};
        const D: &str = "1 sort bitvec 1\n2 sort bitvec 3\n3 state 1 busy\n4 state 2 data\n\
5 state 2 target\n6 one 1\n7 zero 1\n8 zero 2\n9 one 2\n10 init 1 3 6\n11 init 2 4 8\n\
12 init 2 5 8\n13 add 2 4 9\n14 next 2 4 13\n15 add 2 5 9\n16 next 2 5 15\n17 eq 1 4 5\n\
18 ite 1 17 7 3\n19 next 1 3 18\n20 not 1 3 idle\n";
        let file = crate::adapter::btor2::parser::parse(D).expect("parse");
        // ⊥ cube = {busy != 0} (state); good = {idle == 1} (COMBINATIONAL, ⟺ busy==0).
        let source = vec![PredicateExpr::Cmp {
            register: "busy".into(),
            op: CmpOp::Ne,
            value: 0,
        }];
        let good = vec![PredicateExpr::Cmp {
            register: "idle".into(),
            op: CmpOp::Eq,
            value: 1,
        }];
        match must_precondition_interpolant(&file, &source, &good, 5_000) {
            RefineOutcome::Predicate(PredicateExpr::CmpReg { lhs, op, rhs }) => {
                assert_eq!(op, CmpOp::Eq);
                assert!(
                    (lhs == "data" && rhs == "target") || (lhs == "target" && rhs == "data"),
                    "combinational-good must-pre discovered the wrong relation: {lhs} {op:?} {rhs}"
                );
            }
            other => panic!(
                "expected data==target via combinational-good (`idle`) resolution; got {other:?}"
            ),
        }
    }

    /// Validation sweep: run the interpolation invariant-discovery over every
    /// `*.btor2` in `MUNUNU_DISCOVER_DIR` (real HWMCC / OpenTitan designs) and print
    /// the discovered predicates per design — how often does it find a non-trivial
    /// relational / bound invariant on REAL designs? `#[ignore]`d.
    #[test]
    #[ignore = "manual: sweeps an external BTOR2 dir named by MUNUNU_DISCOVER_DIR"]
    fn sweep_discovery_on_external_designs() {
        let Ok(dir) = std::env::var("MUNUNU_DISCOVER_DIR") else {
            eprintln!("SKIPPED: opt-in sweep — set MUNUNU_DISCOVER_DIR to run it.");
            return;
        };
        let max_bytes: u64 = std::env::var("MUNUNU_DISCOVER_MAXBYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20_000);
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "btor2"))
            .filter(|p| {
                std::fs::metadata(p)
                    .map(|m| m.len() <= max_bytes)
                    .unwrap_or(false)
            })
            .collect();
        files.sort();
        let mut found = 0usize;
        let mut relational = 0usize;
        for p in &files {
            let Ok(content) = std::fs::read_to_string(p) else {
                continue;
            };
            let Ok(file) = parser::parse(&content) else {
                continue;
            };
            let preds = discover_relational_predicates(&file, 8, 4_000).predicates;
            if preds.is_empty() {
                continue;
            }
            found += 1;
            let rel = preds.iter().any(|pe| {
                matches!(
                    pe,
                    PredicateExpr::CmpReg { .. } | PredicateExpr::CmpRegAddend { .. }
                ) || matches!(pe, PredicateExpr::Cmp { op, .. } if !matches!(op, CmpOp::Eq | CmpOp::Ne))
            });
            if rel {
                relational += 1;
            }
            let name = p.file_name().unwrap().to_string_lossy();
            eprintln!("  {name}: {preds:?}");
        }
        eprintln!(
            "\nSWEEP: {} designs (<= {}B); discovered non-trivial on {}, of which {} had a \
             RELATIONAL/BOUND invariant (the interpolation-unique forms)",
            files.len(),
            max_bytes,
            found,
            relational
        );
    }

    /// A realizable step (`x==0 → x==1`: `0+1=1`) must report `Realizable`, never a
    /// spurious predicate.
    #[test]
    fn realizable_step_needs_no_refinement() {
        let file = parser::parse(COUNTER).expect("parse");
        match synthesize_refinement_predicate(&file, &[cmp("x", 0)], &[cmp("x", 1)], 5_000) {
            RefineOutcome::Realizable => {}
            RefineOutcome::Predicate(pe) => panic!("x==0 → x==1 IS realizable, got {pe:?}"),
            RefineOutcome::Unavailable(w) => eprintln!("SKIP: {w}"),
        }
    }

    /// ENUMERATING (mununu#1188) — a step is cut short exactly by the gaps whose query the
    /// solver did not answer: those have words ([`RefineGap::cut_short`]) and are what
    /// [`telling_gap`] returns; an answered query has neither. The words carry the caller's
    /// step and the gap's fact. Exhaustive over [`GapKind::ALL`].
    #[test]
    fn x1188_a_step_is_cut_short_exactly_by_the_gaps_the_solver_did_not_answer() {
        for kind in GapKind::ALL {
            let gap = RefineGap::new(kind, "the fact");
            let said = gap.cut_short("the step", Some("the other engine"));
            assert_eq!(said.is_none(), kind.solver_answered(), "{kind:?}: {said:?}");
            assert_eq!(
                telling_gap(std::slice::from_ref(&gap)).is_none(),
                kind.solver_answered(),
                "{kind:?}"
            );
            if let Some(text) = said {
                assert!(
                    text.contains("the step") && text.contains("the fact"),
                    "{kind:?}: {text}"
                );
            }
        }
    }

    /// mununu#1188 — which gap a step that collected several is reported by: a cancelled run,
    /// then a missing solver, a stop at the limit, a failure, a query never posed; in either
    /// order of arrival, with answered queries among them; the first of two of one kind; and
    /// none when every query was answered or none was asked.
    #[test]
    fn x1188_the_telling_gap_is_the_highest_ranked_unanswered_one_in_either_order() {
        const ORDER: [GapKind; 5] = [
            GapKind::Cancelled,
            GapKind::SolverMissing,
            GapKind::SolverStopped,
            GapKind::SolverFailed,
            GapKind::NotPosed,
        ];
        for kind in GapKind::ALL {
            assert_eq!(
                ORDER.contains(&kind),
                !kind.solver_answered(),
                "{kind:?}: every unanswered kind has a rank, and no answered one"
            );
        }
        let gap = |kind: GapKind| RefineGap::new(kind, format!("{kind:?}"));
        assert_eq!(telling_gap(&[]), None);
        assert_eq!(
            telling_gap(&[gap(GapKind::NoInterpolant), gap(GapKind::OutsideGrammar)]),
            None
        );
        for (i, high) in ORDER.iter().enumerate() {
            for low in &ORDER[i + 1..] {
                for gaps in [
                    [gap(*high), gap(GapKind::NoInterpolant), gap(*low)],
                    [gap(*low), gap(GapKind::OutsideGrammar), gap(*high)],
                ] {
                    assert_eq!(
                        telling_gap(&gaps).map(|g| g.kind),
                        Some(*high),
                        "{high:?} over {low:?}"
                    );
                }
            }
        }
        let first = RefineGap::new(GapKind::SolverStopped, "the first query");
        let second = RefineGap::new(GapKind::SolverStopped, "the second query");
        assert_eq!(telling_gap(&[first.clone(), second]), Some(&first));
    }

    /// SEAM (mununu#1188) — every way cvc5 ends a refinement query, read where the refinement
    /// reads it: the same spurious step (`x == 0 → x == 2` on the counter) with a stand-in
    /// cvc5 per row. A stop at the limit, a missing binary and a failure are three kinds, and
    /// none of them is the answered kind a reply the grammar does not take leaves. z3 decides
    /// the step is spurious for real, so each row reaches the interpolation query.
    #[test]
    fn x1188_every_way_cvc5_ends_a_refinement_query_has_its_kind() {
        use crate::adapter::btor2::native_interp::stand_in_solver::{StandIn, stand_in};
        let file = parser::parse(COUNTER).expect("parse");
        let rows: [(&str, Option<StandIn>, GapKind); 4] = [
            (
                "exits the way cvc5 does at its time limit",
                Some(StandIn::Exits {
                    code: 134,
                    stderr: "cvc5 interrupted by timeout.",
                }),
                GapKind::SolverStopped,
            ),
            (
                "exits with a failure",
                Some(StandIn::Exits {
                    code: 3,
                    stderr: "boom",
                }),
                GapKind::SolverFailed,
            ),
            ("is not there", None, GapKind::SolverMissing),
            (
                "replies with something that is no interpolant the grammar takes",
                Some(StandIn::Replies { bytes: 4 }),
                GapKind::OutsideGrammar,
            ),
        ];
        for (row, what, want) in rows {
            let dir = tempfile::tempdir().expect("tempdir");
            let cvc5 = match what {
                Some(what) => stand_in(dir.path(), what),
                None => dir.path().join("no-such-solver"),
            };
            let _env = crate::adapter::env_var::override_for_test("MUNUNU_CVC5_PATH", &cvc5);
            match synthesize_refinement_predicate(&file, &[cmp("x", 0)], &[cmp("x", 2)], 5_000) {
                RefineOutcome::Unavailable(gap) => {
                    assert_eq!(gap.kind, want, "a cvc5 that {row}: {gap}")
                }
                other => panic!("a cvc5 that {row} gives no predicate: {other:?}"),
            }
        }
    }

    /// mununu#1188 — the discovery says why it stopped: with a cvc5 that stops at the query
    /// limit it has no predicate and carries the stop, where it used to return an empty list
    /// that read as "nothing to find". `a` holds at 0, `b = a + 1`, `bad = (b == 2)`: `bad` is
    /// not one step from the initial states, so the first interpolation query is asked.
    #[test]
    fn x1188_a_discovery_cut_short_at_the_query_limit_carries_the_stop() {
        use crate::adapter::btor2::native_interp::stand_in_solver::{StandIn, stand_in};
        const D: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 a\n4 init 1 3 2\n5 next 1 3 3\n\
                         6 state 1 b\n7 init 1 6 2\n8 one 1\n9 add 1 3 8\n10 next 1 6 9\n\
                         11 constd 1 2\n12 sort bitvec 1\n13 eq 12 6 11\n14 bad 13\n";
        let file = parser::parse(D).expect("parse");
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = stand_in(
            dir.path(),
            StandIn::Exits {
                code: 134,
                stderr: "cvc5 interrupted by timeout.",
            },
        );
        let _env = crate::adapter::env_var::override_for_test("MUNUNU_CVC5_PATH", &fake);
        let found = discover_relational_predicates(&file, 8, 4_000);
        assert!(found.predicates.is_empty(), "{found:?}");
        assert_eq!(
            found.stopped.as_ref().map(|gap| gap.kind),
            Some(GapKind::SolverStopped),
            "{found:?}"
        );
    }
}
