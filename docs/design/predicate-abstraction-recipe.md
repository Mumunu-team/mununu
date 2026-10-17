# Predicate-Abstraction Recipe — KMTS Lifter Operational Guide

> **Status: live (R.5 + R.6 shipped, 2026-06).** Practical recipe for predicate seeding, may/must image computation, CEGAR refinement, and operational debugging in mununu's BTOR2 → KMTS lifter. The cube CEGAR loop, the may/must predicate-image, UF wrapping, Craig interpolation, and the controllability-aware lift are all live; the cube-verdict soundness story (§4.9) is anchored to live code. Companions: [`native-sv-abstraction.md`](native-sv-abstraction.md) (architecture; §6 is the design framing), [`kmts-theory.md`](kmts-theory.md) (theoretical foundations), [`predicate-abstraction-worked-example.md`](predicate-abstraction-worked-example.md) (one module carried end to end — RTL → BTOR2 → coarse `KleeneBot` → interpolant → refined `KleeneT`). §4.9 carries a `> Source of truth:` anchor; the fine-grained refinement-heuristic sub-sections (§4.4–§4.8 lemma library, two-axis partitioning) document the intended shape and may still outrun the shipped implementation in places — those retain unanchored caveats.

## §1 What predicate abstraction is

### §1.1 The Graf–Saidi construction

Predicate abstraction (Graf and Saidi, CAV 1997, *Construction of Abstract State Graphs with PVS*) takes a concrete transition system `M = (S, R, V)` and a finite set of *predicates* `P = {p_1, …, p_k}` over the concrete state space, and constructs an *abstract* transition system `M^# = (S^#, R^#, V^#)` whose states are predicate cubes (Boolean valuations over `P`). Each abstract state `b ∈ S^#` corresponds to the set of concrete states satisfying `b`'s predicate cube: `γ(b) = { s ∈ S : ∀ p_i ∈ P. b(p_i) = p_i(s) }`. There are at most `2^k` abstract states; in practice many cubes are unreachable or inconsistent and the reachable abstract state space is much smaller than `2^k`.

The construction's central operation is the *predicate-image* computation: given a source abstract state `b` and a candidate target `b'`, decide whether there exist concretisations `s ∈ γ(b)`, `s' ∈ γ(b')` with `(s, s') ∈ R`. This is an SMT query over the concrete transition relation extended with the predicate definitions; it is the algorithmic core of the recipe.

### §1.2 Relationship to KMTS

A predicate-abstracted model is naturally a KMTS, not a plain abstract transition system. The two relations:

```text
R_may(b, b')   ⟺   ∃ s ∈ γ(b), s' ∈ γ(b'). (s, s') ∈ R    (existential SMT query)
R_must(b, b')  ⟺   ∀ s ∈ γ(b). ∃ s' ∈ γ(b'). (s, s') ∈ R   (universal-existential SMT query)
```

with the invariant `R_must ⊆ R_may` holding by construction. The 3-valued AP labelling is

```text
L(b, q) = T   if ∀ s ∈ γ(b). q(s)
L(b, q) = F   if ∀ s ∈ γ(b). ¬q(s)
L(b, q) = ⊥   otherwise
```

for each formula atomic proposition `q` (which need not be in `P` — the predicate set `P` defines the abstract state space; the formula AP set defines the labelling).

This connection was made explicit by Cleaveland and Steffen (1993) for general abstract interpretation and by Godefroid–Huth–Jagadeesan (2001) for the specific KMTS-as-predicate-abstraction reading. Mununu's BTOR2 lifter materialises both relations using the two SMT query modes of §3.

### §1.3 What's hard about predicate abstraction in practice

Three operational difficulties dominate, and the rest of this doc is structured around them:

1. **Where do the initial predicates come from?** Bad seeding produces an abstraction that returns `⊥` everywhere; good seeding produces a useful first-pass abstraction the user can refine. §2.
2. **The predicate-image SMT queries are the slow path.** Wide multipliers, dividers, and bit-blast-heavy arithmetic kill QF_BV solvers. The §3 two-mode strategy (concrete operators for must-mode; UF abstraction for may-mode) is the operational answer.
3. **CEGAR refinement can oscillate or under-refine.** Picking the *right* predicate to add at each refinement step — and distinguishing predicate refinement from operator (UF) concretisation — is non-trivial. §4 describes the two-axis CEGAR algorithm and its bounded-refinement defaults.

A fourth difficulty, *compositional tightness* (§5), shows up only in multi-module settings.

## §2 Predicate seeding strategies

A predicate set `P` must be a *finite* set of Boolean-valued expressions over the concrete state space. Good `P`s have predicates that discriminate the property's atomic propositions, capture control-state structure, and equate connected ports across module boundaries. Bad `P`s have either too few predicates (returning `⊥` everywhere) or too many (blowing up the predicate-image computation time).

### §2.1 Source 1 — formula atomic propositions

Every distinct sub-expression of the property formula that evaluates to a Boolean becomes a predicate. For a formula `□ (req == 1 ⇒ ◇ (ack == 1))`, the predicates are `req == 1` and `ack == 1`. The lifter walks the formula AST, collects atomic propositions of the form `reg == constant`, `reg < constant`, `reg ∈ {…}`, `reg & mask == constant`, and any Boolean-typed register reference, and seeds them into `P`.

This is *necessary* but not *sufficient* — a predicate set that only contains formula APs may produce `⊥` verdicts because the abstract model lacks enough state to track relevant intermediate behaviour. Source 2 handles intermediate-state coverage.

### §2.2 Source 2 — COI register-equality predicates

For each constant the property's cone-of-influence (§5 of the architecture doc) syntactically references, add `reg == constant` to `P` for the corresponding register. This generalises today's [`scan_significant_constants`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs) (which today seeds `Discover` abstractions from syntactic constants in guards); under the new lifter, syntactic constants in *any* expression in the COI become predicates.

This is the load-bearing source for FSMs that use bit-encoded states (e.g. `state == 3'b101`). Without these predicates the abstract model collapses every state-equality check to `⊥`, and any safety property mentioning a particular state value returns `⊥`.

### §2.3 Source 3 — typedef-enum membership

For each typedef-enum register `e` in the COI with variants `{V_1, …, V_n}`, add `n` predicates `is_V_1`, …, `is_V_n` where `is_V_i = (e == V_i)`. This replaces the soon-to-be-deleted [`fsm.rs`](../../crates/mununu-core/src/adapter/systemverilog/fsm.rs) typedef-enum extractor — the predicate set carries the same information (state-variant membership) without needing a separate FSM-aware code path.

The lifter reads typedef-enum information from BTOR2 metadata (Yosys preserves `(* enum_value_… *)` attributes through `setundef` and `write_btor`). Where BTOR2 lacks the metadata, the user supplies it via the sidecar.

### §2.4 Source 4 — user-supplied sidecar predicates

A new `predicates: Vec<MuFormula>` field per module (post-S.3 schema, replacing the variant-based `SignalAbstraction` enum). Each entry is `{ name: String, formula: MuFormula }`. The formula uses the existing mu-calculus AP grammar plus the module's signal names.

Examples:

```json
{
  "predicates": [
    { "name": "boot_in_unsafe",         "formula": "boot_fsm ∈ {5, 6, 7}" },
    { "name": "fifo_full",              "formula": "fill_count == 4" },
    { "name": "axi_addr_in_ctrl_range", "formula": "awaddr ∈ {32'h1000..32'h1FFF}" }
  ]
}
```

User-supplied predicates are the primary way to express *intent-specific* abstractions — predicates derived from the engineer's understanding of the design's invariants, not from syntactic patterns. They are checked for syntactic well-formedness and signal-name resolution but not for semantic redundancy with auto-derived predicates (Sources 1–3) — duplication is harmless beyond a small redundant-image cost.

### §2.5 Source 5 — CEGAR refinement (§4 below)

When the KleeneDomain evaluator returns `KleeneBot` on a property, refinement adds predicates derived from the spuriousness check's UNSAT core. These predicates are not chosen by the user; they are *interpolants* extracted from the SMT proof that the abstract counterexample does not concretise. §4 walks the algorithm.

### §2.6 Anti-patterns

- **Adding `reg < constant` predicates for every register and every constant in the design** — combinatorial blow-up; almost always the wrong choice. Restrict to constants in the COI.
- **Adding predicates over wires the property does not observe** — useless; the lifter prunes these but the prune step itself costs time.
- **Adding predicates over the output of a UF-wrapped operator** (`reg == f_mul(a, b)` when `f_mul` is uninterpreted) — the predicate evaluates to `⊥` everywhere, contributing no information. Move the wrap to a more strategic location (e.g. `f_mul`'s inputs) or concretise the operator at that location.
- **Adding more than ~20 predicates per module without monitoring image-query time** — the predicate image queries grow as `2^|P|` worst case; >20 predicates per module is a code smell unless verified to scale.

## §3 Predicate-image computation via SMT — two modes

The §1.2 KMTS construction requires two SMT queries per (`b`, `b'`) pair: existential for `R_may`, universal-existential for `R_must`. They have different operational profiles. §3a covers must-mode (concrete operators, accurate, slow); §3b covers may-mode (UF-abstracted operators, fast, less informative).

### §3a Must-mode predicate-image (concrete operators)

**Query shape.** For each candidate must-edge `(b, b')`:

```text
∀ s ⊨ b. ∃ s' ⊨ b'. (s, s') ∈ R_concrete
```

In SMT form, expanded for a transition relation given by the BTOR2 `next` nodes for each state register:

```text
∀ s : (∧_{p_i ∈ P} (b(p_i) ↔ p_i(s)))
  ⇒ ∃ s' : (∧_{p_i ∈ P} (b'(p_i) ↔ p_i(s'))) ∧ next(s, s')
```

The `∀` quantifier is the operational difficulty. Modern QF_BV solvers (z3, bitwuzla, cvc5) do not natively support quantifiers in QF; the encoding strategies are:

1. **Skolemise.** Move the `∀` to a Skolem function on the universally-quantified variables and ask the dual existential. Works in EUF + QF_BV; loses precision for variables with non-trivial dependencies.
2. **Bounded universal elimination.** Enumerate the at most `2^|P|` cubes for `s` and ask one existential per cube. Tractable for small `|P|` (the typical case); blows up for large predicate sets.
3. **CEGAR-on-the-quantifier.** Start with the existential `∃s, s'. s ⊨ b ∧ s' ⊨ b' ∧ next(s, s')` (cheap; over-approximates the universal). On a positive answer, verify the universal by checking the complement — `∃s. s ⊨ b ∧ ∀s'. s' ⊭ b' ∨ ¬next(s, s')`. The complement is a single-quantifier query.

Mununu's lifter adopts strategy (2) for `|P| ≤ 8` per module and (3) for larger predicate sets. Strategy (1) is reserved for the case where the lifter detects EUF symbols in the query (i.e. some operators in `next` were UF-wrapped per §3b) — Skolemisation interacts cleanly with EUF.

**Restricted to candidates from the may set.** A must-edge can exist only where a may-edge exists (`R_must ⊆ R_may` invariant). The lifter computes `R_may` first (cheap; §3b), then iterates over the may-edges and asks the must-query only for candidate must-edges. This bounds the number of must-queries to `|R_may|`, not `|S^#|^2`.

**Soundness.** Must-mode queries *must* use concrete operators (no UF abstraction). A must-edge with a UF-witnessed successor is unsound — UF admits behaviours the concrete operator does not, so a "∀s, ∃s' under UF" witness does not transfer to "∀s, ∃s' under concrete." The lifter enforces this by switching the UF wrapper off for must-mode queries even if the sidecar declares UF wrapping for the operator.

### §3b May-mode predicate-image (UF-abstracted operators)

**Query shape.** For each candidate may-edge `(b, b')`:

```text
∃ s ⊨ b. ∃ s' ⊨ b'. (s, s') ∈ R_UF
```

where `R_UF` is `R_concrete` with the wide-arithmetic cells (per §6.10 of the architecture doc: `$mul`, `$div`, `$mod`, `$pow` unconditionally; `$add`/`$sub` for width > 32) replaced by uninterpreted function symbols with the single axiom of functional consistency (`f(x) = f(x)`).

**Why UF is sound for may.** The UF-abstracted relation admits *more* behaviours than the concrete (because functional consistency is the *only* axiom — `f_mul(2, 3) = f_mul(2, 3) = 6` is not derivable). So if no `s, s'` exists with `(s, s') ∈ R_UF`, certainly no such pair exists under `R_concrete`. A negative may-mode query under UF safely concludes "no may-edge here." A positive may-mode query under UF only claims "there exists *some* UF-consistent witness," which is a sound over-approximation of "there exists a concrete witness" — the UF witness may not correspond to a concrete one, but the may-edge is still admissible because abstraction may add edges.

**Why UF is fast.** EUF + linear-bitvector queries are dramatically faster than QF_BV with wide arithmetic. A multiplier on 32-bit operands has `~2^64` SAT space for a concrete `$mul`; the UF abstraction reduces this to a constraint over the *outputs* of `f_mul(a, b)` modulo functional consistency, which is essentially free.

**Default wrapping policy.** Per the architecture doc §6.10:
- Unconditionally wrap: `$mul`, `$div`, `$mod`, `$pow`.
- Wrap if width > 32 bits: `$add`, `$sub`.
- Never wrap: `$and`, `$or`, `$xor`, `$not` (already cheap for SMT).
- User overrides via sidecar `uf_wrap` (force-wrap specific cell instances) or `uf_unwrap` (force-concretise).

**Yosys lockdown** (per architecture doc §3.4.1). Yosys's `opt_share` and `opt_muxtree` decompose wide arithmetic into shift-and-add networks if allowed. If `$mul` becomes a network of `$add`s before BTOR2 emission, the UF wrapper has nothing to wrap. Mitigation: `keep_hierarchy` on macro instances containing UF-wrapped cells; skip `opt_share`/`opt_muxtree` for modules declaring UF wrapping.

### §3c Cost and termination

Per-module image computation cost (worst case): `O(|S^#|^2)` may-queries + `O(|R_may|)` must-queries. With `|S^#| ≤ 2^|P|`, this is `O(2^(2|P|))` may-queries — exponential in the predicate count but typically heavily bounded by reachability pruning (most predicate cubes are unreachable from the initial abstract state).

Termination is straightforward: the image computation is a fixed-point over the may set seeded by the initial abstract states, iterating until no new abstract state is discovered. Bounded by `2^|P|`.

### §3d The unified lift entry (eager / lazy / compound routing)

> Source of truth: [`lift_predicate_cube`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L2392) + [`ensure_compound_lift_supported`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L2319) — surface: (CLI+API+UI) via `mununu btor2 cegar` / `POST /api/v1/btor2/cegar` / the `/cegar` panel.

Both the may-mode (§3b) and must-mode (§3a) images are reached through **one** gated entry, `lift_predicate_cube(predicates, btor2, opts, lift_opts, strategy)`, which the CEGAR loop and the sv-yosys verify orchestrator call. It runs the compound soundness gate once, then dispatches:

- `LiftStrategy::Eager` → `predicate_cube_lift` (materialises `2^|P|` cubes; the full-fidelity path; the only path that honours **compound predicates** via the `SmtAllPairs` seam).
- `LiftStrategy::Lazy` → `LazyLift` + `materialize_clts_from_lazy` (per-cube on-demand; **identical CLTS to Eager** on the sampling path — asserted by the `u1_eager_lazy_differential_equivalence` test; does not support compounds).

The two strategies share one per-cube sampling body (`cube_sampling_edges`) and one sampled-target must post-pass (`apply_sampled_must_inference`), so they cannot diverge. **Compound rule:** a non-empty `compound_exprs` requires `Eager` + `may = SmtAllPairs` — the sampling representative inverse can't realise compound atoms (e.g. `a==0 && b==0`), and the lazy body never consults `compound_exprs`; `ensure_compound_lift_supported` rejects the unsupported combinations at the entry.

## §4 CEGAR loop with two-axis refinement

When the KleeneDomain evaluator returns `KleeneBot` on a property, the abstract model is too coarse to give a definite verdict. CEGAR (Clarke, Grumberg, Jha, Lu, Veith — CAV 2000, *Counterexample-Guided Abstraction Refinement*) responds by extracting a refinement signal from the abstract counterexample and adding predicates to the model. The mununu lifter's CEGAR loop has two distinguishing features: it operates over a 3-valued verdict (instead of the original 2-valued setting), and it refines on *two axes* (predicate set + UF wrapping set) rather than one.

### §4.1 Algorithm sketch

```text
refine(model M, formula φ, max_rounds K):
    for round in 1..K:
        verdict, cex = evaluate_3valued(M, φ)
        if verdict in {KleeneT, KleeneF}:
            return verdict, cex          # done
        # verdict == KleeneBot — refine
        assert cex is an abstract counterexample (lasso or finite prefix)
        spurious_proof = discharge_concretely(cex)
        if spurious_proof is SAT:
            return KleeneF, cex          # real counterexample → property fails
        # spurious_proof is UNSAT — extract refinement signal
        core = unsat_core(spurious_proof)
        (new_preds, new_concretisations) = partition_core(core)
        if new_preds.is_empty() and new_concretisations.is_empty():
            warn("refinement stalled; verdict remains KleeneBot")
            return KleeneBot, cex
        M = M.with_predicates(M.predicates + new_preds)
                .with_uf_concretised(M.uf_concretised + new_concretisations)
    warn("refinement cap reached at round K; verdict remains KleeneBot")
    return KleeneBot, cex
```

Steps in detail below.

### §4.2 Abstract counterexample lifting

For a safety formula (`νX. (φ ∧ □X)` returning `KleeneBot`), the abstract counterexample is a *finite prefix* — a sequence of abstract states `b_0, b_1, …, b_n` with `b_0 ∈ S_0^#`, every step `(b_i, b_{i+1}) ∈ R_may^#` (note: not `R_must^#` — we are tracing through may-edges to find the trace), and `b_n` failing the safety invariant (`⟦φ⟧_M(b_n) = KleeneF`, or the abstract trace exhibits the failure under refinement).

For a liveness formula (`μX. (φ ∨ ◇X)` returning `KleeneBot`), the abstract counterexample is a *lasso* — a finite prefix `b_0, …, b_k` followed by a loop `b_k, b_{k+1}, …, b_k` along may-edges, such that no state in the loop has `⟦φ⟧_M = KleeneT`.

In both cases, the trace traverses *may*-edges (the over-approximation may admit edges the concrete does not). The next step is discharging concretely.

### §4.3 Concrete discharge — is the abstract counterexample spurious?

Construct an SMT query asking whether the abstract trace has a concrete witness:

```text
∃ s_0, s_1, …, s_n :
    s_0 ⊨ b_0 ∧ initial(s_0)
  ∧ ∀ i ∈ [0, n). (s_i, s_{i+1}) ∈ R_concrete ∧ s_{i+1} ⊨ b_{i+1}
  ∧ violation(s_n)
```

with `R_concrete` *without* UF abstraction (concrete operators only). For the lasso case, add a loop-closure constraint `s_n = s_k`.

Two outcomes:

- **SAT** (with model `(s_0, …, s_n)`). The abstract counterexample concretises to a real counterexample. Return `KleeneF` and the concrete trace as the counterexample witness.
- **UNSAT** (with proof). The abstract counterexample is spurious — it admits no concrete witness. Extract the UNSAT core (the minimal subset of constraints that suffices to derive False) and partition it per §4.4.

### §4.4 Unsat-core partitioning — predicate refinement vs UF concretisation

The unsat core is a set of constraints from the spuriousness query. Each constraint mentions specific BTOR2 cells / register identifiers / predicate definitions / UF instances. The partition rule:

- **Bitvector constants in the core**: state-distinguishability gap. Some pair of abstract states the trace traverses are concretely distinct but the predicate set merges them. Refinement: add a predicate that distinguishes them. The interpolation step (§4.5) extracts a candidate predicate from the core.
- **UF instance terms** (`f_mul(…)`, `f_div(…)`) **in the core**: operator-behaviour gap. The UF abstraction admits a witness for an operator output that the concrete operator does not produce. Refinement: either (a) concretise that specific UF instance (drop the UF wrapper for the cell instance and re-query); or (b) add a *learned-lemma* axiom (e.g. `f_mul(a, 0) = 0`, `f_mul(a, 1) = a`) without full concretisation.

The partition can also detect *both* — a core that mentions both bitvector constants and UF terms indicates a combined gap; the refinement strategy is to refine on both axes simultaneously (add a predicate AND concretise the UF instance, or add a predicate AND a learned-lemma axiom).

### §4.5 Interpolation (the predicate-extraction step)

When the partition indicates predicate refinement is needed, the lifter extracts a candidate predicate via interpolation. Given the UNSAT proof `π` of `A ∧ B ⊨ ⊥`, an *interpolant* is a formula `I` over the *shared variables* of `A` and `B` with `A ⊨ I` and `I ∧ B ⊨ ⊥`. For CEGAR, `A` is the trace prefix up to the first abstract state where the predicate set is too coarse; `B` is the trace suffix; `I` is a predicate that, when added to `P`, suffices to rule out the spurious trace.

Cimatti, Griggio, Mover, and Tonetta (TACAS 2014, *IC3 Modulo Theories via Implicit Predicate Abstraction*) developed the IC3-IA recipe — applying interpolation in the IC3/PDR setting to extract predicates that are *both* useful for ruling out the current spurious trace *and* likely to generalise to other potential spurious traces. The lifter adopts the IC3-IA strategy for predicate extraction.

For interpolant computation, mununu uses z3's `interpolant` API where available, or falls back to a hand-rolled craig-interpolant extraction from z3's UNSAT proof (slow but always available). The first interpolant typically suffices; if it does not (the refined model still returns `KleeneBot` on the same trace), iterate one more round with a different interpolation seed.

### §4.6 UF concretisation strategy

When the partition indicates UF refinement, the lifter has two choices:

1. **Selective concretisation**: drop the UF wrapper for the offending cell instance only. The next image-computation cycle uses concrete operators for that instance. Cost: re-query may and must edges for every successor of states that referenced the de-wrapped instance.
2. **Learned-lemma addition**: add an axiom about the UF symbol without de-wrapping. Common lemmas: `f_mul(a, 0) = 0`; `f_mul(a, 1) = a`; `f_add(a, 0) = a`; `f_add(a, b) = f_add(b, a)` (commutativity); `f_mul(a, b) = f_mul(b, a)` (commutativity); `f_xor(a, a) = 0`. The lifter maintains a per-operator lemma library (Andraus and Sakallah, LPAR 2008 *Reveal — A Formal Verification Tool for Verilog Designs*); the unsat core indicates which lemma to add.

**Default heuristic**: try learned-lemma addition first (cheap; lemma library is small); fall back to selective concretisation when the lemma library is exhausted or the core indicates a non-axiomatic gap (e.g. specific input/output pairing not derivable from the lemma library).

### §4.7 Bounded refinement and stall handling

Default cap: 16 rounds per `(property, module)` pair. Configurable per-module via sidecar `cegar_max_rounds`. On cap-hit, the lifter keeps the verdict as `KleeneBot` and emits a soundness-tagged warning naming the cap, the trace, and the predicate/UF refinement history.

**Stall detection**: if a round produces neither a new predicate nor a new UF concretisation (the partition returns empty), the loop is genuinely stuck — the predicate language is insufficient for the property. The lifter reports `KleeneBot` with an enhanced warning suggesting (a) user-supplied predicates, (b) richer SMT theory (e.g. arrays for memory-cell models), or (c) the property is concretely undecidable for the current abstraction language.

### §4.8 Soundness annotations

Every fallback in the refinement loop carries a `// SOUNDNESS:` annotation in the lifter source:

- Spurious-discharge SAT → `KleeneF` verdict (sound by construction; concrete witness was found).
- Spurious-discharge UNSAT → predicate/UF refinement (sound by construction; the original abstract trace is removed by the refined model).
- Refinement-cap hit → `KleeneBot` verdict (sound; the abstract model is reported as-is).
- Stall → `KleeneBot` verdict with diagnostic (sound; same as cap-hit but with a different cause).

The architecture doc §6.9 catalogues these as part of the broader soundness story.

### §4.9 The audited-sound cube fragment

> Source of truth: [`predicate_cube_lift`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L2440) + [`evaluate_tri`](../../crates/mununu-core/src/mu_calculus/evaluator.rs#L792) + [`cube_modality_soundness_warnings`](../../crates/mununu-core/src/mu_calculus/mod.rs#L284) — surface: (CLI+API+UI) via `mununu btor2 cegar` / `POST /api/v1/btor2/cegar` / the `/cegar` panel.

A `KleeneT` / `KleeneF` verdict on the predicate cube is only *sound* — i.e.
guaranteed to transfer to the concrete RTL by the §4.5 preservation theorem
([`kmts-theory.md`](kmts-theory.md#L216)) — for one fragment of the modal
mu-calculus: **`Control::All`, bare (label-agnostic), unbounded** modalities.
This is the slice the `btor2 cegar` / verification path rides on (M.4's
`<> (p5 || p6 || p7)` is exactly this shape). The fragment is not a limitation
to apologise for: a synchronous design has *one clock = one step* with the input
quantified inside the modality (`EX φ` = ∃-input-successor ⊨ φ; `AX φ` = ∀), so
the bare `<>` / `[]` are the *complete* modal vocabulary for synchronous
verification. Label-discriminated modalities belong to the asynchronous /
process-algebra world (the explicit-CLTS path), not the cube.

**The soundness chain is mechanised in CI** (the 2026-06-23 cube-modal audit):

1. **The lift is a sound may/must KMTS** — `may ⊇ concrete` (over-approximation,
   the §4.5 may-step-accommodation premise) and `must ⊆ concrete`
   (under-approximation, the must-step-preservation premise). Established by a
   differential test against an independent concrete oracle
   ([`simulate_one_step`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs)):
   [`po1_cube_brackets_concrete_*`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L3105).
2. **The evaluator computes the §4.3 semantics** — the production `KleeneDom`
   modal step equals the Bruns–Godefroid 3-valued modal definition for every
   `{Sharp, MayOnly} × {T, F}` edge configuration. Established by an enumerated
   conformance test:
   [`po5_kleene_modal_matches_bruns_godefroid_4_3`](../../crates/mununu-core/src/mu_calculus/evaluator.rs#L4950).
3. **Compose (1) + (2)**: a sound KMTS in, §4.3 out ⇒ §4.5 transfers a definite
   cube verdict to the concrete design. This is what makes M.4 *provably* sound,
   not merely argued.

**Out-of-fragment forms are gated, not silently answered.**
[`cube_modality_soundness_warnings`](../../crates/mununu-core/src/mu_calculus/mod.rs#L284)
(wired into the CEGAR loop at
[`cegar.rs`](../../crates/mununu-core/src/adapter/btor2/cegar.rs#L720), surfaced
on the `warnings` channel across all three surfaces) emits a soundness warning —
*not* a hard reject, so V.6's controllability-aware cube keeps working — for:

| Modal form over a cube | Why warned | Obligation |
|---|---|---|
| `ctrl = controllable \| environment` | semantics is now **sound** (per-player de Alfaro–Godefroid–Jagadeesan LICS 2004 rule, **PO-3 / R.6.8 CLOSED 2026-06-23** — `evaluate_tri` routes the controllability arms through `modal_trit_from_target`); the advisory remains because over a *plain verification* cube the controllability partition is a build-time default (the lone `step` label is `Controllable`-by-default) and is meaningful only over a controllability-aware (R.6.6) cube | none (advisory only; sound) |
| bounded `steps = k` | the may/must filter is not applied to bounded modal steps | PO-4 / R.6.3.b |
| label-specific on a non-cube label | the cube collapses every concrete action onto its own label(s) ⇒ vacuous (`<step>` over a single-`step` cube `==` bare ⇒ no warning) | (expressiveness boundary; no proof possible) |

**Honest verdict semantics (Claims Integrity).** Because `may` over-approximates,
the cube can return `KleeneBot` where a finer abstraction would decide — and a
`KleeneBot` is the *correct* sound answer, not a failure. M.4 illustrates the
discipline: the `{p5, p6, p7}` cube *detects* the CWE-1245 hazard (pre_fix
`T=7 ⊥=0`, definite) and *shows the fix removes the definite hazard* (post_fix
`T=4 ⊥=3`), but it does **not** prove the fixed FSM safe — proving safety would
need a finer abstraction / CEGAR to convergence. A definite-safe claim on the
coarse cube would be unsound.

## §5 Port-equality heuristic for compositional tightness

This section reproduces the architecture doc §7.2 worked counterexample in full, with the sidecar diff and the verify.toml.

### §5.1 The failure mode

Two modules connected by a shared net + a data bus. Each module is independently predicate-abstracted with predicates over its local state and the shared net's value. On composition, the per-module predicate sets do not include a cross-module equality between the data bus's driver-side and consumer-side values; the composed KMTS admits the spurious behaviour where the consumer reads data different from what the producer drove. Safety formulas mentioning consumer-observed data return `KleeneBot`.

### §5.2 The worked example — `multi_producer_consumer_top`

[`examples/systemverilog/multi_producer_consumer_top.sv`](../../examples/systemverilog/multi_producer_consumer_top.sv) instantiates [`multi_producer.sv`](../../examples/systemverilog/multi_producer.sv) and [`multi_consumer.sv`](../../examples/systemverilog/multi_consumer.sv) with a shared `valid` net and a 4-bit `data` bus. Property pair: `□ (consumer.received ⇒ producer.sent)` (safety) and `□ ◇ consumer.received` (liveness, under fairness).

**Initial sidecar (insufficient):**

```json
{
  "$schema": "mununu_sv_annotation_v2",
  "module": "multi_producer_consumer_top",
  "modules": [
    {
      "name": "producer",
      "predicates": [
        { "name": "p_valid_low",  "formula": "valid == 0" },
        { "name": "p_valid_high", "formula": "valid == 1" },
        { "name": "p_count_zero", "formula": "count == 0" },
        { "name": "p_count_pos",  "formula": "count > 0" }
      ]
    },
    {
      "name": "consumer",
      "predicates": [
        { "name": "c_valid_low",  "formula": "valid == 0" },
        { "name": "c_valid_high", "formula": "valid == 1" },
        { "name": "c_count_zero", "formula": "count == 0" },
        { "name": "c_count_pos",  "formula": "count > 0" }
      ]
    }
  ]
}
```

Verdict on composed KMTS: safety = `KleeneBot`. Trace: producer's `MayOnly` transition on `(valid=1, data=k)` synchronizes with consumer's `MayOnly` transition on `(valid=1, data=k)` for *any* `k` — neither side carries a predicate relating `producer.data_out` to `consumer.data_in`, so the composed transition admits the cross-data spurious behaviour.

**Refined sidecar (one new predicate):**

```json
{
  "$schema": "mununu_sv_annotation_v2",
  "module": "multi_producer_consumer_top",
  "modules": [...as above...],
  "composition": {
    "predicates": [
      {
        "name": "data_eq_on_handshake",
        "formula": "valid ⇒ producer.data_out == consumer.data_in"
      }
    ]
  }
}
```

Verdict: safety = `KleeneT`, liveness = `KleeneT` (under fairness).

### §5.3 Why this works

The new predicate constrains the composed transition relation across the module boundary: any composed transition where `valid == 1` AND `producer.data_out != consumer.data_in` is now ruled out as a may-edge (the predicate evaluates to `KleeneF`, eliminating the spurious successor). The safety formula's atomic proposition `consumer.received` is now witnessed by a must-successor on every may-edge that satisfies the predicate; the verdict graduates from `KleeneBot` to `KleeneT`.

### §5.4 Generalising — the auto-emit heuristic

The lifter automatically emits canonical port-equality predicates for every declared multi-module connection. For each connection `from: "producer.data_out", to: "consumer.data_in"`, the lifter adds a composition-level predicate `data_eq_on_handshake = (handshake_signal ⇒ producer.data_out == consumer.data_in)`, where `handshake_signal` is inferred from the connection's `valid`/`ready`-style signal pair if present, or set to `true` otherwise (the unconditional equality).

Authors only need to add predicates manually for:
- Arbitrated buses (multiple drivers selected by an arbiter signal).
- Stateful intermediates (FIFOs, buffers, retiming stages that decouple driver and consumer in time).
- Cross-domain bridges (CDC synchronisers) where the equality is only eventual, not cycle-by-cycle.

### §5.5 What the heuristic does *not* solve

The port-equality heuristic addresses the cross-module *data-flow* tightness gap but not deeper gaps:

- **Multi-cycle protocols** (e.g. AXI with response IDs that pair requests with responses across many cycles) require predicates over the protocol state machines, not just port equality. The user supplies these.
- **Latency-sensitive properties** (e.g. "request gets a response within 10 cycles") require predicates over latency counters, which is a different abstraction story (often best handled by extending the property formula with a bounded counter or `s_eventually[<10]` operator).

## §6 Operational guide

### §6.1 Authoring a sidecar with predicates

Start minimal:

1. Run the lifter on a fresh design with no sidecar (just `module`, `source`). It produces a verdict using auto-derived predicates (Sources 1–3 of §2). Inspect the verdict.
2. If `KleeneBot`, look at the produced verdict's `refinement_trace` (the lifter emits one whenever the verdict is not `KleeneT`/`KleeneF`). The trace names the predicates the auto-derivation added and the abstract state where the verdict went `KleeneBot`.
3. Inspect the abstract counterexample (also emitted with `KleeneBot` verdicts). Look for what the predicate set does *not* distinguish — usually a control-state intermediate or a data-flow value not captured by the formula's APs.
4. Add 1–2 sidecar predicates targeting that gap. Re-run.
5. If the verdict graduates to `KleeneT`/`KleeneF`, you are done. If it stays `KleeneBot`, look at the new refinement trace and iterate.

Most fixtures stabilise within 3–5 sidecar predicates beyond the auto-derived set.

### §6.2 Reading a refinement trace

The lifter emits refinement traces as JSON to `<output>/refinement_trace.json`. Structure:

```json
{
  "verdict": "KleeneBot",
  "rounds": [
    {
      "round": 1,
      "verdict": "KleeneBot",
      "abstract_counterexample": { /* lasso or prefix */ },
      "spurious_check": "UNSAT",
      "unsat_core_partition": {
        "predicate_constants": ["boot_fsm == 3", "boot_fsm == 4"],
        "uf_instances": []
      },
      "refinement": {
        "predicates_added": [{ "name": "boot_in_init", "formula": "boot_fsm == 3" }],
        "uf_concretised": []
      }
    },
    /* ...further rounds... */
  ],
  "final_verdict": "KleeneT"
}
```

Read top-down:
- The `verdict` field tells you the final verdict (KleeneT/KleeneF/KleeneBot).
- Each round entry says what the verdict was at that round, what the abstract counterexample looked like, whether spurious-discharge passed, what the UNSAT core partitioned to, and what refinement was added.
- A `KleeneBot` final verdict means the refinement cap was hit or stalled; check the last round's `refinement` to see if it produced empty additions (stall) or non-empty additions (cap-hit).

### §6.3 Debugging a non-terminating CEGAR run

Symptoms: the lifter runs for the full `cegar_max_rounds` and returns `KleeneBot`.

Causes and fixes (in order of likelihood):

1. **The predicate language cannot express the necessary distinction.** Check the last round's interpolant; if it is over signals not in the cone of the formula, the formula needs a property-level refactor (e.g. add the missing observation to the property). If the interpolant repeats from earlier rounds, the CEGAR loop is oscillating — add the cycle-breaking predicate manually.
2. **A wide-arithmetic operator needs concretisation, not UF.** Check the last round's `unsat_core_partition` — if `uf_instances` is non-empty and the lifter chose learned-lemma additions, override the sidecar to force concretisation (`uf_unwrap: ["instance_name"]`).
3. **The abstract counterexample is real and the spurious-discharge keeps returning SAT.** Should not happen — SAT should cause the verdict to switch to `KleeneF` immediately. If it does happen, there is a bug in the spurious-discharge query construction; capture the query and file an issue.
4. **The predicate-image computation is timing out at each round.** Check the per-query SMT timeout (`MUNUNU_KMTS_SMT_TIMEOUT_MS`, default 30 s). Increase for genuinely large designs; reduce the predicate set if many predicates are auto-derived and unused.

### §6.4 Inspecting the may/must split

For a given verified KMTS, the lifter emits a summary JSON to `<output>/kmts_summary.json`:

```json
{
  "module": "uart_tx",
  "abstract_states": 14,
  "transitions": {
    "sharp": 23,
    "may_only": 5
  },
  "predicates": [
    { "name": "tx_idle",      "from": "auto", "source": "property AP" },
    { "name": "tx_busy",      "from": "auto", "source": "property AP" },
    { "name": "baud_at_zero", "from": "sidecar", "source": "user" }
  ]
}
```

The `transitions.may_only` count is the operational signal of abstraction quality: lower is tighter. If the count is high (>50% of transitions), consider adding predicates that lift may-only edges to sharp.

## §7 Reading list

### §7.1 Foundations of predicate abstraction

1. **S. Graf and H. Saidi, *Construction of Abstract State Graphs with PVS*** (CAV 1997, LNCS 1254). The original predicate-abstraction construction. The §1.1 definition is from this paper.
2. **E. M. Clarke, O. Grumberg, S. Jha, Y. Lu, and H. Veith, *Counterexample-Guided Abstraction Refinement*** (CAV 2000, LNCS 1855). The original CEGAR loop. Mununu's §4 algorithm is the 3-valued / two-axis adaptation of this recipe.
3. **T. Ball and S. K. Rajamani, *The SLAM Project: Debugging System Software via Static Analysis*** (POPL 2002). The first industrial-scale CEGAR-with-predicate-abstraction system. Less directly relevant to mununu (SLAM targeted C source, not RTL) but instructive for the operational scaling story.

### §7.2 SMT-driven predicate-image and CEGAR

4. **A. Cimatti, A. Griggio, S. Mover, and S. Tonetta, *IC3 Modulo Theories via Implicit Predicate Abstraction*** (TACAS 2014, LNCS 8413). The IC3-IA recipe for interpolation-based predicate discovery. §4.5's interpolation step adopts this recipe.
5. **R. E. Bryant, S. M. German, and M. N. Velev, *Exploiting Positive Equality in a Logic of Equality with Uninterpreted Functions*** (CAV 1999, LNCS 1633). The EUF + positive-equality framework that underlies §3b's UF abstraction soundness. Bryant–Burch–Dill (CAV 1994) is the original processor-verification paper that motivated EUF; this 1999 paper formalises the positive-equality fragment that makes the may-side-only soundness asymmetry of §3b precise.

### §7.3 UF refinement and selective concretisation

6. **Z. S. Andraus and K. A. Sakallah, *Reveal — A Formal Verification Tool for Verilog Designs*** (LPAR 2008, LNCS 5330). The UF-refinement recipe via selective re-interpretation that §4.6 adopts. Reveal's heuristic for choosing between selective concretisation and learned-lemma addition informs the mununu lifter's default strategy.

### §7.4 KMTS foundations (cross-reference)

The KMTS theoretical foundations are covered in [`kmts-theory.md`](kmts-theory.md). The papers most directly relevant to this recipe doc are:

- Bruns–Godefroid CONCUR 2000 — 3-valued mu-calculus + preservation theorem.
- Huth–Jagadeesan–Schmidt TACAS 2001 — KMTS definition + compositional model checking.
- Godefroid–Jagadeesan TACAS 2003 — CEGAR refinement for KMTS.
- Larsen–Larsen–Wąsowski FoSSaCS 2007 — compositional KMTS, congruence of refinement under composition.

### §7.5 Mununu cross-references

- Architecture: [`native-sv-abstraction.md`](native-sv-abstraction.md) §6 (the algorithmic framework), §7 (the composition story), §9 (what the new lifter replaces).
- Theory: [`kmts-theory.md`](kmts-theory.md).
- Broader literature catalog: [`abstraction-literature.md`](abstraction-literature.md) §KMTS and §Predicate-abstraction sections.
- KMTS lifter (post-R.2): [`crates/mununu-core/src/adapter/btor2/kmts_lift.rs`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs).
- Predicate-image SMT helper (extension of today's [`kripke_smt.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs)): the lifter's submodule for §3a/§3b queries.
- CEGAR refinement loop (post-R.5): the lifter's `refine` function.
- UF wrapping policy (post-R.5b): the lifter's `uf_wrap` module.
