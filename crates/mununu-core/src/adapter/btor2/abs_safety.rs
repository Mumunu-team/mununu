//! Phase 2 · P2.2 — the safety driver of the shared lazy-refinement core (plan
//! `cube-ic3ia-invariant-discovery.md` §9).
//!
//! Decides `AG ¬bad` by **predicate-abstraction forward reachability with
//! interpolation refinement**, over the shared core: a predicate set `P` induces a
//! finite abstraction (cubes = predicate valuations); the forward may-reachable
//! cube set over-approximates the reachable states; if it excludes every `bad`
//! cube, the design is safe. When the abstraction is too coarse (a `bad` cube is
//! may-reachable but no concrete counterexample exists), the shared refinement
//! primitive ([`super::refine::synthesize_refinement_predicate`]) — and, for
//! reachability spuriousness, the interpolation engine
//! ([`super::native_interp`]) — synthesise a new predicate and the pass restarts.
//!
//! **Soundness is structural, not search-dependent.** Every verdict is
//! independently re-verified before it is returned. `Safe` only when the
//! may-reachable set `R#` is a *checked* inductive invariant over the EXACT
//! transition — `Init ⟹ R#`, `R# ∧ T ⟹ R#'`, `R# ⟹ ¬bad` — so a bug in the
//! abstraction/BFS can at worst make it abstain, never wrongly prove. `Unsafe`
//! only when [`super::native_bmc::bmc_bad_reachable`] exhibits a concrete
//! counterexample. Everything else is `Unknown` — the driver never guesses.
//!
//! The IC3-frame incremental optimisation (block CTIs without enumerating the
//! reachable cube set — the scalability half of §9's safety driver) is **P2.2b**;
//! this first increment establishes the sound refinement-driven skeleton the frame
//! ladder will slot into.

use std::collections::{BTreeSet, HashMap, VecDeque};

use z3::ast::{Ast, BV, Bool};

use crate::adapter::btor2::ast::{Btor2File, ConstValue, Nid, Node};
use crate::adapter::btor2::native_bmc::{BmcOutcome, bmc_bad_reachable};
use crate::adapter::btor2::predicate_expr::{CmpOp, PredicateExpr};
use crate::adapter::btor2::refine::{
    GapKind, RefineGap, RefineOutcome, sequence_interpolant_predicates,
    synthesize_refinement_predicate, telling_gap,
};
use crate::adapter::owner::{mununu_defect, mununu_limitation, technique_limit};
use crate::adapter::sidecar::predicate_image::btor2_encode::{SignalKind, encode_design};

/// mununu#1121 U2.10 — the engines that decide the same `bad` without this pass; this pass runs
/// under `btor2 verify --engine ic3` only (`recoverability::verify_safety_with_engine`).
const OTHER_ENGINES: &str = "the default portfolio (`--engine portfolio` on `btor2 verify`) decides the same property \
     with its SAT and IC3 members";

/// The per-query solver budget is fixed by the caller of this pass (no verb exposes it), so a
/// timeout is a mununu limitation with the other engines as the way out, not a budget the user
/// can raise.
fn solver_timeout(timeout_ms: u32, query: &str) -> String {
    mununu_limitation(
        &format!(
            "z3 hit its {timeout_ms} ms per-query budget on the {query} query; the budget is \
             fixed on this engine"
        ),
        Some(OTHER_ENGINES),
    )
}

/// The words a report carries only when every query of the refinement step was answered: the
/// step met the interpolants themselves, and none is a predicate the abstraction can take.
pub(crate) const GRAMMAR_CEILING: &str = "grammar ceiling";

/// How a refinement step that gave the abstraction nothing new ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarrenStep {
    /// No query gave a predicate.
    NoPredicate,
    /// The queries gave predicates, and the abstraction already had every one.
    NothingNew,
}

/// What this pass reports when a refinement step gave nothing new, and the kind of the gap
/// that cut the step short when one did (mununu#1188).
///
/// Before the gaps were typed, both sites said "grammar ceiling" whatever had happened — for a
/// cvc5 that stopped at its query limit as for one that answered. The ceiling is a statement
/// about the interpolants, so it is made only when [`telling_gap`] finds no unanswered query.
/// Otherwise the report names that gap, in the words that fit its kind: the query limit is
/// fixed by this pass's caller (no verb exposes it), so a stop at it is a mununu limitation
/// with the other engines as the way out, never a budget the user is told to raise.
fn barren_refinement(step: BarrenStep, gaps: &[RefineGap]) -> (String, Option<GapKind>) {
    let Some(gap) = telling_gap(gaps) else {
        let what = match step {
            BarrenStep::NoPredicate => format!(
                "the spurious abstract counterexample could not be ruled out: every \
                 interpolation query was answered, and no interpolant is a predicate the \
                 abstraction takes (the {GRAMMAR_CEILING})"
            ),
            BarrenStep::NothingNew => format!(
                "the refinement stalled: every interpolation query was answered, and the \
                 abstraction already has each predicate they gave (the {GRAMMAR_CEILING})"
            ),
        };
        return (technique_limit(&what, OTHER_ENGINES), None);
    };
    let cut_short = match step {
        BarrenStep::NoPredicate => {
            "the interpolation query that would rule out the spurious abstract counterexample"
        }
        BarrenStep::NothingNew => {
            "the refinement gave no predicate the abstraction lacks, and one of its \
             interpolation queries"
        }
    };
    match gap.cut_short(cut_short, Some(OTHER_ENGINES)) {
        Some(text) => (text, Some(gap.kind)),
        // `telling_gap` returns only gaps of queries the solver did not answer, and those are
        // the kinds `cut_short` has words for.
        None => (
            mununu_defect(&format!(
                "an answered refinement query was read as an unanswered one: {}",
                gap.what
            )),
            None,
        ),
    }
}

/// A `sat` answer without a model: the solver contradicted itself, or we misread it.
fn sat_without_model(query: &str) -> String {
    mununu_defect(&format!(
        "z3 answered sat on the {query} query but returned no model"
    ))
}

/// Our own candidate invariant failed the direct check that makes `Safe` sound; the verdict is
/// withheld.
fn invariant_check_failed(which: &str) -> String {
    mununu_defect(&format!(
        "the abstract invariant this pass computed failed its direct check ({which}); the \
         verdict is withheld"
    ))
}

/// Verdict of the predicate-abstraction safety driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbsVerdict {
    /// `AG ¬bad` — the may-reachable abstraction is a *verified* inductive invariant
    /// excluding `bad`. `predicates` is how many were in `P` at convergence.
    Safe { predicates: usize },
    /// `bad` is reachable — `depth` from `bmc_bad_reachable`.
    Unsafe { depth: u32 },
    /// Abstained: refinement stalled (a needed predicate is outside the parseable
    /// grammar), the abstraction blew up, cvc5 was absent, or a solver timed out.
    ///
    /// `refinement` is set when a refinement step that was CUT SHORT is why (mununu#1188):
    /// the kind of the query that was not answered — a solver stopped at its limit, absent,
    /// failed, cancelled, or a query that could not be posed. `None` on every other
    /// abstention, the grammar ceiling included: there the queries were answered.
    Unknown {
        reason: String,
        refinement: Option<GapKind>,
    },
}

impl AbsVerdict {
    /// An abstention that no cut-short refinement explains.
    fn unknown(reason: impl Into<String>) -> Self {
        AbsVerdict::Unknown {
            reason: reason.into(),
            refinement: None,
        }
    }

    /// The abstention of a refinement step that gave nothing new.
    fn barren(step: BarrenStep, gaps: &[RefineGap]) -> Self {
        let (reason, refinement) = barren_refinement(step, gaps);
        AbsVerdict::Unknown { reason, refinement }
    }
}

/// The predicates a refinement step gave, and the gap of each of its queries that gave none.
struct Refinement {
    predicates: Vec<PredicateExpr>,
    gaps: Vec<RefineGap>,
}

/// A cube = a full valuation of the predicate set (indexed by predicate position).
type Cube = Vec<bool>;

/// P2.2 — decide `AG ¬bad` by predicate-abstraction reachability + refinement.
/// `max_refine` bounds the refinement loop; `max_cubes` caps the reachable-set
/// enumeration (abstain if exceeded); `timeout_ms` bounds each solver query.
pub fn verify_safety_abs(
    file: &Btor2File,
    max_refine: u32,
    max_cubes: usize,
    timeout_ms: u32,
) -> AbsVerdict {
    // Seed predicates from the `bad` cone's comparison atoms + the reset values of
    // the named state cells — the cheap, always-available starting abstraction.
    let mut preds = seed_predicates(file);

    for _round in 0..max_refine {
        match one_pass(file, &preds, max_cubes, timeout_ms) {
            PassResult::Safe => {
                return AbsVerdict::Safe {
                    predicates: preds.len(),
                };
            }
            PassResult::Unsafe { depth } => return AbsVerdict::Unsafe { depth },
            PassResult::Refine(p) => {
                if preds.iter().any(|q| q == &p) {
                    return AbsVerdict::unknown(
                        "refinement produced an already-present predicate (stalled)",
                    );
                }
                preds.push(p);
            }
            PassResult::NoRefinement(gaps) => {
                let (reason, refinement) = no_single_step_refinement(&gaps);
                return AbsVerdict::Unknown { reason, refinement };
            }
            PassResult::Unknown(reason) => return AbsVerdict::unknown(reason),
        }
    }
    AbsVerdict::unknown(format!("no fixpoint within {max_refine} refinements"))
}

enum PassResult {
    Safe,
    Unsafe {
        depth: u32,
    },
    Refine(PredicateExpr),
    /// No edge of the spurious path gave a predicate; the gaps of the edges whose query gave
    /// none (an edge that is realizable leaves no gap).
    NoRefinement(Vec<RefineGap>),
    Unknown(String),
}

/// What [`verify_safety_abs`] reports when no edge of the spurious path refined. With every
/// query answered that is this pass's own limit (it has no path interpolation); with one cut
/// short, the path may well have had a refinable edge, and the report says what cut it short.
fn no_single_step_refinement(gaps: &[RefineGap]) -> (String, Option<GapKind>) {
    if telling_gap(gaps).is_some() {
        return barren_refinement(BarrenStep::NoPredicate, gaps);
    }
    (
        mununu_limitation(
            "the spurious abstract counterexample has no single-step-refinable edge; ruling it \
             out needs a path-interpolation refinement this pass does not have",
            Some(OTHER_ENGINES),
        ),
        None,
    )
}

/// One abstraction pass at a fixed predicate set: build the may-reachable cube set;
/// if it excludes `bad` → verify inductive → Safe; else classify the abstract
/// counterexample (real → Unsafe; spurious → a refinement predicate).
fn one_pass(
    file: &Btor2File,
    preds: &[PredicateExpr],
    max_cubes: usize,
    timeout_ms: u32,
) -> PassResult {
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = match encode_design(file) {
            Ok(v) => v,
            Err(e) => return PassResult::Unknown(format!("encode: {e}")),
        };
        // Named-state interface (cur / nx) + transition, mirroring `refine`.
        let name_to_nid: HashMap<String, Nid> = view
            .signals
            .iter()
            .filter(|s| s.kind == SignalKind::State)
            .filter_map(|s| s.symbol.clone().map(|sym| (sym, s.nid)))
            .collect();
        let nid_to_name: HashMap<Nid, String> =
            name_to_nid.iter().map(|(n, &d)| (d, n.clone())).collect();
        let named = |suffix: &str| -> HashMap<Nid, BV> {
            view.state_curr
                .iter()
                .filter_map(|(nid, bv)| {
                    let sym = nid_to_name.get(nid)?;
                    Some((*nid, BV::new_const(format!("{sym}{suffix}"), bv.get_size())))
                })
                .collect()
        };
        let cur = named("__c");
        let nx = named("__n");
        let inp: HashMap<Nid, BV> = view
            .inputs
            .iter()
            .map(|(nid, bv)| (*nid, BV::new_const(format!("i{nid}__x"), bv.get_size())))
            .collect();
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

        // Predicate z3 Bools over cur and nx.
        let build_over = |frame: &HashMap<Nid, BV>, p: &PredicateExpr| -> Option<Bool> {
            let lookup = |name: &str| {
                name_to_nid
                    .get(name)
                    .and_then(|nid| frame.get(nid))
                    .cloned()
            };
            p.build_constraint(&lookup)
        };
        let (pred_cur, pred_nx): (Vec<Bool>, Vec<Bool>) = {
            let mut pc = Vec::new();
            let mut pn = Vec::new();
            for p in preds {
                match (build_over(&cur, p), build_over(&nx, p)) {
                    (Some(c), Some(n)) => {
                        pc.push(c);
                        pn.push(n);
                    }
                    _ => {
                        return PassResult::Unknown(PredicateExpr::unknown_register_reason(
                            "predicate references unknown register",
                            preds,
                            name_to_nid.keys().map(String::as_str),
                        ));
                    }
                }
            }
            (pc, pn)
        };

        // Init / bad over cur (+ bad over nx).
        let props = extract_props(file);
        let init_cur = init_bool(&view, &props, &cur, &nid_to_name);
        let bad_cur = bad_bool(&view, &props, &cur);

        let mk_solver = || {
            let s = z3::Solver::new();
            let mut pr = z3::Params::new();
            pr.set_u32("timeout", timeout_ms);
            s.set_params(&pr);
            s
        };
        let cube_bool = |c: &Cube, frame_preds: &[Bool]| -> Bool {
            let lits: Vec<Bool> = frame_preds
                .iter()
                .zip(c)
                .map(|(p, &pos)| if pos { p.clone() } else { p.not() })
                .collect();
            if lits.is_empty() {
                Bool::from_bool(true)
            } else {
                Bool::and(&lits.iter().collect::<Vec<_>>())
            }
        };
        // All-SAT enumeration of the predicate valuations satisfying `phi`
        // (`phi` is over `over_preds` — either cur or nx). Bounded by `max_cubes`; `Err` carries
        // the owner-naming reason (cap, solver budget, or a sat without a model).
        let all_cubes =
            |what: &str, phi: &Bool, over_preds: &[Bool]| -> Result<Vec<Cube>, String> {
                let s = mk_solver();
                s.assert(phi);
                let mut out = Vec::new();
                loop {
                    match s.check() {
                        z3::SatResult::Sat => {}
                        z3::SatResult::Unsat => break,
                        z3::SatResult::Unknown => return Err(solver_timeout(timeout_ms, what)),
                    }
                    let model = s.get_model().ok_or_else(|| sat_without_model(what))?;
                    let cube: Cube = over_preds
                        .iter()
                        .map(|p| {
                            model
                                .eval(p, true)
                                .and_then(|b| b.as_bool())
                                .unwrap_or(false)
                        })
                        .collect();
                    // block this exact valuation
                    let block = cube_bool(&cube, over_preds).not();
                    s.assert(&block);
                    out.push(cube);
                    if out.len() > max_cubes {
                        return Err(technique_limit(
                            &format!(
                                "the abstract {what} enumeration exceeded its cap of {max_cubes} \
                             cubes (the predicate abstraction enumerates cubes explicitly)"
                            ),
                            OTHER_ENGINES,
                        ));
                    }
                }
                Ok(out)
            };

        // Reset-initial cubes (predicate valuations consistent with Init).
        let init_cubes = match all_cubes("init-cube", &init_cur, &pred_cur) {
            Ok(v) => v,
            Err(reason) => return PassResult::Unknown(reason),
        };

        // Forward BFS of may-reachable cubes; record a parent for path recovery.
        let mut reachable: BTreeSet<Cube> = init_cubes.iter().cloned().collect();
        let mut parent: HashMap<Cube, Cube> = HashMap::new();
        let mut queue: VecDeque<Cube> = init_cubes.into_iter().collect();
        while let Some(c) = queue.pop_front() {
            // may-successors: predicate valuations of `cube(c) ∧ T` at next-state.
            let src = Bool::and(&[&cube_bool(&c, &pred_cur), &transition]);
            let succs = match all_cubes("post-image", &src, &pred_nx) {
                Ok(v) => v,
                Err(reason) => return PassResult::Unknown(reason),
            };
            for d in succs {
                if reachable.insert(d.clone()) {
                    parent.insert(d.clone(), c.clone());
                    queue.push_back(d);
                    if reachable.len() > max_cubes {
                        return PassResult::Unknown(technique_limit(
                            &format!(
                                "the may-reachable cube set exceeded its cap of {max_cubes} \
                                 cubes (the predicate abstraction enumerates cubes explicitly)"
                            ),
                            OTHER_ENGINES,
                        ));
                    }
                }
            }
        }

        // A reachable cube that intersects `bad`?
        let bad_reach: Option<Cube> = reachable
            .iter()
            .find(|c| {
                let s = mk_solver();
                s.assert(cube_bool(c, &pred_cur));
                s.assert(&bad_cur);
                s.check() == z3::SatResult::Sat
            })
            .cloned();

        let Some(bad_cube) = bad_reach else {
            // No bad cube may-reachable ⇒ candidate safe. VERIFY the invariant
            // R# = ⋁ reachable cube (over cur) is genuinely inductive + bad-free.
            let inv = {
                let disj: Vec<Bool> = reachable.iter().map(|c| cube_bool(c, &pred_cur)).collect();
                Bool::or(&disj.iter().collect::<Vec<_>>())
            };
            // Init ⟹ inv
            if sat_of(&mk_solver, &[&init_cur, &inv.not()]) != z3::SatResult::Unsat {
                return PassResult::Unknown(invariant_check_failed(
                    "it does not contain the initial states",
                ));
            }
            // inv ⟹ ¬bad
            if sat_of(&mk_solver, &[&inv, &bad_cur]) != z3::SatResult::Unsat {
                return PassResult::Unknown(invariant_check_failed("it intersects `bad`"));
            }
            // inv ∧ T ⟹ inv'  — need inv over nx.
            let inv_nx = {
                let disj: Vec<Bool> = reachable.iter().map(|c| cube_bool(c, &pred_nx)).collect();
                Bool::or(&disj.iter().collect::<Vec<_>>())
            };
            if sat_of(&mk_solver, &[&inv, &transition, &inv_nx.not()]) != z3::SatResult::Unsat {
                return PassResult::Unknown(invariant_check_failed(
                    "it is not closed under the transition relation",
                ));
            }
            return PassResult::Safe;
        };

        // A bad cube is may-reachable. Is there a CONCRETE counterexample? (Sound
        // Unsafe gate.) Bound the depth by the abstract path length.
        let path_len = recover_path_len(&bad_cube, &parent);
        match bmc_bad_reachable(file, (path_len + 2).max(4)) {
            Ok(BmcOutcome::Violated { depth }) => return PassResult::Unsafe { depth },
            Ok(BmcOutcome::NoCexWithin { .. }) => {} // spurious → refine
            Err(_) => {}                             // fall through to refinement
        }

        // Spurious abstract counterexample → refine. First try the shared
        // single-step primitive on a spurious edge of the abstract path; that
        // handles locally-impossible steps. (Reachability-spurious cases — where
        // every edge is realizable but the composed path is not — need the
        // interpolation engine's frame refinement; P2.2b.)
        let path = recover_path(&bad_cube, &parent);
        let mut gaps: Vec<RefineGap> = Vec::new();
        for w in path.windows(2) {
            let (src, dst) = (&w[0], &w[1]);
            let source = cube_to_predicates(src, preds);
            let target = cube_to_predicates(dst, preds);
            match synthesize_refinement_predicate(file, &source, &target, timeout_ms) {
                RefineOutcome::Predicate(p) => return PassResult::Refine(p),
                RefineOutcome::Realizable => continue,
                RefineOutcome::Unavailable(gap) => gaps.push(gap),
            }
        }
        PassResult::NoRefinement(gaps)
    })
}

/// SAT of the conjunction of `terms` under a timeout-bounded solver.
fn sat_of(mk: &dyn Fn() -> z3::Solver, terms: &[&Bool]) -> z3::SatResult {
    let s = mk();
    for &t in terms {
        s.assert(t);
    }
    s.check()
}

// ============================================================================
// P2.2b — the IC3 frame ladder (scalability half of §9's safety driver).
//
// Blocks counterexamples-to-induction (CTIs) *incrementally* over the predicate
// abstraction — NO enumeration of the reachable cube set — so it decides designs
// whose reachable abstract state space is large but whose inductive invariant is
// compact (where the `verify_safety_abs` BFS hits `max_cubes`). Queries run over
// the EXACT transition with the frames expressed as clauses over predicates
// (Cimatti–Griggio IC3ia). Refinement of a spurious CTI reuses the shared P2.1
// primitive; the deeper reachability-spurious refinement via path interpolation
// is the remaining completeness half.
//
// SOUNDNESS is again structural: `Safe` is returned only after the converged
// frame is *independently re-verified* as an inductive invariant; `Unsafe` only
// via `bmc_bad_reachable`. A bug in the blocking/propagation can only cause an
// abstain (or, bounded by `max_frames`, non-termination), never a wrong verdict.
// ============================================================================

/// A literal of the predicate abstraction: `(predicate index, polarity)`.
type Lit = (usize, bool);
/// A cube = a (possibly partial) conjunction of predicate literals.
type IcCube = Vec<Lit>;

/// The z3-scope-bound abstraction pieces (built once per solver scope). Holds
/// owned z3 `Bool`s valid for the enclosing [`z3::with_z3_config`] closure.
struct AbsScope {
    pred_cur: Vec<Bool>,
    pred_nx: Vec<Bool>,
    transition: Bool,
    init_cur: Bool,
    bad_cur: Bool,
    bad_nx: Bool,
}

/// The named state registers of an encoded design — the names a predicate may use (the
/// candidates an "unknown register" abstention lists, mununu#1121).
fn state_register_names(
    view: &crate::adapter::sidecar::predicate_image::btor2_encode::Btor2SmtView,
) -> Vec<String> {
    view.signals
        .iter()
        .filter(|s| s.kind == SignalKind::State)
        .filter_map(|s| s.symbol.clone())
        .collect()
}

/// Build the abstraction scope for `preds` over the exact transition — the shared
/// cur/nx interface, the instantiated transition, and each predicate's z3 `Bool`
/// over current- and next-state. Must be called inside a `with_z3_config` closure.
fn build_abs_scope(
    view: &crate::adapter::sidecar::predicate_image::btor2_encode::Btor2SmtView,
    preds: &[PredicateExpr],
    props: &Props,
) -> Option<AbsScope> {
    let name_to_nid: HashMap<String, Nid> = view
        .signals
        .iter()
        .filter(|s| s.kind == SignalKind::State)
        .filter_map(|s| s.symbol.clone().map(|sym| (sym, s.nid)))
        .collect();
    let nid_to_name: HashMap<Nid, String> =
        name_to_nid.iter().map(|(n, &d)| (d, n.clone())).collect();
    let named = |suffix: &str| -> HashMap<Nid, BV> {
        view.state_curr
            .iter()
            .filter_map(|(nid, bv)| {
                let sym = nid_to_name.get(nid)?;
                Some((*nid, BV::new_const(format!("{sym}{suffix}"), bv.get_size())))
            })
            .collect()
    };
    let cur = named("__c");
    let nx = named("__n");
    let inp: HashMap<Nid, BV> = view
        .inputs
        .iter()
        .map(|(nid, bv)| (*nid, BV::new_const(format!("i{nid}__x"), bv.get_size())))
        .collect();
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

    let build_over = |frame: &HashMap<Nid, BV>, p: &PredicateExpr| -> Option<Bool> {
        let lookup = |name: &str| {
            name_to_nid
                .get(name)
                .and_then(|nid| frame.get(nid))
                .cloned()
        };
        p.build_constraint(&lookup)
    };
    let mut pred_cur = Vec::new();
    let mut pred_nx = Vec::new();
    for p in preds {
        pred_cur.push(build_over(&cur, p)?);
        pred_nx.push(build_over(&nx, p)?);
    }
    Some(AbsScope {
        pred_cur,
        pred_nx,
        transition,
        init_cur: init_bool(view, props, &cur, &nid_to_name),
        bad_cur: bad_bool(view, props, &cur),
        bad_nx: bad_bool(view, props, &nx),
    })
}

/// `⋀ literal` of a cube over the given predicate frame (`pred_cur` or `pred_nx`).
fn cube_over(preds: &[Bool], c: &IcCube) -> Bool {
    if c.is_empty() {
        return Bool::from_bool(true);
    }
    let lits: Vec<Bool> = c
        .iter()
        .map(|&(i, pos)| {
            if pos {
                preds[i].clone()
            } else {
                preds[i].not()
            }
        })
        .collect();
    Bool::and(&lits.iter().collect::<Vec<_>>())
}

/// The frame formula `F(i)` over current-state: the property `¬bad` (implicitly in
/// every IC3 frame), `Init` (only at `i == 0`), and `¬cube` for every cube blocked
/// at level `≥ i` (delta encoding).
fn frame_formula(scope: &AbsScope, frames: &[Vec<IcCube>], i: usize) -> Bool {
    // `¬bad` is part of every frame — an IC3 frame over-approximates the states
    // reachable in ≤ i steps AND is contained in the safe region.
    let mut terms: Vec<Bool> = vec![scope.bad_cur.not()];
    if i == 0 {
        terms.push(scope.init_cur.clone());
    }
    for level in frames.iter().skip(i) {
        for c in level {
            terms.push(cube_over(&scope.pred_cur, c).not());
        }
    }
    Bool::and(&terms.iter().collect::<Vec<_>>())
}

/// The full predicate valuation of a z3 model (over `pred_cur`) as a cube.
fn extract_cube(model: &z3::Model, pred_cur: &[Bool]) -> IcCube {
    pred_cur
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let v = model
                .eval(p, true)
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            (i, v)
        })
        .collect()
}

/// P2.2b/P2.3b — decide `AG ¬bad` by the IC3 frame ladder over the predicate
/// abstraction, with sequence-interpolation refinement (P2.3b). Sound by independent
/// verdict verification (see the module banner); guarded so it always terminates
/// (abstains on non-convergence). `max_frames` bounds the ladder depth; `max_refine`
/// the refinement loop.
///
/// **Honest status (the make-or-break result, 2026-07-13):** decides small designs
/// but does NOT converge on real ones (even 644-byte `paper_v3`) — the CTI blocking
/// hits its iteration guard and abstains. z3-SPACER decides these instantly. A
/// competitive owned IC3/PDR is a large specialised engine; this is a research
/// FOUNDATION whose *reusable* value is the refinement primitives in
/// [`super::refine`] (P2.1/P2.3, reused by the branching driver P2.4), not a
/// production safety engine — safety-at-scale is covered by
/// [`super::native_interp`] + z3-SPACER. Its one surface is `btor2 verify --engine ic3`
/// (`recoverability::verify_safety_with_engine`), where an abstention's reason is the
/// report's `detail`.
pub fn verify_safety_ic3(
    file: &Btor2File,
    max_frames: usize,
    max_refine: u32,
    timeout_ms: u32,
) -> AbsVerdict {
    let mut preds = seed_predicates(file);
    for _ in 0..max_refine {
        match ic3_pass(file, &preds, max_frames, timeout_ms) {
            Ic3Pass::Safe => {
                return AbsVerdict::Safe {
                    predicates: preds.len(),
                };
            }
            Ic3Pass::Unsafe { depth } => return AbsVerdict::Unsafe { depth },
            Ic3Pass::Refine(step) => {
                let before = preds.len();
                for p in step.predicates {
                    if !preds.contains(&p) {
                        preds.push(p);
                    }
                }
                if preds.len() == before {
                    return AbsVerdict::barren(BarrenStep::NothingNew, &step.gaps);
                }
            }
            Ic3Pass::NoRefinement(gaps) => {
                return AbsVerdict::barren(BarrenStep::NoPredicate, &gaps);
            }
            Ic3Pass::Unknown(reason) => return AbsVerdict::unknown(reason),
        }
    }
    AbsVerdict::unknown(format!(
        "IC3 no convergence within {max_refine} refinements"
    ))
}

enum Ic3Pass {
    Safe,
    Unsafe {
        depth: u32,
    },
    Refine(Refinement),
    /// The spurious counterexample's refinement gave no predicate; its queries' gaps.
    NoRefinement(Vec<RefineGap>),
    Unknown(String),
}

fn ic3_pass(
    file: &Btor2File,
    preds: &[PredicateExpr],
    max_frames: usize,
    timeout_ms: u32,
) -> Ic3Pass {
    let props = extract_props(file);
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = match encode_design(file) {
            Ok(v) => v,
            Err(e) => return Ic3Pass::Unknown(format!("encode: {e}")),
        };
        let scope = match build_abs_scope(&view, preds, &props) {
            Some(s) => s,
            None => {
                return Ic3Pass::Unknown(PredicateExpr::unknown_register_reason(
                    "predicate references unknown register",
                    preds,
                    state_register_names(&view).iter().map(String::as_str),
                ));
            }
        };
        let mk = || {
            let s = z3::Solver::new();
            let mut p = z3::Params::new();
            p.set_u32("timeout", timeout_ms);
            s.set_params(&p);
            s
        };

        // Init ∧ bad? (exact formulas — a real bad init state.)
        if sat_of(&mk, &[&scope.init_cur, &scope.bad_cur]) == z3::SatResult::Sat {
            return Ic3Pass::Unsafe { depth: 0 };
        }

        // frames[0] = Init; frames[k≥1] start empty (F(k) = ⊤).
        let mut frames: Vec<Vec<IcCube>> = vec![vec![]];
        let mut k = 0usize;
        loop {
            k += 1;
            frames.push(Vec::new());
            if k > max_frames {
                return Ic3Pass::Unknown(technique_limit(
                    &format!("the abstract IC3 did not converge within {max_frames} frames"),
                    OTHER_ENGINES,
                ));
            }

            // Block every bad-predecessor in F(k). Bounded so a blocking that fails to
            // converge (an implementation-completeness gap) ABSTAINS rather than hangs.
            let mut block_iters = 0u32;
            loop {
                block_iters += 1;
                if block_iters > 400 {
                    return Ic3Pass::Unknown(technique_limit(
                        "the abstract IC3 did not converge within its limit of 400 blocking \
                         iterations at one frame",
                        OTHER_ENGINES,
                    ));
                }
                let query = Bool::and(&[
                    &frame_formula(&scope, &frames, k),
                    &scope.transition,
                    &scope.bad_nx,
                ]);
                let s = mk();
                s.assert(&query);
                match s.check() {
                    z3::SatResult::Unsat => break, // no bad-predecessor in F(k)
                    z3::SatResult::Unknown => {
                        return Ic3Pass::Unknown(solver_timeout(timeout_ms, "bad-predecessor"));
                    }
                    z3::SatResult::Sat => {
                        let model = match s.get_model() {
                            Some(m) => m,
                            None => return Ic3Pass::Unknown(sat_without_model("bad-predecessor")),
                        };
                        let cti = extract_cube(&model, &scope.pred_cur);
                        match rec_block(file, &scope, &mut frames, cti, k, &mk, timeout_ms) {
                            BlockResult::Blocked => continue,
                            BlockResult::Unsafe { depth } => {
                                return Ic3Pass::Unsafe { depth };
                            }
                            BlockResult::Refine(step) => return Ic3Pass::Refine(step),
                            BlockResult::NoRefinement(gaps) => {
                                return Ic3Pass::NoRefinement(gaps);
                            }
                            BlockResult::Unknown(r) => return Ic3Pass::Unknown(r),
                        }
                    }
                }
            }

            // Propagate ALL clauses forward first (push any clause still inductive at
            // the next level), THEN test for a fixpoint.
            for i in 1..k {
                let movable: Vec<IcCube> = frames[i].clone();
                let mut keep = Vec::new();
                for g in movable {
                    let q = Bool::and(&[
                        &frame_formula(&scope, &frames, i),
                        &scope.transition,
                        &cube_over(&scope.pred_nx, &g),
                    ]);
                    if sat_of(&mk, &[&q]) == z3::SatResult::Unsat {
                        frames[i + 1].push(g); // still inductive at i+1 → push
                    } else {
                        keep.push(g);
                    }
                }
                frames[i] = keep;
            }
            // Fixpoint: some frame `i` with an empty delta AND whose `F(i+1)` is a
            // genuine inductive invariant (independently re-verified — the soundness
            // gate). A non-fixpoint empty frame (nothing yet blocked there) fails the
            // check and the ladder simply deepens.
            for i in 1..k {
                if frames[i].is_empty() && verify_inductive(&scope, &frames, i + 1, &mk) {
                    return Ic3Pass::Safe;
                }
            }
        }
    })
}

enum BlockResult {
    Blocked,
    Unsafe {
        depth: u32,
    },
    Refine(Refinement),
    /// Neither the single-step query nor the sequence interpolation gave a predicate; the
    /// gaps of their queries.
    NoRefinement(Vec<RefineGap>),
    Unknown(String),
}

/// Block cube `s0` at level `lvl` (prove it unreachable in ≤ `lvl` steps) via an
/// obligation queue over the exact transition. On reaching frame 0 (an abstract
/// counterexample to `Init`), classify concretely: a real BMC counterexample ⇒
/// `Unsafe`; otherwise refine (P2.1) or abstain.
#[allow(clippy::too_many_arguments)]
fn rec_block(
    file: &Btor2File,
    scope: &AbsScope,
    frames: &mut [Vec<IcCube>],
    s0: IcCube,
    lvl: usize,
    mk: &dyn Fn() -> z3::Solver,
    timeout_ms: u32,
) -> BlockResult {
    // Obligations processed lowest-level-first. Bounded so a runaway obligation
    // chain abstains rather than hangs.
    let mut obligations: Vec<(usize, IcCube)> = vec![(lvl, s0)];
    const MAX_OBLIGATIONS: u32 = 2000;
    let mut steps = 0u32;
    while let Some(idx) = obligations
        .iter()
        .enumerate()
        .min_by_key(|(_, (l, _))| *l)
        .map(|(i, _)| i)
    {
        steps += 1;
        if steps > MAX_OBLIGATIONS {
            return BlockResult::Unknown(technique_limit(
                &format!(
                    "the abstract IC3 did not converge within its limit of {MAX_OBLIGATIONS} \
                     proof obligations"
                ),
                OTHER_ENGINES,
            ));
        }
        let (j, c) = obligations.remove(idx);
        if j == 0 {
            // Abstract CEX reaches Init. Concrete check bounds by the ladder depth.
            match bmc_bad_reachable(file, (frames.len() as u32 + 2).max(4)) {
                Ok(BmcOutcome::Violated { depth }) => return BlockResult::Unsafe { depth },
                _ => {
                    // Spurious. First try the cheap single-step refinement.
                    let source = cube_to_predicate_list(scope, &c);
                    let bad_atoms = bad_target_atoms(file);
                    // Every query that gives no predicate leaves its gap: whether the step
                    // met the grammar or was cut short is read from them (mununu#1188).
                    let mut gaps: Vec<RefineGap> = Vec::new();
                    match synthesize_refinement_predicate(file, &source, &bad_atoms, timeout_ms) {
                        RefineOutcome::Predicate(p) => {
                            return BlockResult::Refine(Refinement {
                                predicates: vec![p],
                                gaps,
                            });
                        }
                        RefineOutcome::Realizable => {}
                        RefineOutcome::Unavailable(gap) => gaps.push(gap),
                    }
                    // P2.3b — targeted per-trace refinement: SEQUENCE-interpolate the
                    // depth-n BMC unrolling of this spurious counterexample → a SET of
                    // reachability predicates that collectively rule the trace out.
                    let seq = sequence_interpolant_predicates(file, frames.len(), timeout_ms);
                    gaps.extend(seq.gaps);
                    if !seq.predicates.is_empty() {
                        return BlockResult::Refine(Refinement {
                            predicates: seq.predicates,
                            gaps,
                        });
                    }
                    return BlockResult::NoRefinement(gaps);
                }
            }
        }
        // Predecessor of c in F(j-1)?  F(j-1) ∧ T ∧ c'
        let q = Bool::and(&[
            &frame_formula(scope, frames, j - 1),
            &scope.transition,
            &cube_over(&scope.pred_nx, &c),
        ]);
        let s = mk();
        s.assert(&q);
        match s.check() {
            z3::SatResult::Unknown => {
                return BlockResult::Unknown(solver_timeout(timeout_ms, "blocking"));
            }
            z3::SatResult::Sat => {
                // Predecessor exists → block it first, then retry c.
                let model = match s.get_model() {
                    Some(m) => m,
                    None => return BlockResult::Unknown(sat_without_model("blocking")),
                };
                let d = extract_cube(&model, &scope.pred_cur);
                obligations.push((j - 1, d));
                obligations.push((j, c));
            }
            z3::SatResult::Unsat => {
                // No predecessor in F(j-1). If c intersects Init we cannot block it as
                // unreachable — defer to the frame-0 concrete check.
                if sat_of(mk, &[&scope.init_cur, &cube_over(&scope.pred_cur, &c)])
                    == z3::SatResult::Sat
                {
                    obligations.push((0, c));
                    continue;
                }
                // Generalize (MIC) and add ¬g to F(1..=j).
                let g = mic(scope, frames, &c, j, mk);
                frames[j].push(g);
            }
        }
    }
    BlockResult::Blocked
}

/// Minimal inductive clause: greedily drop literals from `c` while the reduced cube
/// still has no predecessor in `F(j-1)` and does not intersect `Init`.
fn mic(
    scope: &AbsScope,
    frames: &[Vec<IcCube>],
    c: &IcCube,
    j: usize,
    mk: &dyn Fn() -> z3::Solver,
) -> IcCube {
    let mut cur = c.clone();
    let mut i = 0;
    while i < cur.len() && cur.len() > 1 {
        let mut trial = cur.clone();
        trial.remove(i);
        let no_pred = sat_of(
            mk,
            &[
                &frame_formula(scope, frames, j - 1),
                &scope.transition,
                &cube_over(&scope.pred_nx, &trial),
            ],
        ) == z3::SatResult::Unsat;
        let init_disjoint = sat_of(mk, &[&scope.init_cur, &cube_over(&scope.pred_cur, &trial)])
            == z3::SatResult::Unsat;
        if no_pred && init_disjoint {
            cur = trial; // keep the drop
        } else {
            i += 1;
        }
    }
    cur
}

/// The frame formula `F(i)` over **next**-state (`¬bad_nx ∧ ⋀ ¬cube_over(pred_nx)`),
/// for the consecution check. Only called with `i ≥ 1` (no `Init` term).
fn frame_formula_nx(scope: &AbsScope, frames: &[Vec<IcCube>], i: usize) -> Bool {
    let mut terms: Vec<Bool> = vec![scope.bad_nx.not()];
    for level in frames.iter().skip(i) {
        for c in level {
            terms.push(cube_over(&scope.pred_nx, c).not());
        }
    }
    Bool::and(&terms.iter().collect::<Vec<_>>())
}

/// Independently verify `F(i)` is an inductive invariant: `Init ⟹ F(i)`,
/// `F(i) ⟹ ¬bad` (structural — `¬bad` is in every frame), `F(i) ∧ T ⟹ F(i)'`.
/// The soundness gate for `Safe`.
fn verify_inductive(
    scope: &AbsScope,
    frames: &[Vec<IcCube>],
    i: usize,
    mk: &dyn Fn() -> z3::Solver,
) -> bool {
    let inv = frame_formula(scope, frames, i);
    let inv_nx = frame_formula_nx(scope, frames, i);
    // Init ⟹ inv
    if sat_of(mk, &[&scope.init_cur, &inv.not()]) != z3::SatResult::Unsat {
        return false;
    }
    // inv ∧ T ⟹ inv'  (inv ⟹ ¬bad is structural)
    sat_of(mk, &[&inv, &scope.transition, &inv_nx.not()]) == z3::SatResult::Unsat
}

/// A cube's literals as a `PredicateExpr` list (positive literal → the predicate,
/// negative → its negation), for handing a spurious CTI to the refinement primitive.
fn cube_to_predicate_list(_scope: &AbsScope, _c: &IcCube) -> Vec<PredicateExpr> {
    // The scope holds z3 Bools, not the source PredicateExprs; the caller passes the
    // predicate list separately. This shim returns empty (source = ⊤) so the
    // refinement asks "from anywhere, can we reach bad?" — a coarse but sound query.
    Vec::new()
}

/// The `bad` cone's `eq(register, const)` atoms as a `PredicateExpr` target list.
fn bad_target_atoms(file: &Btor2File) -> Vec<PredicateExpr> {
    // Reuse the seed extraction, then keep only atoms that appear in the bad cone —
    // approximated here as all seeded comparison atoms (sound: a superset target only
    // makes the refinement query easier to satisfy, never unsound).
    seed_predicates(file)
}

fn recover_path(target: &Cube, parent: &HashMap<Cube, Cube>) -> Vec<Cube> {
    let mut path = vec![target.clone()];
    let mut cur = target;
    while let Some(p) = parent.get(cur) {
        path.push(p.clone());
        cur = p;
    }
    path.reverse();
    path
}

fn recover_path_len(target: &Cube, parent: &HashMap<Cube, Cube>) -> u32 {
    (recover_path(target, parent).len() as u32).saturating_sub(1)
}

/// A cube's positive literals as `register == value` predicates is NOT recoverable
/// (a cube is a valuation of *derived* predicates, not registers); instead the
/// source/target regions handed to the refinement primitive are the cube's
/// predicates themselves at their valuation.
fn cube_to_predicates(cube: &Cube, preds: &[PredicateExpr]) -> Vec<PredicateExpr> {
    preds
        .iter()
        .zip(cube)
        .map(|(p, &pos)| {
            if pos {
                p.clone()
            } else {
                PredicateExpr::Not(Box::new(p.clone()))
            }
        })
        .collect()
}

// ---- shared small helpers (mirrors of native_interp's, kept local so this module
//      does not depend on native_interp's private internals) ----

struct Props {
    bad: Vec<Nid>,
    init: Vec<(Nid, Nid)>,
}

fn extract_props(file: &Btor2File) -> Props {
    let mut p = Props {
        bad: Vec::new(),
        init: Vec::new(),
    };
    for l in &file.lines {
        match &l.node {
            Node::Bad { signal } => p.bad.push(signal.nid()),
            Node::Init { state, value, .. } => p.init.push((*state, value.nid())),
            _ => {}
        }
    }
    p
}

fn curr_bv<'a>(
    view: &'a crate::adapter::sidecar::predicate_image::btor2_encode::Btor2SmtView,
    nid: &Nid,
) -> Option<&'a BV> {
    view.signal_bvs
        .get(nid)
        .or_else(|| view.state_curr.get(nid))
        .or_else(|| view.inputs.get(nid))
}

fn bad_bool(
    view: &crate::adapter::sidecar::predicate_image::btor2_encode::Btor2SmtView,
    props: &Props,
    frame: &HashMap<Nid, BV>,
) -> Bool {
    // Substitute state_curr → frame in each bad operand's BV.
    let one1 = BV::from_u64(1, 1);
    let subs: Vec<(&BV, &BV)> = view
        .state_curr
        .iter()
        .filter_map(|(nid, bv)| frame.get(nid).map(|f| (bv, f)))
        .collect();
    let disj: Vec<Bool> = props
        .bad
        .iter()
        .filter_map(|op| curr_bv(view, op).map(|bv| bv.substitute(&subs).eq(&one1)))
        .collect();
    if disj.is_empty() {
        Bool::from_bool(false)
    } else {
        Bool::or(&disj.iter().collect::<Vec<_>>())
    }
}

fn init_bool(
    view: &crate::adapter::sidecar::predicate_image::btor2_encode::Btor2SmtView,
    props: &Props,
    cur: &HashMap<Nid, BV>,
    _nid_to_name: &HashMap<Nid, String>,
) -> Bool {
    let subs: Vec<(&BV, &BV)> = view
        .state_curr
        .iter()
        .filter_map(|(nid, bv)| cur.get(nid).map(|c| (bv, c)))
        .collect();
    let conj: Vec<Bool> = props
        .init
        .iter()
        .filter_map(|(state, value_nid)| {
            let c = cur.get(state)?;
            let vbv = curr_bv(view, value_nid)?.substitute(&subs);
            Some(c.eq(&vbv))
        })
        .collect();
    if conj.is_empty() {
        Bool::from_bool(true)
    } else {
        Bool::and(&conj.iter().collect::<Vec<_>>())
    }
}

/// Seed predicates: the `register == reset_value` atom of each named state cell
/// whose init is a constant (the reachability-constraining atoms), plus a
/// `register == 0` fallback. Cheap and always available; refinement adds the rest.
fn seed_predicates(file: &Btor2File) -> Vec<PredicateExpr> {
    let mut const_vals: HashMap<Nid, u64> = HashMap::new();
    let mut sym: HashMap<Nid, String> = HashMap::new();
    for l in &file.lines {
        match &l.node {
            Node::State {
                symbol: Some(s), ..
            } => {
                sym.insert(l.nid, s.clone());
            }
            Node::Const { value, .. } => {
                let v: Option<u64> = match value {
                    ConstValue::Zero => Some(0),
                    ConstValue::One => Some(1),
                    ConstValue::Dec(d) => u64::try_from(*d).ok(),
                    ConstValue::Bin(s) => u64::from_str_radix(s, 2).ok(),
                    ConstValue::Hex(s) => u64::from_str_radix(s, 16).ok(),
                    ConstValue::Ones => None, // width-dependent; skip
                };
                if let Some(v) = v {
                    const_vals.insert(l.nid, v);
                }
            }
            _ => {}
        }
    }
    let mut preds: Vec<PredicateExpr> = Vec::new();
    let mut push = |name: &str, v: u64| {
        let p = PredicateExpr::Cmp {
            register: name.to_string(),
            op: CmpOp::Eq,
            value: v,
        };
        if !preds.contains(&p) {
            preds.push(p);
        }
    };
    // (1) reset-value atoms — `register == init_value`.
    for l in &file.lines {
        if let Node::Init { state, value, .. } = &l.node
            && let (Some(name), Some(v)) = (sym.get(state), const_vals.get(&value.nid()))
        {
            // PR A5 — `init s -N` is the constant's bit-not within its width.
            let v = if value.is_negated() {
                let w =
                    crate::adapter::btor2::parser::node_bv_width(file, value.nid()).unwrap_or(64);
                let mask = if w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
                !*v & mask
            } else {
                *v
            };
            push(name, v);
        }
    }
    // (2) comparison atoms in the design — any `eq(state_register, const)` (the
    // control's decision conditions, incl. the `bad` cone). Cheap, sound, and it
    // makes the abstraction distinguish the states the property cares about.
    for l in &file.lines {
        if let Node::Op {
            op: crate::adapter::btor2::ast::Op::Eq,
            args,
            ..
        } = &l.node
            && args.len() == 2
        {
            let (n0, n1) = (args[0].nid(), args[1].nid());
            if let (Some(name), Some(v)) = (sym.get(&n0), const_vals.get(&n1)) {
                push(name, *v);
            } else if let (Some(name), Some(v)) = (sym.get(&n1), const_vals.get(&n0)) {
                push(name, *v);
            }
        }
    }
    preds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::btor2::native_interp::stand_in_solver::{StandIn, stand_in};
    use crate::adapter::btor2::parser;

    /// Whether `v` is an abstention these tests cannot judge on this host: the refinement had
    /// no cvc5. Read from the typed gap, not from the reason's words; and under
    /// `MUNUNU_REQUIRE_TOOLS=1` it is a failure, not a skip (mununu#981).
    fn skipped_without_cvc5(v: &AbsVerdict) -> bool {
        let AbsVerdict::Unknown {
            reason,
            refinement: Some(GapKind::SolverMissing),
        } = v
        else {
            return false;
        };
        crate::adapter::require_tool("cvc5 (the abstract IC3 refinement)", false);
        eprintln!("SKIP: {reason}");
        true
    }

    /// Trivially safe: `x` holds at 0, `bad = (x == 3)`. R# = {x==0} excludes bad
    /// and is inductive — Safe with no refinement.
    #[test]
    fn abs_proves_constant_hold_safe() {
        const HOLD: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n5 next 1 3 3\n\
                            6 constd 1 3\n7 sort bitvec 1\n8 eq 7 3 6\n9 bad 8\n";
        let file = parser::parse(HOLD).expect("parse");
        match verify_safety_abs(&file, 8, 4096, 5_000) {
            AbsVerdict::Safe { .. } => {}
            v if skipped_without_cvc5(&v) => {}
            other => panic!("expected Safe, got {other:?}"),
        }
    }

    /// A NON-1-inductive safe design decided by the abstraction: `a` holds at 0,
    /// `b = a + 1`, `bad = (b == 2)`. `b` reaches only 1 (since `a` stays 0), never
    /// 2 — but `¬(b==2)` is not 1-inductive (from a free `a==1`, `b'=2`). The
    /// `a==0` reset atom + the `b==2` bad atom give an abstraction whose
    /// may-reachable set excludes bad → verified Safe. This is the case native
    /// k-induction abstains on that the predicate abstraction gets.
    #[test]
    fn abs_proves_non_inductive_safe_via_abstraction() {
        // a: hold at 0; b: next = a + 1; bad = (b == 2).
        const D: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 a\n4 init 1 3 2\n5 next 1 3 3\n\
                         6 state 1 b\n7 init 1 6 2\n8 one 1\n9 add 1 3 8\n10 next 1 6 9\n\
                         11 constd 1 2\n12 sort bitvec 1\n13 eq 12 6 11\n14 bad 13\n";
        let file = parser::parse(D).expect("parse");
        match verify_safety_abs(&file, 8, 4096, 5_000) {
            AbsVerdict::Safe { .. } => {}
            v if skipped_without_cvc5(&v) => {}
            // NEVER Unsafe (b never reaches 2).
            other => panic!("expected Safe via abstraction, got {other:?}"),
        }
    }

    /// Genuinely reachable `bad` must be `Unsafe` (via the concrete-BMC gate).
    #[test]
    fn abs_refutes_reachable_bad() {
        // x init 0; next x = x + 1 (2-bit); bad = (x == 3). Reachable at depth 3.
        const REACH: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n5 one 1\n\
                             6 add 1 3 5\n7 next 1 3 6\n8 constd 1 3\n9 sort bitvec 1\n\
                             10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(REACH).expect("parse");
        match verify_safety_abs(&file, 8, 4096, 5_000) {
            AbsVerdict::Unsafe { .. } => {}
            v if skipped_without_cvc5(&v) => {}
            other => panic!("expected Unsafe, got {other:?}"),
        }
    }

    /// `bad` at init ⇒ Unsafe depth 0.
    #[test]
    fn abs_detects_initial_violation() {
        const INIT_BAD: &str =
            "1 sort bitvec 1\n2 one 1\n3 state 1 x\n4 init 1 3 2\n5 next 1 3 2\n6 bad 3\n";
        let file = parser::parse(INIT_BAD).expect("parse");
        match verify_safety_abs(&file, 8, 4096, 5_000) {
            AbsVerdict::Unsafe { depth } => assert_eq!(depth, 0),
            v if skipped_without_cvc5(&v) => {}
            other => panic!("expected Unsafe depth 0, got {other:?}"),
        }
    }

    // ---- P2.2b: the IC3 frame ladder (verify_safety_ic3) ----

    #[test]
    fn ic3_proves_constant_hold_safe() {
        const HOLD: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n5 next 1 3 3\n\
                            6 constd 1 3\n7 sort bitvec 1\n8 eq 7 3 6\n9 bad 8\n";
        let file = parser::parse(HOLD).expect("parse");
        match verify_safety_ic3(&file, 32, 8, 5_000) {
            AbsVerdict::Safe { .. } => {}
            v if skipped_without_cvc5(&v) => {}
            other => panic!("expected Safe (IC3), got {other:?}"),
        }
    }

    #[test]
    fn ic3_refutes_reachable_bad() {
        const REACH: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n5 one 1\n\
                             6 add 1 3 5\n7 next 1 3 6\n8 constd 1 3\n9 sort bitvec 1\n\
                             10 eq 9 3 8\n11 bad 10\n";
        let file = parser::parse(REACH).expect("parse");
        match verify_safety_ic3(&file, 32, 8, 5_000) {
            AbsVerdict::Unsafe { .. } => {}
            v if skipped_without_cvc5(&v) => {}
            other => panic!("expected Unsafe (IC3), got {other:?}"),
        }
    }

    #[test]
    fn ic3_detects_initial_violation() {
        const INIT_BAD: &str =
            "1 sort bitvec 1\n2 one 1\n3 state 1 x\n4 init 1 3 2\n5 next 1 3 2\n6 bad 3\n";
        let file = parser::parse(INIT_BAD).expect("parse");
        match verify_safety_ic3(&file, 32, 8, 5_000) {
            AbsVerdict::Unsafe { depth } => assert_eq!(depth, 0),
            v if skipped_without_cvc5(&v) => {}
            other => panic!("expected Unsafe depth 0 (IC3), got {other:?}"),
        }
    }

    /// P2.3 make-or-break probe: run the IC3 frame ladder (with path-interpolation
    /// refinement) on a real HWMCC design (`MUNUNU_INTERP_PROBE`), printing the
    /// verdict + wall-clock — does IC3's cheap blocking + rare interpolation decide
    /// it? Ignored by default (needs an external file + cvc5).
    #[test]
    #[ignore = "manual: manual: reads an external BTOR2 file named by MUNUNU_INTERP_PROBE"]
    fn probe_ic3_on_external_design() {
        let Ok(path) = std::env::var("MUNUNU_INTERP_PROBE") else {
            eprintln!(
                "SKIPPED: opt-in probe — set MUNUNU_INTERP_PROBE=/path/to/design.btor2 to run it."
            );
            return;
        };
        let content = std::fs::read_to_string(&path).expect("read design");
        let file = parser::parse(&content).expect("parse btor2");
        let eu = |k: &str, d: u32| {
            std::env::var(k)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(d)
        };
        let t0 = std::time::Instant::now();
        let v = verify_safety_ic3(
            &file,
            eu("MUNUNU_IC3_FRAMES", 40) as usize,
            eu("MUNUNU_IC3_REFINE", 24),
            eu("MUNUNU_IC3_QTO", 8_000),
        );
        eprintln!(
            "IC3 verdict on {path} [{}ms]: {v:?}",
            t0.elapsed().as_millis()
        );
    }

    /// The IC3 frame ladder proves the non-1-inductive safe design via *incremental
    /// blocking* (invariant re-verified before Safe) — no reachable-cube enumeration.
    /// `a` holds at 0, `b = a + 1`, `bad = (b == 2)`.
    #[test]
    fn ic3_proves_non_inductive_safe() {
        const D: &str = "1 sort bitvec 2\n2 zero 1\n3 state 1 a\n4 init 1 3 2\n5 next 1 3 3\n\
                         6 state 1 b\n7 init 1 6 2\n8 one 1\n9 add 1 3 8\n10 next 1 6 9\n\
                         11 constd 1 2\n12 sort bitvec 1\n13 eq 12 6 11\n14 bad 13\n";
        let file = parser::parse(D).expect("parse");
        match verify_safety_ic3(&file, 32, 8, 5_000) {
            AbsVerdict::Safe { .. } => {}
            v if skipped_without_cvc5(&v) => {}
            // NEVER Unsafe (b never reaches 2) — the soundness gate guarantees it.
            other => panic!("expected Safe via IC3 blocking, got {other:?}"),
        }
    }

    /// The design of the mununu#1188 reproducer: a 3-bit `x` from 0 that steps by 2, and
    /// `bad = (x == 5)`. It is safe (`x` stays even), and the seeds `x == 0` / `x == 5` do not
    /// prove it: `x == 3`, the predecessor of `bad`, is in the first frame, so the IC3 pass has
    /// a spurious counterexample to rule out and asks cvc5 for an interpolant.
    const STEP_BY_TWO: &str = "1 sort bitvec 3\n2 zero 1\n3 state 1 x\n4 init 1 3 2\n\
                               5 constd 1 2\n6 add 1 3 5\n7 next 1 3 6\n8 constd 1 5\n\
                               9 sort bitvec 1\n10 eq 9 3 8\n11 bad 10\n";

    /// The IC3 pass on [`STEP_BY_TWO`] with a stand-in for cvc5 that does `what` with every
    /// query. z3 runs for real: only the interpolation queries meet the stand-in.
    fn ic3_with_a_stand_in_cvc5(what: StandIn) -> AbsVerdict {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = stand_in(dir.path(), what);
        let _env = crate::adapter::env_var::override_for_test("MUNUNU_CVC5_PATH", &fake);
        let file = parser::parse(STEP_BY_TWO).expect("parse");
        verify_safety_ic3(&file, 32, 8, 5_000)
    }

    /// REPRODUCER (mununu#1188) — a cvc5 that stops at the query limit on every interpolation
    /// query leaves the IC3 pass with no predicate, and the report says the query was cut
    /// short: it carries the gap's kind and does not call the stop a grammar ceiling. Fails on
    /// the parent, where the reason of this run read "grammar ceiling" and nothing in the
    /// verdict told a stopped solver from an answered one. The stand-in exits the way cvc5
    /// does at `--tlimit`, at once, so the row is the same on every host.
    #[test]
    fn x1188_a_refinement_cut_short_at_the_query_limit_is_not_called_a_grammar_ceiling() {
        let v = ic3_with_a_stand_in_cvc5(StandIn::Exits {
            code: 134,
            stderr: "cvc5 interrupted by timeout.",
        });
        let AbsVerdict::Unknown { reason, refinement } = &v else {
            panic!("no interpolation query was answered, so the pass abstains: {v:?}");
        };
        assert_eq!(
            *refinement,
            Some(GapKind::SolverStopped),
            "the abstention carries what cut the refinement short: {reason}"
        );
        assert!(
            !reason.contains(GRAMMAR_CEILING),
            "a query the solver did not answer says nothing about the interpolants: {reason}"
        );
    }

    /// The control of the reproducer: the same design, and a cvc5 that answers every query
    /// with something the predicate grammar does not take. Every query was answered, so this
    /// one is the ceiling, and no gap kind is reported as having cut the step short.
    #[test]
    fn x1188_a_refinement_whose_every_query_was_answered_is_the_grammar_ceiling() {
        let v = ic3_with_a_stand_in_cvc5(StandIn::Replies { bytes: 4 });
        let AbsVerdict::Unknown { reason, refinement } = &v else {
            panic!("no interpolant the grammar takes, so the pass abstains: {v:?}");
        };
        assert_eq!(*refinement, None, "{reason}");
        assert!(reason.contains(GRAMMAR_CEILING), "{reason}");
    }

    /// ENUMERATING (mununu#1188, class `member-stop-reason`) — every kind of refinement gap,
    /// at both sites of the IC3 pass that report a barren refinement and at the single-step
    /// pass's: an answered query (no interpolant, one outside the grammar) is the technique's
    /// ceiling and names no kind; every other kind is named, carries its fact, and is never
    /// called the ceiling — alone, and beside an answered query in either order. Exhaustive
    /// over [`GapKind::ALL`], so a new kind is judged here before it ships.
    #[test]
    fn x1188_every_gap_kind_decides_whether_a_barren_refinement_is_the_grammar_ceiling() {
        let gap = |kind: GapKind| RefineGap::new(kind, format!("the fact of {kind:?}"));
        let answered: Vec<GapKind> = GapKind::ALL
            .into_iter()
            .filter(|k| k.solver_answered())
            .collect();
        assert_eq!(
            answered,
            [GapKind::NoInterpolant, GapKind::OutsideGrammar],
            "the answered kinds"
        );
        for step in [BarrenStep::NoPredicate, BarrenStep::NothingNew] {
            let (reason, told) = barren_refinement(step, &[]);
            assert_eq!(told, None, "{step:?}, no gap: {reason}");
            assert!(reason.contains(GRAMMAR_CEILING), "{step:?}: {reason}");
            for kind in GapKind::ALL {
                let (reason, told) = barren_refinement(step, &[gap(kind)]);
                if kind.solver_answered() {
                    assert_eq!(told, None, "{step:?} / {kind:?}: {reason}");
                    assert!(
                        reason.contains(GRAMMAR_CEILING),
                        "{step:?} / {kind:?}: {reason}"
                    );
                    continue;
                }
                assert_eq!(told, Some(kind), "{step:?} / {kind:?}: {reason}");
                assert!(
                    !reason.contains(GRAMMAR_CEILING),
                    "{step:?} / {kind:?}: {reason}"
                );
                assert!(
                    reason.contains(&gap(kind).what),
                    "{step:?} / {kind:?}: the report carries the gap's fact: {reason}"
                );
                for other in &answered {
                    for gaps in [[gap(*other), gap(kind)], [gap(kind), gap(*other)]] {
                        let (reason, told) = barren_refinement(step, &gaps);
                        assert_eq!(
                            told,
                            Some(kind),
                            "{step:?} / {kind:?} beside {other:?}: {reason}"
                        );
                        assert!(
                            !reason.contains(GRAMMAR_CEILING),
                            "{step:?} / {kind:?} beside {other:?}: {reason}"
                        );
                    }
                }
            }
        }
        // The single-step pass has no ceiling of its own to claim: with every query answered
        // it says what it lacks, and with one cut short it says what the IC3 pass says.
        for kind in GapKind::ALL {
            let (reason, told) = no_single_step_refinement(&[gap(kind)]);
            assert!(!reason.contains(GRAMMAR_CEILING), "{kind:?}: {reason}");
            if kind.solver_answered() {
                assert_eq!(told, None, "{kind:?}: {reason}");
            } else {
                assert_eq!(
                    (reason, told),
                    barren_refinement(BarrenStep::NoPredicate, &[gap(kind)]),
                    "{kind:?}"
                );
            }
        }
    }
}
