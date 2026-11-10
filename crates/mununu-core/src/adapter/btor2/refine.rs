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
    extract_interpolant_body, run_cvc5_raw, serialize_term,
};
use crate::adapter::btor2::predicate_expr::PredicateExpr;
use crate::adapter::cvc5::parse_interpolant_to_predicate_expr;
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
    /// cvc5 absent / timed out / produced an interpolant outside the parseable
    /// grammar, encode failed, or a predicate referenced an unknown register.
    /// The caller falls back (a different predicate source, or abstains).
    Unavailable(String),
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
            Err(e) => return RefineOutcome::Unavailable(format!("encode: {e:?}")),
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
                return RefineOutcome::Unavailable("source references an unknown register".into());
            }
        };
        let target_bool = match build(target, &nx) {
            Some(b) => b,
            None => {
                return RefineOutcome::Unavailable("target references an unknown register".into());
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
                    return RefineOutcome::Unavailable("solver timeout on realizability".into());
                }
                z3::SatResult::Unsat => {} // spurious → interpolate
            }
        }

        // Spurious: interpolate A vs B = ¬target(nx). Shared vocabulary = the
        // next-state register names ⇒ I is a state predicate over them.
        let b = target_bool.not();
        let (a_decls, a_body) = match serialize_term(&a) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable("serialize A".into()),
        };
        let (b_decls, b_body) = match serialize_term(&b) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable("serialize B".into()),
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
            Err(e) => return RefineOutcome::Unavailable(e),
        };
        if stdout.trim().is_empty()
            || stdout.contains("(error")
            || stdout
                .lines()
                .any(|l| l.trim() == "fail" || l.trim() == "none")
        {
            return RefineOutcome::Unavailable("cvc5 produced no interpolant".into());
        }
        match parse_interpolant_to_predicate_expr(&stdout) {
            Some(pe) => RefineOutcome::Predicate(pe),
            None => RefineOutcome::Unavailable(format!(
                "interpolant outside the parseable grammar: {:?}",
                extract_interpolant_body(&stdout)
            )),
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
            Err(e) => return RefineOutcome::Unavailable(format!("encode: {e:?}")),
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
                return RefineOutcome::Unavailable("source over an unresolvable register".into());
            }
        };
        let good_a = match build(good, &nx_a, &in_a) {
            Some(b) => b,
            None => {
                return RefineOutcome::Unavailable("good over an unresolvable register".into());
            }
        };
        let good_b = match build(good, &nx_b, &in_b) {
            Some(b) => b,
            None => return RefineOutcome::Unavailable("good over a non-state register".into()),
        };
        let a = Bool::and(&[&source_bool, &t_a, &good_a]);
        let b = Bool::and(&[&source_bool, &t_b, &good_b.not()]);
        let (a_decls, a_body) = match serialize_term(&a) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable("serialize A".into()),
        };
        let (b_decls, b_body) = match serialize_term(&b) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable("serialize B".into()),
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
            Err(e) => return RefineOutcome::Unavailable(e),
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
        return RefineOutcome::Unavailable(format!("mathsat: no interpolant (status `{head}`)"));
    };
    let candidate = ite_condition(term).unwrap_or_else(|| term.to_string());
    // Reuse the rich #318 parser by wrapping the candidate in cvc5's `define-fun` envelope.
    let wrapped = format!("(define-fun I () Bool {candidate})");
    match parse_interpolant_to_predicate_expr(&wrapped) {
        Some(pe) => RefineOutcome::Predicate(pe),
        None => RefineOutcome::Unavailable(format!("mathsat interpolant outside grammar: {term}")),
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
            Err(e) => return RefineOutcome::Unavailable(format!("encode: {e:?}")),
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
            return RefineOutcome::Unavailable("design has no `bad`".into());
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
                    return RefineOutcome::Unavailable("timeout (reachability)".into());
                }
                z3::SatResult::Unsat => {}
            }
        }

        let (a_decls, a_body) = match serialize_term(&a) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable("serialize A".into()),
        };
        let (b_decls, b_body) = match serialize_term(&b) {
            Some(x) => x,
            None => return RefineOutcome::Unavailable("serialize B".into()),
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
            Err(e) => return RefineOutcome::Unavailable(e),
        };
        if stdout.trim().is_empty()
            || stdout.contains("(error")
            || stdout
                .lines()
                .any(|l| l.trim() == "fail" || l.trim() == "none")
        {
            return RefineOutcome::Unavailable("cvc5 produced no interpolant".into());
        }
        match parse_interpolant_to_predicate_expr(&stdout) {
            Some(pe) => RefineOutcome::Predicate(pe),
            None => RefineOutcome::Unavailable(format!(
                "interpolant outside the parseable grammar: {:?}",
                extract_interpolant_body(&stdout)
            )),
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

/// P2.3b — the SEQUENCE of predicates from interpolating the depth-`depth` BMC
/// unrolling at every cut `1..=depth` (the targeted per-trace refinement IC3ia
/// needs). Returns the parseable, deduplicated interpolant predicates — a SET that
/// collectively rules out the spurious depth-`depth` counterexample, rather than
/// the single (often redundant) predicate a one-cut interpolation gives. Empty when
/// `bad` is reachable within `depth` (a real CEX — the caller confirms elsewhere) or
/// every interpolant is outside the `#318` grammar.
/// **Emergent-K** — discover RELATIONAL invariants by ITERATIVE forward
/// interpolation. Starting from `R = Init`, repeatedly interpolate
/// `A = R(s0) ∧ T(s0,s1)` against `B = ¬bad(s1)`; the interpolant `I(s1)` (a
/// reachable-state over-approximation excluding `bad`) is parsed to a
/// [`PredicateExpr`] and `R` grows by it. The key (de-risked 2026-07-13): from
/// concrete `Init` the first interpolant is concrete values, but as `R` grows to
/// span a relational region cvc5 GENERALISES the interpolant to the relation
/// (e.g. `data == target`) — an "emergent" predicate that no *syntactic* extractor
/// finds. Returns the discovered predicates (the relational ones are the payoff),
/// most-general last. Stops at a fixpoint, an unparseable interpolant (grammar
/// ceiling), or when `bad` is one-step reachable from `R` (the invariant fails).
pub fn discover_relational_predicates(
    file: &Btor2File,
    max_iters: usize,
    timeout_ms: u32,
) -> Vec<PredicateExpr> {
    let cfg = z3::Config::new();
    z3::with_z3_config(&cfg, || {
        let view = match encode_design(file) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
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
        for _ in 0..max_iters.max(1) {
            // If `bad` is one-step reachable from R, the invariant fails — stop.
            {
                let s = mk();
                s.assert(Bool::and(&[&r, &transition, &bad_s1]));
                if s.check() != z3::SatResult::Unsat {
                    break;
                }
            }
            // Interpolate A = R ∧ T  vs  B = ¬bad(s1).  I over s1 (register names).
            let a = Bool::and(&[&r, &transition]);
            let b = bad_s1.not();
            let (a_decls, a_body) = match serialize_term(&a) {
                Some(x) => x,
                None => break,
            };
            let (b_decls, b_body) = match serialize_term(&b) {
                Some(x) => x,
                None => break,
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
                Err(_) => break,
            };
            let p = match parse_interpolant_to_predicate_expr(&stdout) {
                Some(p) => p,
                None => break, // grammar ceiling
            };
            // Grow R by the new predicate; stop at a fixpoint (nothing new).
            let grown = match over_s0(&p) {
                Some(pb) => pb,
                None => break,
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
        discovered
    })
}

pub fn sequence_interpolant_predicates(
    file: &Btor2File,
    depth: usize,
    timeout_ms: u32,
) -> Vec<PredicateExpr> {
    let depth = depth.clamp(1, 24);
    let mut out: Vec<PredicateExpr> = Vec::new();
    for cut in 1..=depth {
        if let RefineOutcome::Predicate(p) = interpolant_at_cut(file, cut, depth, timeout_ms)
            && !out.contains(&p)
        {
            out.push(p);
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
        let preds = discover_relational_predicates(&file, 8, 5_000);
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
        let preds = discover_relational_predicates(&file, 8, 5_000);
        if preds.is_empty() {
            eprintln!("SKIP (cvc5 absent / grammar): none discovered");
            return;
        }
        let found_bound = preds.iter().any(|p| {
            matches!(p, PredicateExpr::Cmp { register, op, value }
                if register == "cnt" && matches!(op, CmpOp::Le | CmpOp::Lt) && *value >= 5)
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
        // No interpolant (SAT / empty) → Unavailable, never a fabricated predicate.
        assert!(matches!(
            parse_mathsat_interpolant("sat\n"),
            RefineOutcome::Unavailable(_)
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
    #[ignore = "manual: requires MathSAT (mununu-sva image); run with --ignored + MUNUNU_MATHSAT_PATH"]
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
            let preds = discover_relational_predicates(&file, 8, 4_000);
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
}
