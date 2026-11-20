//! R-F5 — symbolic (BDD-backed) abstraction engine.
//!
//! The explicit predicate-cube path materializes `2^|P|` cube states and builds the
//! may/must transition relation with `O(2^2|P|)` SMT queries (`adapter/btor2/kmts_lift.rs`).
//! The symbolic alternative represents state sets and the transition relation as BDDs
//! (via OxiDD) and evaluates the mu-calculus by fixpoint image/preimage, so the cube
//! space is never enumerated. See `docs/design/post-rf5-architecture.md`.
//!
//! Shipped:
//! - **R-F5.0** — the de-risking spike: symbolic KMTS + box preimage + νX fixpoint,
//!   validated cell-for-cell against the explicit `evaluate_tri` (see the tests).
//! - **R-F5.1** — [`SymbolicContext`] (the present/next BDD-var frame + minterm /
//!   set / rename helpers) and [`TritBdd`] (the BDD-backed counterpart of
//!   [`crate::mu_calculus::trit::TritSet`] — a `(must, may)` BDD pair with the pure
//!   boolean ops and a `TritSet` bridge), with an **exhaustive `TritBdd ≡ TritSet`
//!   differential**.
//! - **R-F5.2** — [`SymbolicKmts`] (built from a `Clts` via
//!   [`SymbolicKmts::from_clts`]): the may/must relation + AP labels as BDDs, and a
//!   recursive [`SymbolicKmts::evaluate`] running the mu-calculus by BDD
//!   image/preimage + μ/ν fixpoint — the symbolic counterpart of `evaluate_tri`,
//!   validated against it cell-for-cell across box/diamond/μ/ν/nested/boolean over
//!   several KMTSes.
//! - **R-F5.2b** — guarded modalities: **label** guards + **current/next
//!   state-variable** guards (`req_cur`/`forb_cur`/`req_next`/`forb_next`) as a
//!   RESTRICTED relation `R ∧ label_conjunct ∧ cur_ok(x) ∧ next_ok(x')`, validated
//!   vs `evaluate_tri`. Controllability (`ctrl`) and step-bounds (`steps`) are an
//!   honest error (R-F5.2c).
//! - **R-F5.3** — symbolic edge construction from BTOR2 (the `O(2^2|P|)`-SMT-avoidance
//!   win): `adapter/btor2/symbolic_bitblast.rs`'s `BddBitBlaster` builds `R_may`/`R_must`
//!   as an `AbstractRelation` once, without per-cube-pair SMT.
//! - **R-F5.4** — `--engine symbolic` on `btor2 cegar` / `sv cegar` / `sv verify-auto`
//!   (CLI + API); `adapter/btor2/symbolic_engine.rs::symbolic_cube_verdicts`.
//! - **R-F5.5** — the symbolic CEGAR loop (`symbolic_cegar_refine`), verify-auto wiring,
//!   compound-predicate cube dims, and state-predicate-guarded modalities in the cube
//!   evaluator.
//!
//! Remaining: R-F5.2c (controllability + step-bounded modalities — currently honest
//! errors, out of the predicate-cube fragment) and R-F5.6 (cone-of-influence restriction
//! so the bit-blast is not capped by whole-design bit count).
//!
//! 3-valued box semantics (Bruns–Godefroid; mirrors `evaluator::modal_trit_core`):
//! for `[]φ`, `must = ∀ may-successors. φ.must` and `may = ∀ must-successors. φ.may`
//! — as preimages, `box.must = ¬∃next. R_may ∧ ¬φ.must[next]` and
//! `box.may = ¬∃next. R_must ∧ ¬φ.may[next]`.

use oxidd::bdd::{self, BDDFunction, BDDManagerRef};
use oxidd::{BooleanFunction, FunctionSubst, Manager, ManagerRef, Subst, VarNo};

use crate::mu_calculus::trit::{Trit, TritSet};

/// The BDD variable frame for a symbolic abstraction over `n` predicate bits: `n`
/// present-state vars `x` and `n` next-state vars `x'`, plus the constants and the
/// helpers a symbolic engine needs (minterms, set construction, present→next rename,
/// the next-var cube for quantification). One `SymbolicContext` owns one OxiDD
/// manager; all BDDs built through it share that manager's unique table.
///
/// A concrete state / cube index `i ∈ 0..2^n` corresponds to the minterm over the
/// present vars whose bit `b` is set iff `(i >> b) & 1 == 1`.
pub struct SymbolicContext {
    _manager: BDDManagerRef,
    present: Vec<BDDFunction>,
    next: Vec<BDDFunction>,
    present_varnos: Vec<VarNo>,
    next_cube: BDDFunction,
    tt: BDDFunction,
    ff: BDDFunction,
}

impl SymbolicContext {
    /// Build a context over `num_vars` present + `num_vars` next boolean variables.
    pub fn new(num_vars: usize) -> Self {
        let manager = bdd::new_manager(1 << 16, 1 << 16, 1);
        let (present, next, tt, ff) = manager.with_manager_exclusive(|m| {
            let vars = m.add_vars(2 * num_vars as VarNo);
            let present: Vec<_> = (0..num_vars)
                .map(|i| BDDFunction::var(m, vars.start + i as VarNo).unwrap())
                .collect();
            let next: Vec<_> = (0..num_vars)
                .map(|i| BDDFunction::var(m, vars.start + (num_vars + i) as VarNo).unwrap())
                .collect();
            (present, next, BDDFunction::t(m), BDDFunction::f(m))
        });
        let present_varnos: Vec<VarNo> = (0..num_vars as VarNo).collect();
        let mut next_cube = tt.clone();
        for v in &next {
            next_cube = next_cube.and(v).unwrap();
        }
        Self {
            _manager: manager,
            present,
            next,
            present_varnos,
            next_cube,
            tt,
            ff,
        }
    }

    /// Number of present (= next) variables.
    pub fn num_vars(&self) -> usize {
        self.present.len()
    }

    /// The always-true BDD `⊤`.
    pub fn top(&self) -> &BDDFunction {
        &self.tt
    }

    /// The always-false BDD `⊥` (the empty set).
    pub fn bottom(&self) -> &BDDFunction {
        &self.ff
    }

    /// The cube of next-state variables (to quantify them out in a preimage).
    pub fn next_cube(&self) -> &BDDFunction {
        &self.next_cube
    }

    fn minterm(idx: usize, vars: &[BDDFunction], tt: &BDDFunction) -> BDDFunction {
        let mut m = tt.clone();
        for (b, v) in vars.iter().enumerate() {
            let lit = if (idx >> b) & 1 == 1 {
                v.clone()
            } else {
                v.not().unwrap()
            };
            m = m.and(&lit).unwrap();
        }
        m
    }

    /// The present-var minterm for state index `idx`.
    pub fn present_minterm(&self, idx: usize) -> BDDFunction {
        Self::minterm(idx, &self.present, &self.tt)
    }

    /// The next-var minterm for state index `idx`.
    pub fn next_minterm(&self, idx: usize) -> BDDFunction {
        Self::minterm(idx, &self.next, &self.tt)
    }

    /// The characteristic BDD (over present vars) of an explicit set of state indices.
    pub fn set_from_indices(&self, indices: impl IntoIterator<Item = usize>) -> BDDFunction {
        let mut s = self.ff.clone();
        for i in indices {
            s = s.or(&self.present_minterm(i)).unwrap();
        }
        s
    }

    /// Does state `idx` belong to the set `f`? (its present-minterm implies `f`.)
    pub fn holds_at(&self, f: &BDDFunction, idx: usize) -> bool {
        let mt = self.present_minterm(idx);
        mt.and(f).unwrap() == mt
    }

    /// Rename a present-var function to the next-var frame (for a preimage).
    pub fn to_next(&self, f: &BDDFunction) -> BDDFunction {
        let subst = Subst::new(self.present_varnos.clone(), self.next.clone());
        f.substitute(&subst).unwrap()
    }
}

/// A 3-valued state set as a `(must, may)` BDD pair over the present-state vars, with
/// the KMTS invariant `must ⊑ may`. The BDD-backed counterpart of
/// [`crate::mu_calculus::trit::TritSet`]: the pure boolean ops (`and`/`or`/`not`) and
/// convergence check (`eq_set`) mirror `TritSet` exactly (an exhaustive differential
/// pins the equivalence), but the representation is symbolic — a set of `2^|P|` cubes
/// is one BDD, not a `2^|P|`-bit vector.
#[derive(Clone)]
pub struct TritBdd {
    must: BDDFunction,
    may: BDDFunction,
}

impl TritBdd {
    /// `(must, may)` directly (debug-asserts `must ⊑ may`).
    ///
    /// SOUNDNESS (must ⊆ may fix, Tier 0) — this check stays a `debug_assert!`
    /// because it runs on the hot modal path (once per node per fixpoint
    /// iteration) and a per-node BDD `and` in release would cost real time. The
    /// **release** guarantee of the underlying invariant now comes one layer up,
    /// once per lift, from `adapter::btor2::kmts_lift::assert_must_subset_may`,
    /// which rejects a KMTS whose `R_must ⊄ R_may` with an `IrConsistencyError`
    /// before it ever reaches this constructor. Given a well-formed KMTS the
    /// operators preserve `must ⊑ may` (see §5 of docs/design/past-shadow-
    /// soundness.md), so this assert should never fire; it remains as the debug
    /// tripwire that a future operator change did not break preservation.
    pub fn from_parts(must: BDDFunction, may: BDDFunction) -> Self {
        debug_assert!(
            must.and(&may).unwrap() == must,
            "TritBdd invariant must ⊑ may"
        );
        Self { must, may }
    }

    /// `True` everywhere.
    pub fn all_true(ctx: &SymbolicContext) -> Self {
        Self {
            must: ctx.tt.clone(),
            may: ctx.tt.clone(),
        }
    }

    /// `False` everywhere.
    pub fn all_false(ctx: &SymbolicContext) -> Self {
        Self {
            must: ctx.ff.clone(),
            may: ctx.ff.clone(),
        }
    }

    pub fn must(&self) -> &BDDFunction {
        &self.must
    }

    pub fn may(&self) -> &BDDFunction {
        &self.may
    }

    /// Kleene conjunction (truth-meet): `(must ∧ must, may ∧ may)`.
    pub fn and(&self, other: &Self) -> Self {
        Self {
            must: self.must.and(&other.must).unwrap(),
            may: self.may.and(&other.may).unwrap(),
        }
    }

    /// Kleene disjunction (truth-join): `(must ∨ must, may ∨ may)`.
    pub fn or(&self, other: &Self) -> Self {
        Self {
            must: self.must.or(&other.must).unwrap(),
            may: self.may.or(&other.may).unwrap(),
        }
    }

    /// Kleene negation: swap and complement — `must' = ¬may`, `may' = ¬must` (so the
    /// `must ⊑ may` invariant is preserved, exactly as `TritSet::not`).
    pub fn not(&self) -> Self {
        Self {
            must: self.may.not().unwrap(),
            may: self.must.not().unwrap(),
        }
    }

    /// Structural equality on both BDDs — the fixpoint convergence test. (ROBDDs are
    /// canonical, so this is exact set equality, O(1) on the shared manager.)
    pub fn eq_set(&self, other: &Self) -> bool {
        self.must == other.must && self.may == other.may
    }

    /// The trit verdict at state `idx`.
    pub fn verdict_at(&self, ctx: &SymbolicContext, idx: usize) -> Trit {
        if ctx.holds_at(&self.must, idx) {
            Trit::True
        } else if ctx.holds_at(&self.may, idx) {
            Trit::Unknown
        } else {
            Trit::False
        }
    }

    /// Bridge from the explicit `TritSet` (`state_count` states over `ctx`'s vars).
    pub fn from_trit_set(ctx: &SymbolicContext, ts: &TritSet, state_count: usize) -> Self {
        let mut must = ctx.ff.clone();
        let mut may = ctx.ff.clone();
        for i in 0..state_count {
            match ts.verdict_at(i) {
                Trit::True => {
                    let m = ctx.present_minterm(i);
                    must = must.or(&m).unwrap();
                    may = may.or(&m).unwrap();
                }
                Trit::Unknown => {
                    may = may.or(&ctx.present_minterm(i)).unwrap();
                }
                Trit::False => {}
            }
        }
        Self { must, may }
    }

    /// Bridge to the explicit `TritSet` over `state_count` states (for differential
    /// testing + interop with the explicit evaluator).
    pub fn to_trit_set(&self, ctx: &SymbolicContext, state_count: usize) -> TritSet {
        use bitvec::prelude::*;
        let mut must = bitvec![usize, Lsb0; 0; state_count];
        let mut may = bitvec![usize, Lsb0; 0; state_count];
        for i in 0..state_count {
            if ctx.holds_at(&self.must, i) {
                must.set(i, true);
            }
            if ctx.holds_at(&self.may, i) {
                may.set(i, true);
            }
        }
        TritSet::from_parts(must, may)
    }
}

/// A symbolic KMTS: a [`SymbolicContext`] plus the may/must transition relation
/// (`R_may` / `R_must` BDDs over present+next vars) and the state-AP labels (each a
/// [`TritBdd`] over present vars). Built from an explicit [`Clts`] via
/// [`SymbolicKmts::from_clts`]; evaluated by [`SymbolicKmts::evaluate`], which runs
/// the mu-calculus by BDD image/preimage — the symbolic counterpart of
/// `evaluator::evaluate_tri`. (R-F5.2)
pub struct SymbolicKmts {
    ctx: SymbolicContext,
    r_may: BDDFunction,
    r_must: BDDFunction,
    labels: std::collections::HashMap<String, TritBdd>,
    /// R-F5.2b — per transition-label the `(x, x')` relation of edges carrying it
    /// (only the labels named in the formula's guards), for label-guarded modalities.
    label_edges: std::collections::HashMap<String, BDDFunction>,
    /// R-F5.2b — per state-variable the present-var set of states carrying it (only
    /// the variables named in the formula's guards), for `req_*` / `forb_*` guards.
    state_var: std::collections::HashMap<String, BDDFunction>,
    state_count: usize,
}

impl SymbolicKmts {
    /// Build the symbolic twin of an explicit `Clts` (Sharp / MayOnly edges; the AP
    /// labels for every predicate named in `formula`). State index `s.index()` maps
    /// to the present-minterm of the same index, so a symbolic verdict at index `i`
    /// lines up with `evaluate_tri`'s `verdict_at(i)`.
    pub fn from_clts<S: crate::clts::IdStorage, L: crate::clts::IdStorage>(
        clts: &crate::clts::Clts<S, L>,
        formula: &crate::mu_calculus::Formula,
    ) -> Result<Self, String> {
        use crate::clts::{TransitionModality, Tristate};
        use crate::mu_calculus::Node;

        let state_count = clts.state_count();
        let mut num_vars = 1usize;
        while (1usize << num_vars) < state_count {
            num_vars += 1;
        }
        let ctx = SymbolicContext::new(num_vars);
        let sids: Vec<_> = clts.states().collect();

        // R-F5.2b — the label / state-variable names the formula's guards reference.
        let mut needed_labels = std::collections::BTreeSet::new();
        let mut needed_vars = std::collections::BTreeSet::new();
        for node in formula.nodes() {
            if let Node::Modal { guard, .. } = node {
                for l in &guard.labels {
                    needed_labels.insert(l.clone());
                }
                for v in guard
                    .current
                    .required
                    .iter()
                    .chain(&guard.current.forbidden)
                    .chain(&guard.next.required)
                    .chain(&guard.next.forbidden)
                {
                    needed_vars.insert(v.clone());
                }
            }
        }
        let carries = |t: &crate::clts::Transition<S, L>, name: &str| -> bool {
            t.labels()
                .iter()
                .any(|lid| clts.label_bitset(*lid).is_some_and(|b| b.test(name)))
        };

        // Transition relation (+ per-needed-label edge relations).
        let mut r_may = ctx.ff.clone();
        let mut r_must = ctx.ff.clone();
        let mut label_edges: std::collections::HashMap<String, BDDFunction> = needed_labels
            .iter()
            .map(|l| (l.clone(), ctx.ff.clone()))
            .collect();
        for &sid in &sids {
            let i = sid.index();
            for t in clts.outgoing(sid) {
                let j = t.target().index();
                let e = ctx.present_minterm(i).and(&ctx.next_minterm(j)).unwrap();
                r_may = r_may.or(&e).unwrap();
                match t.modality() {
                    TransitionModality::Sharp => {
                        r_must = r_must.or(&e).unwrap();
                    }
                    TransitionModality::MayOnly => {}
                    TransitionModality::MustHyperOnly(_) => {
                        return Err(crate::adapter::owner::mununu_limitation(
                            "the symbolic evaluator does not carry hyper-must (MustHyperOnly) \
                             transitions",
                            Some("the explicit evaluator takes them"),
                        ));
                    }
                }
                for l in &needed_labels {
                    if carries(t, l) {
                        let slot = label_edges.get_mut(l).unwrap();
                        *slot = slot.or(&e).unwrap();
                    }
                }
            }
        }

        // R-F5.2b — the present-var state set for each needed state-variable.
        let mut state_var: std::collections::HashMap<String, BDDFunction> =
            std::collections::HashMap::new();
        for v in &needed_vars {
            let mut set = ctx.ff.clone();
            for &sid in &sids {
                if clts.state_variable_bitset(sid).contains(v.as_str()) {
                    set = set.or(&ctx.present_minterm(sid.index())).unwrap();
                }
            }
            state_var.insert(v.clone(), set);
        }

        // AP labels for every predicate the formula names.
        let mut names = std::collections::BTreeSet::new();
        for node in formula.nodes() {
            if let Node::Predicate(n) = node {
                names.insert(n.clone());
            }
        }
        let mut labels = std::collections::HashMap::new();
        for name in names {
            let mut must = ctx.ff.clone();
            let mut may = ctx.ff.clone();
            for &sid in &sids {
                let i = sid.index();
                match clts.state_3valued_predicate(sid, &name) {
                    Some(Tristate::KleeneT) => {
                        let m = ctx.present_minterm(i);
                        must = must.or(&m).unwrap();
                        may = may.or(&m).unwrap();
                    }
                    Some(Tristate::KleeneBot) => {
                        may = may.or(&ctx.present_minterm(i)).unwrap();
                    }
                    Some(Tristate::KleeneF) | None => {}
                }
            }
            labels.insert(name, TritBdd::from_parts(must, may));
        }

        Ok(Self {
            ctx,
            r_may,
            r_must,
            labels,
            label_edges,
            state_var,
            state_count,
        })
    }

    /// R-F5.2b — the may/must relations RESTRICTED to a guard's matching transitions:
    /// `R ∧ (⋀ label_edges[l]) ∧ cur_ok(x) ∧ next_ok(x')`, where `cur_ok` /
    /// `next_ok` are the `req_*`/`forb_*` state-variable filters over present / next
    /// vars. A required label / variable absent from the model contributes `⊥` (no
    /// edge matches → the modal is vacuous there), exactly as the explicit filter.
    fn guarded_relations(&self, guard: &crate::mu_calculus::Guard) -> (BDDFunction, BDDFunction) {
        let var_set = |name: &str| -> BDDFunction {
            self.state_var
                .get(name)
                .cloned()
                .unwrap_or_else(|| self.ctx.ff.clone())
        };
        let mut c = self.ctx.tt.clone();
        for l in &guard.labels {
            let le = self
                .label_edges
                .get(l)
                .cloned()
                .unwrap_or_else(|| self.ctx.ff.clone());
            c = c.and(&le).unwrap();
        }
        for v in &guard.current.required {
            c = c.and(&var_set(v)).unwrap();
        }
        for v in &guard.current.forbidden {
            c = c.and(&var_set(v).not().unwrap()).unwrap();
        }
        for v in &guard.next.required {
            c = c.and(&self.ctx.to_next(&var_set(v))).unwrap();
        }
        for v in &guard.next.forbidden {
            c = c
                .and(&self.ctx.to_next(&var_set(v).not().unwrap()))
                .unwrap();
        }
        (self.r_may.and(&c).unwrap(), self.r_must.and(&c).unwrap())
    }

    /// The `SymbolicContext` (for reading verdicts).
    pub fn context(&self) -> &SymbolicContext {
        &self.ctx
    }

    /// Number of (explicit) states.
    pub fn state_count(&self) -> usize {
        self.state_count
    }

    /// 3-valued box preimage of `phi` over the given may/must relations
    /// (Bruns–Godefroid). `box.must` uses `R_may`, `box.may` uses `R_must`.
    fn box_pre(&self, phi: &TritBdd, r_may: &BDDFunction, r_must: &BDDFunction) -> TritBdd {
        use oxidd::{BooleanFunctionQuant, BooleanOperator};
        let must_next = self.ctx.to_next(phi.must());
        let box_must = r_may
            .apply_exists(
                BooleanOperator::And,
                &must_next.not().unwrap(),
                self.ctx.next_cube(),
            )
            .unwrap()
            .not()
            .unwrap();
        let may_next = self.ctx.to_next(phi.may());
        let box_may = r_must
            .apply_exists(
                BooleanOperator::And,
                &may_next.not().unwrap(),
                self.ctx.next_cube(),
            )
            .unwrap()
            .not()
            .unwrap();
        TritBdd::from_parts(box_must, box_may)
    }

    /// 3-valued diamond preimage of `phi` over the given may/must relations.
    /// `dia.must` uses `R_must`, `dia.may` uses `R_may`.
    fn diamond_pre(&self, phi: &TritBdd, r_may: &BDDFunction, r_must: &BDDFunction) -> TritBdd {
        use oxidd::{BooleanFunctionQuant, BooleanOperator};
        let must_next = self.ctx.to_next(phi.must());
        let dia_must = r_must
            .apply_exists(BooleanOperator::And, &must_next, self.ctx.next_cube())
            .unwrap();
        let may_next = self.ctx.to_next(phi.may());
        let dia_may = r_may
            .apply_exists(BooleanOperator::And, &may_next, self.ctx.next_cube())
            .unwrap();
        TritBdd::from_parts(dia_must, dia_may)
    }

    /// Evaluate a mu-calculus formula symbolically → a `TritBdd` verdict over the
    /// present vars. Supports the **unguarded** fragment (True/False, predicates,
    /// `!`/`&&`/`||`, `[]`/`<>`, `mu`/`nu`); a guarded / controllability /
    /// step-bounded modality returns an error (R-F5.2b).
    pub fn evaluate(&self, formula: &crate::mu_calculus::Formula) -> Result<TritBdd, String> {
        let mut bindings = std::collections::HashMap::new();
        self.eval_node(formula, formula.root(), &mut bindings)
    }

    fn eval_node(
        &self,
        f: &crate::mu_calculus::Formula,
        id: crate::mu_calculus::NodeId,
        bindings: &mut std::collections::HashMap<crate::mu_calculus::FormulaVarId, TritBdd>,
    ) -> Result<TritBdd, String> {
        use crate::mu_calculus::{Control, Guard, ModalKind, Node};
        Ok(match f.node(id) {
            Node::True => TritBdd::all_true(&self.ctx),
            Node::False => TritBdd::all_false(&self.ctx),
            Node::Predicate(name) => self
                .labels
                .get(name)
                .cloned()
                .unwrap_or_else(|| TritBdd::all_false(&self.ctx)),
            Node::Variable(v) => bindings.get(v).cloned().ok_or_else(|| {
                crate::adapter::owner::mununu_defect(&format!(
                    "formula variable #{} is unbound in the symbolic evaluator",
                    v.index()
                ))
            })?,
            Node::Not(n) => self.eval_node(f, *n, bindings)?.not(),
            Node::And(a, b) => self
                .eval_node(f, *a, bindings)?
                .and(&self.eval_node(f, *b, bindings)?),
            Node::Or(a, b) => self
                .eval_node(f, *a, bindings)?
                .or(&self.eval_node(f, *b, bindings)?),
            Node::Modal {
                kind,
                guard,
                target,
            } => {
                // The symbolic evaluator supports the SOUND in-fragment guards: label +
                // current/next state-variable guards (R-F5.2b/5c, `guarded_relations`).
                // Controllability (`ctrl`) and step-bounds (`steps`) are DELIBERATELY
                // kept as honest errors — they are OUT-of-fragment over a predicate cube
                // (ctrl = a build-time default partition, not a real game; steps = not
                // may/must-filtered — the PO-4 gap), so evaluating them would return an
                // unsound/vacuous verdict. This mirrors the soundness boundary the cube
                // path draws via `cube_modality_soundness_warnings` (mu_calculus/mod.rs).
                if guard.control != Control::All {
                    return Err(
                        "symbolic evaluator: controllability guards (`ctrl`) are out-of-fragment \
                         over a predicate cube — use a bare `[]`/`<>` modality"
                            .into(),
                    );
                }
                if guard.max_steps.is_some() {
                    return Err(
                        "symbolic evaluator: step-bounded modalities (`steps`) are out-of-fragment \
                         over a predicate cube — use an unbounded `[]`/`<>` modality"
                            .into(),
                    );
                }
                let phi = self.eval_node(f, *target, bindings)?;
                let (r_may, r_must) = if *guard == Guard::default() {
                    (self.r_may.clone(), self.r_must.clone())
                } else {
                    self.guarded_relations(guard)
                };
                match kind {
                    ModalKind::Box => self.box_pre(&phi, &r_may, &r_must),
                    ModalKind::Diamond => self.diamond_pre(&phi, &r_may, &r_must),
                }
            }
            Node::Mu { var, body } => self.fixpoint(f, *var, *body, bindings, false)?,
            Node::Nu { var, body } => self.fixpoint(f, *var, *body, bindings, true)?,
        })
    }

    /// Kleene iteration for a least (`greatest=false`, from ⊥) or greatest
    /// (`greatest=true`, from ⊤) fixpoint. Convergence is exact set equality
    /// (`eq_set`) — ROBDDs are canonical.
    fn fixpoint(
        &self,
        f: &crate::mu_calculus::Formula,
        var: crate::mu_calculus::FormulaVarId,
        body: crate::mu_calculus::NodeId,
        bindings: &mut std::collections::HashMap<crate::mu_calculus::FormulaVarId, TritBdd>,
        greatest: bool,
    ) -> Result<TritBdd, String> {
        let mut x = if greatest {
            TritBdd::all_true(&self.ctx)
        } else {
            TritBdd::all_false(&self.ctx)
        };
        loop {
            bindings.insert(var, x.clone());
            let next = self.eval_node(f, body, bindings)?;
            if next.eq_set(&x) {
                bindings.remove(&var);
                return Ok(next);
            }
            x = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxidd::{BooleanFunctionQuant, BooleanOperator};

    use crate::clts::{Clts, DefaultLabelIdx, DefaultStateIdx, TransitionModality, Tristate};
    use crate::mu_calculus::evaluator::{Environment, evaluate_tri};
    use crate::mu_calculus::parser;

    // ---- R-F5.1: TritBdd ≡ TritSet differential ---------------------------------

    /// The `code`-th trit-set over `n` states in base-3 (digit 0=False, 1=⊥, 2=True).
    fn trit_set_from_base3(code: usize, n: usize) -> TritSet {
        use bitvec::prelude::*;
        let mut must = bitvec![usize, Lsb0; 0; n];
        let mut may = bitvec![usize, Lsb0; 0; n];
        let mut c = code;
        for i in 0..n {
            let d = c % 3;
            c /= 3;
            if d == 2 {
                must.set(i, true);
                may.set(i, true);
            } else if d == 1 {
                may.set(i, true);
            }
        }
        TritSet::from_parts(must, may)
    }

    /// Exhaustively check that `TritBdd`'s and/or/not agree with `TritSet` on every
    /// trit-set (and every pair) over a small state count.
    #[test]
    fn tritbdd_matches_tritset_exhaustive() {
        let n = 2; // 2 predicate bits → 4 states → 3^4 = 81 trit-sets
        let ctx = SymbolicContext::new(n);
        let states = 1usize << n;
        let total = 3usize.pow(states as u32); // one of {F, ⊥, T} per state
        let sets: Vec<TritSet> = (0..total).map(|c| trit_set_from_base3(c, states)).collect();

        for a in &sets {
            let ta = TritBdd::from_trit_set(&ctx, a, states);
            // round-trip
            assert!(
                ta.to_trit_set(&ctx, states).eq_set(a),
                "TritBdd round-trips through TritSet"
            );
            // not (TritBdd inherent .not() vs TritSet's `!` operator)
            assert!(
                ta.not().to_trit_set(&ctx, states).eq_set(&!a.clone()),
                "not() agrees"
            );
            for b in &sets {
                let tb = TritBdd::from_trit_set(&ctx, b, states);
                assert!(
                    ta.and(&tb)
                        .to_trit_set(&ctx, states)
                        .eq_set(&a.clone().and(b)),
                    "and() agrees"
                );
                assert!(
                    ta.or(&tb)
                        .to_trit_set(&ctx, states)
                        .eq_set(&a.clone().or(b)),
                    "or() agrees"
                );
            }
        }
    }

    #[test]
    fn tritbdd_eq_set_is_canonical() {
        let ctx = SymbolicContext::new(2);
        let a = TritBdd::from_trit_set(&ctx, &trit_set_from_base3(17, 4), 4);
        let a2 = TritBdd::from_trit_set(&ctx, &trit_set_from_base3(17, 4), 4);
        let b = TritBdd::from_trit_set(&ctx, &trit_set_from_base3(18, 4), 4);
        assert!(a.eq_set(&a2), "same content ⇒ eq_set (ROBDD canonical)");
        assert!(!a.eq_set(&b), "different content ⇒ not eq_set");
    }

    // ---- R-F5.0 spike: symbolic fixpoint ≡ evaluate_tri -------------------------

    /// A minimal symbolic KMTS built on a [`SymbolicContext`]: a may/must relation
    /// (over present+next vars) from an explicit edge list `(src, dst, is_must)`.
    struct SymKmts {
        ctx: SymbolicContext,
        r_may: BDDFunction,
        r_must: BDDFunction,
    }

    impl SymKmts {
        fn new(state_bits: usize, edges: &[(usize, usize, bool)]) -> Self {
            let ctx = SymbolicContext::new(state_bits);
            let mut r_may = ctx.bottom().clone();
            let mut r_must = ctx.bottom().clone();
            for &(src, dst, is_must) in edges {
                let e = ctx
                    .present_minterm(src)
                    .and(&ctx.next_minterm(dst))
                    .unwrap();
                r_may = r_may.or(&e).unwrap();
                if is_must {
                    r_must = r_must.or(&e).unwrap();
                }
            }
            SymKmts { ctx, r_may, r_must }
        }

        /// 3-valued box preimage of `phi`.
        fn box_pre(&self, phi: &TritBdd) -> TritBdd {
            // box.must = ¬∃next. R_may ∧ ¬φ.must[next]
            let must_next = self.ctx.to_next(phi.must());
            let box_must = self
                .r_may
                .apply_exists(
                    BooleanOperator::And,
                    &must_next.not().unwrap(),
                    self.ctx.next_cube(),
                )
                .unwrap()
                .not()
                .unwrap();
            // box.may = ¬∃next. R_must ∧ ¬φ.may[next]
            let may_next = self.ctx.to_next(phi.may());
            let box_may = self
                .r_must
                .apply_exists(
                    BooleanOperator::And,
                    &may_next.not().unwrap(),
                    self.ctx.next_cube(),
                )
                .unwrap()
                .not()
                .unwrap();
            TritBdd::from_parts(box_must, box_may)
        }

        /// Greatest fixpoint `νX. (p ∧ []X)` — the AGp safety verdict.
        fn nu_p_and_box(&self, p: &TritBdd) -> TritBdd {
            let mut x = TritBdd::all_true(&self.ctx);
            loop {
                let next = p.and(&self.box_pre(&x));
                if next.eq_set(&x) {
                    return next;
                }
                x = next;
            }
        }
    }

    fn explicit_verdicts(edges: &[(usize, usize, bool)], p: &[Tristate], n: usize) -> Vec<Trit> {
        let mut b = Clts::<DefaultStateIdx, DefaultLabelIdx>::builder();
        for i in 0..n {
            b.state(format!("s{i}"));
        }
        b.initial("s0");
        let step = b.labels().intern(["step"]).unwrap();
        let ids: Vec<_> = (0..n)
            .map(|i| b.state_id_or_insert(format!("s{i}")).unwrap())
            .collect();
        for (i, &verdict) in p.iter().enumerate() {
            b.with_3valued_predicate(ids[i], "p", verdict);
        }
        for &(src, dst, is_must) in edges {
            let modality = if is_must {
                TransitionModality::Sharp
            } else {
                TransitionModality::MayOnly
            };
            b.transition_ids_with_modality(ids[src], &[step], ids[dst], modality);
        }
        let clts = b.build().expect("explicit KMTS builds");
        let formula = parser::parse("nu X. p and [] X").expect("formula parses");
        let env = Environment::new(clts.state_count());
        let verdict = evaluate_tri(&formula, &clts, &env).expect("evaluate_tri");
        (0..n).map(|i| verdict.verdict_at(i)).collect()
    }

    fn symbolic_verdicts(
        state_bits: usize,
        edges: &[(usize, usize, bool)],
        p: &[Tristate],
        n: usize,
    ) -> Vec<Trit> {
        let k = SymKmts::new(state_bits, edges);
        let mut p_must = k.ctx.bottom().clone();
        let mut p_may = k.ctx.bottom().clone();
        for (i, &verdict) in p.iter().enumerate() {
            let mt = k.ctx.present_minterm(i);
            match verdict {
                Tristate::KleeneT => {
                    p_must = p_must.or(&mt).unwrap();
                    p_may = p_may.or(&mt).unwrap();
                }
                Tristate::KleeneBot => {
                    p_may = p_may.or(&mt).unwrap();
                }
                Tristate::KleeneF => {}
            }
        }
        let pbdd = TritBdd::from_parts(p_must, p_may);
        let x = k.nu_p_and_box(&pbdd);
        (0..n).map(|i| x.verdict_at(&k.ctx, i)).collect()
    }

    #[test]
    fn spike_sharp_only_matches_evaluate_tri() {
        let edges = &[(0, 1, true), (1, 0, true), (2, 2, true)];
        let p = &[Tristate::KleeneT, Tristate::KleeneT, Tristate::KleeneF];
        let sym = symbolic_verdicts(2, edges, p, 3);
        let exp = explicit_verdicts(edges, p, 3);
        assert_eq!(
            exp,
            vec![Trit::True, Trit::True, Trit::False],
            "explicit AGp"
        );
        assert_eq!(sym, exp, "symbolic == evaluate_tri (Sharp-only)");
    }

    #[test]
    fn spike_may_edge_bottoms_match_evaluate_tri() {
        let edges = &[(0, 1, true), (1, 0, true), (2, 2, true), (0, 2, false)];
        let p = &[Tristate::KleeneT, Tristate::KleeneT, Tristate::KleeneF];
        let sym = symbolic_verdicts(2, edges, p, 3);
        let exp = explicit_verdicts(edges, p, 3);
        assert_eq!(
            exp,
            vec![Trit::Unknown, Trit::Unknown, Trit::False],
            "explicit AGp with the MayOnly edge"
        );
        assert_eq!(sym, exp, "symbolic == evaluate_tri (may/must split)");
    }

    #[test]
    fn oxidd_smoke_boolean_algebra() {
        let ctx = SymbolicContext::new(1);
        let x = ctx.present_minterm(1); // the var is true
        let nx = x.not().unwrap();
        assert!(x.and(&nx).unwrap() == *ctx.bottom(), "x ∧ ¬x = ⊥");
        assert!(x.or(&nx).unwrap() == *ctx.top(), "x ∨ ¬x = ⊤");
    }

    // ---- R-F5.2: SymbolicKmts evaluator ≡ evaluate_tri (unguarded fragment) -----

    fn build_clts(
        edges: &[(usize, usize, bool)],
        preds: &[(&str, &[Tristate])],
        n: usize,
    ) -> Clts<DefaultStateIdx, DefaultLabelIdx> {
        let mut b = Clts::builder();
        for i in 0..n {
            b.state(format!("s{i}"));
        }
        b.initial("s0");
        let step = b.labels().intern(["step"]).unwrap();
        let ids: Vec<_> = (0..n)
            .map(|i| b.state_id_or_insert(format!("s{i}")).unwrap())
            .collect();
        for (name, verdicts) in preds {
            for (i, &v) in verdicts.iter().enumerate() {
                b.with_3valued_predicate(ids[i], *name, v);
            }
        }
        for &(src, dst, is_must) in edges {
            let m = if is_must {
                TransitionModality::Sharp
            } else {
                TransitionModality::MayOnly
            };
            b.transition_ids_with_modality(ids[src], &[step], ids[dst], m);
        }
        b.build().expect("clts builds")
    }

    fn assert_symbolic_matches_tri(
        clts: &Clts<DefaultStateIdx, DefaultLabelIdx>,
        formula_str: &str,
    ) {
        let formula = parser::parse(formula_str).expect("formula parses");
        let env = Environment::new(clts.state_count());
        let tri = evaluate_tri(&formula, clts, &env).expect("evaluate_tri");
        let sym = SymbolicKmts::from_clts(clts, &formula).expect("symbolic model");
        let sv = sym.evaluate(&formula).expect("symbolic evaluate");
        for i in 0..clts.state_count() {
            assert_eq!(
                sv.verdict_at(sym.context(), i),
                tri.verdict_at(i),
                "state {i}, formula `{formula_str}`"
            );
        }
    }

    #[test]
    fn rf5_2_symbolic_evaluator_matches_tri() {
        use Tristate::{KleeneBot as B, KleeneF as F, KleeneT as T};
        // K1 — p-true 2-cycle {s0,s1} + p-false self-loop s2 (all Sharp).
        let k1 = build_clts(
            &[(0, 1, true), (1, 0, true), (2, 2, true)],
            &[("p", &[T, T, F]), ("q", &[F, F, T])],
            3,
        );
        // K2 — K1 + a MayOnly edge s0→s2 (poisons the box at s0).
        let k2 = build_clts(
            &[(0, 1, true), (1, 0, true), (2, 2, true), (0, 2, false)],
            &[("p", &[T, T, F]), ("q", &[F, F, T])],
            3,
        );
        // K3 — a 4-state chain with a MayOnly shortcut + a KleeneBot label.
        let k3 = build_clts(
            &[
                (0, 1, true),
                (1, 2, true),
                (2, 3, true),
                (3, 3, true),
                (1, 3, false),
            ],
            &[("p", &[T, T, F, T]), ("q", &[F, B, T, F])],
            4,
        );

        let formulas = [
            "[] p",
            "<> p",
            "not p",
            "p and q",
            "p or q",
            "not (p or q)",
            "nu X. p and [] X", // AG p
            "mu X. p or <> X",  // EF p
            "nu X. (p or q) and [] X",
            "nu X. (not p) and [] X",             // AG ¬p
            "mu X. q or <> X",                    // EF q
            "nu Y. (mu X. (q or <> X)) and [] Y", // AG EF q (nested νμ)
        ];

        for clts in [&k1, &k2, &k3] {
            for f in &formulas {
                assert_symbolic_matches_tri(clts, f);
            }
        }
    }

    // ---- R-F5.2b: guarded modalities (labels + current/next state-vars) ---------

    #[allow(clippy::type_complexity)]
    fn build_guarded_clts(
        edges: &[(usize, &str, usize, bool)],
        preds: &[(&str, &[Tristate])],
        vars: &[(&str, &[usize])],
        n: usize,
    ) -> Clts<DefaultStateIdx, DefaultLabelIdx> {
        let mut b = Clts::builder();
        for i in 0..n {
            b.state(format!("s{i}"));
        }
        b.initial("s0");
        let ids: Vec<_> = (0..n)
            .map(|i| b.state_id_or_insert(format!("s{i}")).unwrap())
            .collect();
        let mut label_ids = std::collections::HashMap::new();
        for &(_, lname, _, _) in edges {
            label_ids
                .entry(lname.to_string())
                .or_insert_with(|| b.labels().intern([lname]).unwrap());
        }
        for (name, verdicts) in preds {
            for (i, &v) in verdicts.iter().enumerate() {
                b.with_3valued_predicate(ids[i], *name, v);
            }
        }
        for (i, &id) in ids.iter().enumerate() {
            let vs: Vec<&str> = vars
                .iter()
                .filter(|(_, sts)| sts.contains(&i))
                .map(|(name, _)| *name)
                .collect();
            if !vs.is_empty() {
                b.with_variables_for_state(id, vs);
            }
        }
        for &(src, lname, dst, is_must) in edges {
            let lid = label_ids[lname];
            let m = if is_must {
                TransitionModality::Sharp
            } else {
                TransitionModality::MayOnly
            };
            b.transition_ids_with_modality(ids[src], &[lid], ids[dst], m);
        }
        b.build().expect("guarded clts builds")
    }

    #[test]
    fn rf5_2b_guarded_modalities_match_tri() {
        use Tristate::{KleeneBot as B, KleeneF as F, KleeneT as T};
        // s0 --a--> s1, s1 --b--> s2, s0 --a(MayOnly)--> s2, s2 --a--> s2.
        // vars: `hot` on {s1,s2}, `cold` on {s0}. preds p, q.
        let k = build_guarded_clts(
            &[
                (0, "a", 1, true),
                (1, "b", 2, true),
                (0, "a", 2, false),
                (2, "a", 2, true),
            ],
            &[("p", &[T, T, F]), ("q", &[F, B, T])],
            &[("hot", &[1, 2]), ("cold", &[0])],
            3,
        );
        let formulas = [
            "[ labels = {a} ] p",
            "< labels = {a} > p",
            "[ labels = {b} ] p",
            "< labels = {b} > q",
            "[ req_next = {hot} ] p",
            "< req_next = {hot} > q",
            "[ req_cur = {cold} ] p",
            "[ forb_next = {hot} ] p",
            "[ labels = {a}, req_next = {hot} ] p",
            "nu X. p and [ labels = {a} ] X",
            "mu X. q or < labels = {a} > X",
            // unguarded still works alongside guarded
            "nu X. p and [] X",
        ];
        for f in &formulas {
            assert_symbolic_matches_tri(&k, f);
        }
    }

    #[test]
    fn rf5_2c_control_and_step_guards_are_rejected() {
        let clts = build_guarded_clts(&[(0, "a", 0, true)], &[("p", &[Tristate::KleeneT])], &[], 1);
        // Controllability guard — R-F5.2c.
        let ctrl = parser::parse("[ ctrl = controllable ] p").expect("parses");
        let sym = SymbolicKmts::from_clts(&clts, &ctrl).expect("model");
        assert!(
            sym.evaluate(&ctrl).is_err(),
            "controllability guard must be an honest error (R-F5.2c), not a wrong verdict"
        );
        // Step-bounded modality — R-F5.2c.
        let steps = parser::parse("< ( steps <= 2 ) > p").expect("parses");
        let sym2 = SymbolicKmts::from_clts(&clts, &steps).expect("model");
        assert!(
            sym2.evaluate(&steps).is_err(),
            "step-bounded modality must be an honest error (R-F5.2c)"
        );
    }
}
