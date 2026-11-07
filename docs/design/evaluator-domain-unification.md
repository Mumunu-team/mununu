# Evaluator unification over a bulk `Domain` trait (IR-track P2)

> Status: planning (IR-unification track, P2). Design note; the implementation lands in
> P2.2 (rewire 2v, perf-gated — **SHIPPED 2026-06-19**) → P2.3 (rewire 3v — **SHIPPED 2026-06-19**)
> → P2.4 (retire the dead per-element `truth_domain`). Companion: `docs/design/sts-ir.md` (P0/P1),
> `docs/design/kmts-theory.md` §4.
>
> **P2.2 shipped notes.** The trait is named `EvalDomain` (private) with markers `BoolDom` /
> `KleeneDom`. It lives in `evaluator.rs` **co-located with `EvalContext`**, not in a separate
> `domain.rs` — the ctx-dependent trait methods (`modal_image`, `memo_*`, `top`/`bottom`/`not`)
> call `EvalContext`'s private methods, so a separate module would force exposing ~10 internals
> crate-wide; encapsulation won over the file-name suggestion below. Gate met: 2083/2083 crate
> tests pass (verdict-equivalence) + criterion "No change in performance detected" on
> `mu_calculus_evaluate` |S|=2048 and |S|=8192. Record: `P2-2-2026-06-19.md` (a planning note, not tracked).

## 1. Why

The mu-calculus evaluator ships **two parallel bodies** in
[`crates/mununu-core/src/mu_calculus/evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs):

- `eval_node` (2-valued) — representation `BitVec<usize, Lsb0>` (a set of states; bit set =
  property holds), bulk-bitwise lattice ops. `eval_fixpoint` for μ/ν.
- `eval_node_tri` (3-valued Kleene) — representation `TritSet` = a **pair** of BitVecs
  (`must_true`, `may_true`) with `must ⊆ may`. `eval_fixpoint_tri` for μ/ν.

P2's goal is to collapse them into **one generic body** so the two never drift and the 3-valued
path inherits any 2-valued optimisation — **without regressing the BitVec hot path**.

### The premise correction (the finding that shaped this design)

The original P2 framing was "unify the evaluator over the existing `truth_domain::TruthDomain`
trait." An audit found that trait is the **wrong vehicle**:

- `mu_calculus::truth_domain::{TruthDomain, BoolDomain, KleeneDomain}` is pub-exported but used
  **only by its own unit tests** — `evaluator.rs` has zero references to it. It is a complete,
  tested **R.1 design artifact that the R.3 implementation bypassed.**
- It is **per-element** (`type Element; truth_join(&Element, &Element)`). The evaluator is
  **bulk** (whole-state-set BitVec ops). Routing the evaluator through `Vec<Element>` per-state
  loops would gut the BitVec hot path — failing P2's HARD zero-perf-regression gate.

So P2 defines a **new bulk trait** (`Domain`, below) whose associated `Valuation` type is the
whole-state-set representation (`BitVec` for 2v, `TritSet` for 3v), and **retires the dead
per-element `truth_domain`** in P2.4.

## 2. What the two bodies already share (so the merge is smaller than it looks)

Both bodies route through the SAME machinery — only the wrappers differ:

- The entire modal Skolem kernel: `modal_bits_from_target` → `modal_exists` / `modal_forall` →
  `group_transitions_by_uncontrollable_labels`, `transition_target_in_set_{diamond,box}`,
  `eval_modal_bounded`. The 2v path calls it via `eval_modal` (single pass, witness-recording);
  the 3v path calls it **twice** via `modal_bits_from_target` (two filtered passes, no witnesses).
- `predicate_bits` (the 2v predicate/atom lift, incl. the 3-valued-predicate bridge); the 3v path
  wraps its output in `TritSet::from_predicate`.
- Guard partitions, `not_oob_bits` / `oob_bits` masks, `EvaluationOptions`
  (`prior_approximants`, `on_fixpoint_convergence`, `use_partitions`), `fixpoint_kind_to_polarity`.

The genuine divergences a single body must reconcile are narrow:

1. **`Not`.** 2v: bitwise complement + AND `not_oob_bits` (re-clear OOB). 3v:
   `must(¬X) = ¬may(X)`, `may(¬X) = ¬must(X)` — a **must/may swap**, NOT a per-half complement.
   *(`trit.rs::not`)* — the single hardest unification.
2. **Modal filter dispatch.** 2v always `TransitionModalityFilter::All`. 3v picks per (kind, half):
   Diamond → `(must: MustOnly, may: All)`; Box → `(must: All, may: MustOnly)` (R.6.3).
3. **Memoization** — `eval_node` only (the cache is `HashMap<NodeId, BitVec>`); `eval_node_tri`
   has none.
4. **Witness / iteration-rank recording** — `eval_node` only (strategy extraction is meaningless
   under Kleene). The 3v modal passes `NodeId(0)` and records nothing.
5. **OOB semantics** — 2v: OOB masked OUT of every positive set (held bottom). 3v: OOB in `may`
   but not `must` (held Unknown). Lives entirely inside each impl's constructors.

## 3. The `Domain` trait (bulk; one generic body over it)

```rust
// As shipped: in crates/mununu-core/src/mu_calculus/evaluator.rs (P2.2),
// co-located with EvalContext (NOT a separate domain.rs — see the status note
// at the top of this file). The trait is private and named `EvalDomain`;
// constructors (`top`/`bottom`/`from_predicate`/`from_binding`) take `&EvalContext`
// so each impl reads the masks/state-count it needs.
//
// `Valuation` is the whole-state-set representation. The generic
// `eval_node_generic<D: Domain>` is monomorphised to BitVec (Bool) and
// TritSet (Kleene); BitVec bulk-bitwise ops are preserved verbatim under
// monomorphisation (no Vec<Element> per-state fallback).
pub trait Domain {
    type Valuation: Clone;

    // --- lattice corners + lifts (wrap existing inherent ops) ---
    fn bottom(n: usize) -> Self::Valuation;                          // ∅ / all-False
    fn top(n: usize, oob: &BitVec) -> Self::Valuation;              // all / all-True (OOB-aware)
    fn from_predicate(bits: BitVec, oob: &BitVec) -> Self::Valuation; // lift predicate_bits output
    fn from_binding(b: Option<&Self::Valuation>, n: usize) -> Self::Valuation;

    // --- boolean ops (PERF-CRITICAL: must monomorphise to bulk bitwise) ---
    fn and(a: Self::Valuation, b: &Self::Valuation) -> Self::Valuation;
    fn or(a: Self::Valuation, b: &Self::Valuation) -> Self::Valuation;
    fn not(a: Self::Valuation, oob: &BitVec) -> Self::Valuation;     // <-- absorbs the 2v-mask vs 3v-swap divergence

    // --- modal step (PERF-CRITICAL: the preimage kernel) ---
    // Whole-valuation: the impl owns the filter dispatch (3v two-pass) + the
    // witness decision (2v records, 3v no-ops). The shared inner Skolem kernel
    // (modal_exists/modal_forall) stays as free EvalContext methods both impls call.
    fn modal_image<S: IdStorage, L: IdStorage>(
        ctx: &mut EvalContext<'_, S, L>,
        kind: ModalKind,
        guard: &Guard,
        target: &Self::Valuation,
        modal_node_id: NodeId,
    ) -> Result<Self::Valuation, EvaluationError>;

    // --- fixpoint support ---
    fn fixpoint_eq(a: &Self::Valuation, b: &Self::Valuation) -> bool;
    fn seed_from_prior(pa: &PriorApproximant, kind: FixpointKind) -> Option<Self::Valuation>;
    fn approximant_view<'v>(v: &'v Self::Valuation, polarity, iter) -> ApproximantView<'v>;

    // --- 2v-only capabilities, no-op defaults for 3v ---
    const MEMOISED: bool;                                            // true for Bool, false for Kleene
    fn memo_get(ctx: &EvalContext<'_, S, L>, node: NodeId) -> Option<Self::Valuation> { None }
    fn memo_store(ctx: &mut EvalContext<'_, S, L>, node: NodeId, v: &Self::Valuation) {}
    fn record_iteration_ranks(ctx, var, prev, next, iter) {}        // 2v only
}

struct BoolDom;    // Valuation = BitVec<usize, Lsb0>
struct KleeneDom;  // Valuation = TritSet
```

> Naming (final, P2.2): the trait is `EvalDomain`; the markers are `BoolDom` / `KleeneDom`.
> This avoids collision with both the dead `truth_domain::{BoolDomain, KleeneDomain}` (retired in
> P2.4) and the unrelated `abstraction::domains::BoolDomain`.

### Generic body — the 9 `Node` arms

```rust
fn eval_node_generic<D: Domain>(&mut self, node, bindings: &HashMap<Var, D::Valuation>)
    -> Result<D::Valuation, EvaluationError>
{
    if D::MEMOISED && bindings.is_empty() && let Some(hit) = D::memo_get(self, node) { return Ok(hit); }
    let out = match self.formula.node(node) {
        True        => D::top(n, &self.oob_bits),
        False       => D::bottom(n),
        Predicate(p)=> D::from_predicate(self.predicate_bits(p)?, &self.oob_bits),
        Variable(v) => D::from_binding(bindings.get(v), n),
        Not(x)      => D::not(self.eval_node_generic::<D>(x, bindings)?, &self.oob_bits),
        And(l,r)    => D::and(self.eval(l)?, &self.eval(r)?),
        Or(l,r)     => D::or(self.eval(l)?, &self.eval(r)?),
        Modal{k,g,t}=> { let tv = self.eval(t)?; D::modal_image(self, k, g, &tv, node)? }
        Mu{v,b}     => self.eval_fixpoint_generic::<D>(v, b, Least, bindings)?,
        Nu{v,b}     => self.eval_fixpoint_generic::<D>(v, b, Greatest, bindings)?,
    };
    if D::MEMOISED && bindings.is_empty() && !node.is_fixpoint() { D::memo_store(self, node, &out); }
    Ok(out)
}
```

`eval_fixpoint_generic<D>` mirrors today's two fixpoints: seed via `D::seed_from_prior` (else
`D::bottom`/`D::top`), iterate `next = eval_node_generic::<D>(body)`, converge on
`D::fixpoint_eq`, callback via `D::approximant_view`, rank-record via
`D::record_iteration_ranks` (no-op for Kleene).

The existing `eval_node` / `eval_node_tri` become thin wrappers:
`eval_node = eval_node_generic::<BoolDom>`, `eval_node_tri = eval_node_generic::<KleeneDom>`.

## 4. Decomposition + gates

| Step | Scope | Gate |
|---|---|---|
| **P2.1** (this note) | Design: the `Domain` trait, the generic-body sketch, the divergence-handlers, the decomposition. | — |
| **P2.2** ✅ SHIPPED | Defined `EvalDomain` + `BoolDom` (Valuation = BitVec) + `eval_node_generic` + `eval_fixpoint_generic`; rewired `eval_node`/`eval_fixpoint` to delegate to `::<BoolDom>`; deleted the now-dead `bitwise_{and,or,not}` / `variable_bits` (→ `BoolDom` ops) + the standalone `eval_fixpoint`; `eval_modal` → `eval_modal_with_target_set`. `eval_node_tri` untouched. | **PASSED.** `cargo test -p mununu-core` = 2083/2083 (verdict-equivalence); criterion `mu_calculus_evaluate` |S|=2048 (+0.80%, p=0.09) and |S|=8192 (−1.64%, p=0.45) both "No change in performance detected". |
| **P2.3** ✅ SHIPPED | Added `KleeneDom` (Valuation = TritSet, `MEMOISED = false`); `eval_node_tri` now delegates to `::<KleeneDom>`; deleted the hand-written 3v `eval_node_tri` match + `eval_fixpoint_tri` (divergences now in `KleeneDom`: `not` = must/may swap, `modal_image` = the two filtered passes, `seed_from_prior` = both halves). | **PASSED.** `cargo test -p mununu-core` = 2083/2083 incl. `r3_kleene_baseline` projection invariant; trit benches vs `p23-pre` — `shared_subexpr` −2…−4%, `modal_dense` no change (the dense/64 first-run "+18.57%" was a wide-CI noise outlier, confirmed no-change on isolated recheck), `fixpoint_invariant` −0.3…−2%. Record: `P2-3-2026-06-19.md` (a planning note, not tracked). |
| **P2.4** | Retire the dead per-element `truth_domain` module (`TruthDomain`/`BoolDomain`/`KleeneDomain` + tests); update `docs/design/native-sv-abstraction.md` §6.4 + `kmts-theory.md` §4 anchors to point at the bulk `Domain`. | `cargo test` + `/docs-traceability`. |

Each step is its own PR. P2.2 is the load-bearing, riskiest one (the model-checking hot path); it
ships only when the 2v perf benches show no regression vs `main`.

## 5. Behaviour-preservation rules (non-negotiable)

- **Target the current two-pass modal kernel** (`modal_bits_from_target`), NOT the staged
  `#[allow(dead_code)]` R.6 single-pass `modal_trit_from_target` / `modal_trit_core`. P2 is
  behaviour-preserving; pulling the R.6 single-pass forward is a separate, gated change.
- **Preserve bounded-modality behaviour exactly** — including the known modality-blind 3v
  over/under-claim under `guard.max_steps` (the R.6.3.b follow-up corner). Do not "fix" it here.
- **No new trit memoisation** — keep parity (2v memoised, 3v not). Adding a trit memo is scope
  creep (bench R-A1 already showed it does not pay off at current scales).
- **Witness/strategy extraction stays 2v-only** — `BoolDom::modal_image` records, `KleeneDom`
  no-ops; the generic body threads `modal_node_id` but the impl decides.
- **Clone discipline** — pass `&Valuation` into trait ops (no clone into the call); preserve the
  2v↔3v boundary clone-elision the team already tracks.

## 6. Perf-gate procedure (P2.2 / P2.3)

```bash
# baseline on main
git checkout main && cargo bench --bench mu_calculus -- --save-baseline p2-pre
# candidate
git checkout <p2-branch> && cargo bench --bench mu_calculus -- --baseline p2-pre
```

P2.2 keeps iff `mu_calculus_evaluate` (|S|=2048 and 8192) is within criterion noise of `p2-pre`
(the HARD gate — a measured regression blocks the merge, per the §6.7 benchmark-validation
discipline). P2.3 keeps iff the trit groups are within noise. The measurement record lands at
`P2-` (a planning note, not tracked)<step>-<date>.md`.

## 7. Risk register (from the evaluator map)

1. **`Not` must/may swap** — get `D::not` right first; it is a whole-valuation transform, not a
   per-half complement. 2v = complement + OOB mask; 3v = swap.
2. **Modal filter asymmetry** — keep `modal_image` whole-valuation so the 3v two-filter walk
   never leaks must/may concepts into the 2v path.
3. **Monomorphisation must not add indirection** — confirm `BoolDom::and` lowers to the same
   `bitand_assign` (no `dyn`, no per-element fallback); the |S|=8192 bench is the proof.
4. **OOB threading** — `top` / `from_predicate` / `not` all take `oob`; the divergent OOB
   semantics live in the impls; easy to get backwards — covered by verdict-equivalence.
5. **Fixpoint seed reuse (R.5)** — `seed_from_prior` takes `FixpointKind`; 2v reads only
   `must_true`, 3v reads both halves. Preserve per-impl.
