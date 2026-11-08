# Abstraction Literature — Reading List for the Auto-Extraction Pipeline

> **Status: planning.** This doc anchors no live code; it grounds
> [`auto-extraction-architecture.md`](auto-extraction-architecture.md). Code
> citations point at existing files only to identify where each idea
> *would* land; the sketches are intentionally incomplete — they show
> shapes, not implementations.

## Preface

The Caliptra `soc_ifc_boot_fsm` proof-by-fire effort (see
[`caliptra-abstraction-analysis.md`](caliptra-abstraction-analysis.md))
walked into a 33.5 M-transition enumeration that the bit-blaster could
not complete in 22 minutes / 4.6 GB RSS on a release build. The Phase 1.6
work that cleared the input-bit cap surfaced a deeper truth: mununu's
`AbstractionType` + `FieldDomain` + `discovered_values` machinery is a
partial, ad-hoc realisation of techniques that the formal-verification
literature has formalised over 25+ years. This doc reads those papers
back into mununu's vocabulary — what does each contribute, what would
its core algorithm look like as Rust against today's `AbstractionType`
enum, and where in the existing module tree would the change land.

The list is curated, not exhaustive. The selection criterion is
*relevance to mununu's specific bottleneck*: extracting an
explicit-state CLTS from real-world RTL (and, less centrally, C and
agentic workflows) under per-signal abstraction declarations, then
evaluating mu-calculus over the result. Papers that improve IC3 itself
without an abstraction angle, or that target software-only domains, are
omitted. Tool papers (AVR, rIC3, BTOR2) appear when their pipeline
shape is the closest published comparator to mununu's.

Each entry follows the same skeleton:

1. **Theoretical core** — what the paper actually proves or proposes.
2. **Pseudocode** — load-bearing algorithm in 10–15 lines.
3. **mununu-shaped Rust sketch** — 10–20 lines of struct + signatures
   against the *current* mununu types ([`adapter/domain.rs`](../../crates/mununu-core/src/adapter/domain.rs),
   [`adapter/state_enum.rs`](../../crates/mununu-core/src/adapter/state_enum.rs),
   [`adapter/sidecar/mod.rs`](../../crates/mununu-core/src/adapter/sidecar/mod.rs),
   [`mu_calculus/mod.rs`](../../crates/mununu-core/src/mu_calculus/mod.rs)).
4. **Map to mununu** — one line: which primitive / module / stage this
   lands in.

The cross-reference matrix at the end is consumed verbatim by
[`auto-extraction-architecture.md`](auto-extraction-architecture.md) §2 and §5.

---

## 1. Kurshan, *Computer-Aided Verification of Coordinating Processes* (Princeton, 1994) — Localization Reduction

**Theoretical core.** Given a property over a subset of state variables,
*prune* the variables outside that subset along with their next-state
logic. The reduced model has fewer latches but admits more behaviours; any
universal property that holds on the reduction holds on the original
(over-approximation soundness). Refinement re-introduces pruned latches
one at a time until the property is decided or all variables are back.
Kurshan's framing predates CEGAR by six years; CEGAR's first paper
explicitly generalises it.

**Pseudocode.**

```text
input : circuit C with latches L = {l1..ln}, property φ over Lφ ⊆ L
output: verdict {holds, fails-with-cex} or refine-request

A := { l ∈ L | l reachable in dep-graph from Lφ ∩ free(φ) }   # initial keep-set
loop:
    M := build_model(C restricted to A; havoc everything outside A)
    r := check(M, φ)
    if r == holds                       : return holds
    if r == fails(cex) and cex feasible : return fails-with-cex(cex)
    A := A ∪ next_layer(cex)            # one pruned latch back per round
```

**mununu-shaped Rust sketch.**

```rust
// adapter/partition.rs (NEW)
use crate::adapter::domain::{AbstractionType, FieldDomain};

pub struct Partition {
    pub kept:    Vec<FieldDomain>,      // active fields
    pub dropped: Vec<FieldDomain>,      // → AbstractionType::Ignored
}

/// One pass of localization shrinkage: keep only fields transitively
/// referenced by the property + their data-flow ancestors. Iterative
/// refinement is driven by the CEGAR loop in `verify::orchestrator`.
pub fn localize(
    all_fields: Vec<FieldDomain>,
    property_atoms: &[String],          // predicate names from Formula::nodes
    deps: &DepGraph,                    // built from BTOR2 / SV AST
) -> Partition { todo!() }
```

**Map to mununu.** Pipeline Stage 2; parent of `AbstractionType::Ignored`.
mununu today applies localization *only* when a user hand-marks a signal
`"ignored"`; this paper makes it automatic.

---

## 2. Clarke, Grumberg, Jha, Lu, Veith, *Counterexample-Guided Abstraction Refinement* (CAV 2000, JACM 2003)

**Theoretical core.** The canonical CEGAR loop. Build a (sound) abstraction,
model-check it; if the abstract counterexample is *feasible* on the
concrete system, return it; otherwise the counterexample is *spurious*
and contains information sufficient to refine the abstraction — typically
by partitioning a previously-collapsed equivalence class along a
predicate that explains the spurious step. Termination is not guaranteed
in general but holds for finite-state systems where the predicate set is
bounded.

**Pseudocode.**

```text
input : concrete system S, property φ
output: verdict {holds, fails-with-cex}

A := initial_abstraction(S)                # coarse: localization or boolean preds
loop:
    r := check(A, φ)
    if r == holds            : return holds
    cex := simulate(r.cex on S)
    if cex.feasible          : return fails-with-cex(cex)
    π   := analyze_spurious(cex)           # find separating predicate(s)
    A   := refine(A, π)                    # split one class along π
```

**mununu-shaped Rust sketch.**

```rust
// verify/refine.rs (NEW)
use crate::mu_calculus::Formula;
use crate::verify::VerifyReport;

pub enum CegarStep {
    Decided(VerifyReport),
    Spurious { new_predicates: Vec<String> },   // feed into sidecar discovered_values
}

pub fn refine_once(
    ctxdsl: &str,
    sidecar_path: &Path,
    formula: &Formula,
    report: &VerifyReport,                 // result of previous round
) -> CegarStep { todo!() }
```

**Map to mununu.** Stage 6 — wraps `verify::orchestrator::verify_project`.
Refinement output is written back to the sidecar's `discovered_values`,
re-realized, and re-evaluated; the loop terminates when the verdict is
True or a feasible counterexample is found.

---

## 3. Graf & Saidi, *Construction of Abstract State Graphs with PVS* (CAV 1997) — Predicate Abstraction

**Theoretical core.** Replace each concrete state `s` with the tuple
`(p1(s), …, pk(s))` where the `pi` are user- or tool-supplied
predicates. The abstract transition relation is the *best
over-approximation* under this map: there is an abstract edge `â → b̂`
iff some concrete state in the `â` class has a concrete successor in
the `b̂` class. Computing this image is the hot inner loop of every
predicate-abstraction tool; in the original paper, PVS was used as the
decision procedure (slow). The 25 years of follow-up work is mostly
about speeding this step up.

**Pseudocode.**

```text
input : transition relation T(s, s'), predicate set P = {p1..pk}
output: abstract transition relation T̂(â, b̂) ⊆ 2^P × 2^P

for each â ∈ 2^P:
    for each b̂ ∈ 2^P:
        query := ∃ s, s'. T(s, s') ∧ ⋀_i pi(s)  = â_i ∧ ⋀_i pi(s') = b̂_i
        if solver.is_sat(query):  T̂.add((â, b̂))
```

**mununu-shaped Rust sketch.**

```rust
// adapter/sidecar/predicate_image.rs (NEW)
pub struct Predicate {
    pub name: String,                  // becomes a FieldDomain variant name
    pub witness: SmtExpr,              // typed against the source's theory
}

pub struct AbstractTransition {
    pub from: Vec<bool>,               // truth values of predicates in `from`
    pub to:   Vec<bool>,
}

pub fn compute_abstract_transitions(
    t_relation: &SmtExpr,              // T(s, s') in the adapter's theory
    predicates: &[Predicate],
) -> Vec<AbstractTransition> { todo!() }
```

**Map to mununu.** Stage 4 algorithmic ancestor. Today
`kripke_smt.rs:enumerate_values` (#L110) is a *constants* enumerator
keyed by one signal; Graf–Saidi generalises it to *predicate tuples*.

---

## 4. Jain, Kroening, Sharygina, Clarke, *Word-Level Predicate-Abstraction and Refinement Techniques for Verifying RTL Verilog* (IEEE TCAD 2008; DAC 2005 precursor)

**Theoretical core.** Three additions to Graf–Saidi for RTL: (a) predicates
are *word-level* — `x == 3`, `y < x + 1` — encoded as bit-vector formulas
to preserve precision; (b) the abstract image is computed by SMT with
careful clause enumeration to handle thousands of predicates; (c) new
predicates during refinement come from the *weakest precondition* of
Verilog `always_ff` statements along the spurious trace — a syntactic
rule, not a guess. The bit-vector handling is what mununu's
`kripke_smt.rs` already partly does; the WP rule is what we don't.

**Pseudocode.**

```text
input : spurious abstract counterexample π = â0 →â1 →…→ân, RTL statements
output: refined predicate set P'

for i in n .. 0:
    for each always_ff statement S that drives a signal in support(âi):
        wp_i := compute_wp(S, predicates_at(i+1))   # symbolic wp on bit-vectors
        candidates := atoms(wp_i) \ P              # new word-level predicates
        P := P ∪ rank_and_filter(candidates)
```

**mununu-shaped Rust sketch.**

```rust
// adapter/systemverilog/wp_refine.rs (NEW)
use crate::adapter::systemverilog::ast::{Module, AlwaysBlock};

pub struct WeakestPrecondition<'a> {
    pub stmt: &'a AlwaysBlock,
    pub post: SmtExpr,                  // predicate to be re-established
}

pub fn wp_atoms(blocks: &[AlwaysBlock], cex_trace: &[StateRef]) -> Vec<SmtExpr> {
    // walk the trace backwards; for each step, compute wp over the
    // assigning always_ff; collect atomic sub-expressions that mention
    // signals in the support of the failing modality.
    todo!()
}
```

**Map to mununu.** Stage 4 refinement source. The output atoms become new
entries in `SvAnnotation.discovered_values` and flow through
`resolve_to_field_domain` unchanged.

---

## 5. Jain, Kroening, Sharygina, Clarke, *VCEGAR: Verilog CounterExample Guided Abstraction Refinement* (TACAS 2007)

**Theoretical core.** Tool paper for paper #4. VCEGAR's value here is the
*architecture*: a single binary takes Verilog + a property, runs CEGAR
end-to-end, and emits either holds / a concrete counterexample. The
predicate set is seeded from syntactic Verilog atoms (case labels, `==`
guards) and refined by WP. The end-to-end pipeline is the closest 2007
analogue to what `mununu verify` aims to be.

**Pseudocode.**

```text
parse_verilog → AST
seed P with syntactic atoms (== constants, case labels)
build abstract model M̂ from (AST, P)
loop:
    r := bdd_check(M̂, φ)
    if r == holds          : print holds; exit
    if simulate(r.cex on AST) feasible : print cex; exit
    P := P ∪ wp_atoms(r.cex, AST)
    M̂ := rebuild(AST, P)
```

**mununu-shaped Rust sketch.**

```rust
// crates/mununu-cli/src/main.rs — extension to `mununu verify`
pub struct CegarOptions {
    pub max_iterations: u32,            // hard cap (terminate if no progress)
    pub seed_strategy: SeedStrategy,    // SyntacticAtoms | DiscoveredValues
    pub wp_refine: bool,                // enable refinement from cex trace
}

pub fn verify_with_cegar(
    config: &VerifyConfig,
    opts: CegarOptions,
) -> Result<VerifyReport, VerifyError> { todo!() }
```

**Map to mununu.** Stage 6 driver shape. The `mununu verify --cegar`
sub-flag is the natural CLI surface.

---

## 6. Goel & Sakallah, *Model Checking of Verilog RTL Using IC3 with Syntax-Guided Abstraction* (NFM 2019)

**Theoretical core.** AVR's distinguishing idea is to pick predicates *not
from WP* but from the **sub-terms already present in the design** — a
syntax-guided approach inspired by SyGuS. Every constant compared
against, every signal width slice, every arithmetic sub-expression that
appears in the RTL is a candidate predicate. The abstraction is
*implicit* — IC3 reasons over the predicate Boolean cube directly,
without materialising the abstract transition relation. The empirical
result: AVR wins HWMCC on word-level tracks against tools that *do*
materialise.

**Pseudocode.**

```text
input : design D (Verilog → btor2-like word-level IR), property φ
output: verdict + (optional) inductive invariant

P := collect_subterms(D)               # all sub-expressions in AST
                                       # filter by appearance in guards / compares
ic3_loop:
    F := initial frames F0 = init(D), Fi = ⊤ for i ≥ 1
    extend_with_blocking_clauses_over(P)
    if reach φ from Fk:  refine_predicates(syntax-guided choice)
    if invariant found:  return holds
```

**mununu-shaped Rust sketch.**

```rust
// adapter/sidecar/predicate_seed.rs (NEW)
use crate::adapter::systemverilog::ast::{Module, Expr};

pub struct PredicateSeed {
    pub signal: String,                 // FieldDomain.name
    pub witnesses: Vec<i64>,            // becomes DiscoveredValue list
    pub rationale: &'static str,        // "case label" | "== constant" | "width slice"
}

pub fn collect_syntactic_predicates(module: &Module) -> Vec<PredicateSeed> {
    // Pre-SMT pass: walk the AST, scrape (a) case labels, (b) RHS of
    // `signal == constant`, (c) constants appearing in width-aware
    // arithmetic. Cheaper than Stage 4; no SMT calls.
    todo!()
}
```

**Map to mununu.** Stage 3 (predicate seeding). Closest published
artefact to what mununu's `mununu btor2 discover` already does
*syntactically* (SMT predicate-image over the BTOR2 IR in
[`adapter/sidecar/predicate_image/all_smt.rs`](../../crates/mununu-core/src/adapter/sidecar/predicate_image/all_smt.rs);
the case-literal scrape lives in
[`adapter/systemverilog/case_literal_extract.rs`](../../crates/mununu-core/src/adapter/systemverilog/case_literal_extract.rs));
this paper formalises the rule and shows it scales.

---

## 7. Goel & Sakallah, *AVR: Abstractly Verifying Reachability* (TACAS 2020)

**Theoretical core.** Tool paper for #6 with empirical results on HWMCC.
The novelty here over #6 is engineering: AVR ships an end-to-end binary
that takes BTOR2 in, produces verdicts + traces out, and exposes
configuration for predicate-seed strategy. Most relevant to mununu: AVR
demonstrates that the predicate-image-free IC3 approach beats explicit
materialisation on real industrial benchmarks. This is the strongest
argument for the Phase B parallel pipeline.

**Pseudocode.** (Same shape as #6; the contribution is system-level.)

**mununu-shaped Rust sketch.**

```rust
// adapter/btor2/avr_bridge.rs (PHASE B only — out of Phase A scope)
pub enum Verdict {
    Holds { invariant: Option<String> },
    Fails { cex_btor2_witness: PathBuf },
    Unknown,
}

/// Phase B trigger: shell out to AVR (or rIC3) on BTOR2; consume
/// counterexample witness, lift back to a CLTS trace for mununu's
/// existing report renderer.
pub fn check_with_external_avr(
    btor2_path: &Path,
    property_index: u32,
    timeout: Duration,
) -> Result<Verdict, IoError> { todo!() }
```

**Map to mununu.** Phase B reference. Mununu's explicit-state CLTS does
not benefit from AVR's algorithm directly; the bridge would *only* make
sense if Phase A's enumeration runtime fails the decision gate.

---

## 8. Goel & Sakallah, *Syntax-Guided Synthesis for Lemma Generation in Hardware Model Checking* (VMCAI 2021)

**Theoretical core.** Extends #6 by replacing hand-chosen predicate
grammars with *learned* sub-term grammars: SyGuS synthesises lemmas to
strengthen IC3 frames. Relevant to mununu only as a forward-looking
direction — once `mununu btor2 discover` produces stable significant-value
sets, a SyGuS layer could *propose* new predicates from cex traces
without WP.

**Pseudocode.**

```text
input : failing IC3 frame Fi with bad cube c, sub-term grammar G
output: lemma ℓ such that Fi ∧ ℓ blocks c and is inductive relative to G

candidates := enumerate(G, depth ≤ d)
for ℓ in candidates:
    if Fi ∧ ℓ blocks c and inductive_relative_to(Fi, ℓ):
        return ℓ
return ⊥                                # give up; widen G
```

**mununu-shaped Rust sketch.**

```rust
// adapter/sidecar/sygus_extend.rs (FUTURE WORK; not Phase A)
pub struct SubtermGrammar { pub operators: Vec<&'static str>, pub depth: u8 }

pub fn propose_predicates_from_cex(
    cex: &[AbstractState],              // from VerifyReport
    grammar: &SubtermGrammar,
) -> Vec<PredicateSeed> { todo!() }
```

**Map to mununu.** Out of Phase A scope. Cite as a Phase B+ extension to
Stage 3.

---

## 9. Andraus & Sakallah, *Automatic Abstraction and Verification of Verilog Models* (DAC 2004) and *Reveal* (LPAR 2008) — Datapath Abstraction

**Theoretical core.** Partition design signals into **control** (state
machines, narrow flags) and **datapath** (wide adders, multipliers,
shifters). Replace each datapath sub-circuit with an uninterpreted
function (UF) symbol; verify the control logic against UF-axiomatised
datapath. If the result holds, it holds for the concrete design (UF is
sound). Refinement re-instantiates a datapath component when the UF
abstraction is too coarse (e.g., the proof needs `mul(a, b) = mul(b, a)`).

**Pseudocode.**

```text
input : circuit C with signal set S
output: abstracted circuit C', UF mapping

control, datapath := partition_by_width_and_role(S)        # heuristic
for each connected datapath sub-circuit D in C:
    introduce UF symbol f_D with signature inputs(D) → outputs(D)
    replace D with f_D
C' := control ⊕ {f_D | D ∈ datapath}
```

**mununu-shaped Rust sketch.**

```rust
// adapter/partition.rs (NEW, shared with Stage 2)
use crate::adapter::domain::FieldDomain;

pub struct DatapathUf {
    pub symbol: String,                 // f_alu_0
    pub inputs: Vec<String>,            // signal names
    pub output: String,
}

pub struct Partition {
    pub kept: Vec<FieldDomain>,         // control signals
    pub dropped: Vec<FieldDomain>,      // → Ignored
    pub datapath_uf: Vec<DatapathUf>,   // NEW: feeds Stage 4 SMT theory selector
}
```

**Map to mununu.** Stage 2 (jointly with Kurshan). The `wait_count[7:0]`
collapse in `caliptra-abstraction-analysis.md` §2.2 — keep the
`== 0` predicate, drop the increment arithmetic — is exactly this paper's
prescription expressed as a hand-written sidecar entry. Stage 2 makes it
automatic.

---

## 10. Bryant, Kroening, Ouaknine, Seshia, Strichman, Brady, *Deciding Bit-Vector Arithmetic with Abstraction* (TACAS 2007; IJSTTT 2009)

**Theoretical core.** A decision procedure for bit-vector arithmetic that
*alternates under-* and *over-approximations*. Under-approximation: shrink
some bit-vector variables to fewer Boolean variables than their nominal
width; check for SAT. Over-approximation: replace bit-vector operations
with linear-arithmetic over-approximations; check for UNSAT. Tightens
each side until they meet. Crucial for mununu because the BTOR2 path's
runtime cost is *mostly* in solving the abstract-transition formula,
and the under/over alternation is the cheapest way to short-circuit
queries with obvious verdicts.

**Pseudocode.**

```text
input : bit-vector formula φ
output: SAT / UNSAT verdict

w := 1                                     # current under-approximation bit-width
loop:
    φ_under := restrict_widths(φ, w)
    if SAT(φ_under)        : return SAT
    φ_over := linear_overapprox(φ)
    if UNSAT(φ_over)       : return UNSAT
    w := w * 2
    if w > max_width(φ)    : fallback to full bit-blast
```

**mununu-shaped Rust sketch.**

```rust
// adapter/sidecar/bv_abstraction.rs (NEW)
pub enum BvApprox { Under(u32), Over, Concrete }

pub fn decide_bv_with_abstraction(
    formula: &SmtExpr,
    budget: Duration,
) -> Result<bool, TimeoutError> {
    // alternate Under(w) / Over / Concrete; cap each query at budget / 4
    todo!()
}
```

**Map to mununu.** Stage 4 inner loop. When the predicate-image SMT
query saturates Z3, drop into this scheme rather than time out.

---

## 11. Hoder, Bjørner, de Moura, *SMT Techniques for Fast Predicate Abstraction* (CAV 2006)

**Theoretical core.** Treat the predicate-image computation as a single
*all-SMT* problem: ask the solver to enumerate **all satisfying
assignments** of `T(s, s') ∧ assign(P, s) ∧ assign(P, s')` over the
predicate variables, then collect the projection. The DPLL(T) framework
allows this enumeration to share clause learning across the abstract
state space, beating per-state queries by 20×+. Direct fit for mununu's
Stage 4 hot loop.

**Pseudocode.**

```text
input : T(s, s'), predicates P
output: T̂ ⊆ 2^P × 2^P

solver.assert(T(s, s'))
solver.assert(p_i ↔ p_i(s) for each pi ∈ P)         # name predicate vars
solver.assert(p'_i ↔ p_i(s') for each pi ∈ P)
results := []
while solver.check_sat():
    m := solver.model()
    results.add((restrict(m, p_*), restrict(m, p'_*)))
    solver.assert(¬ same_assignment(m on p_* ∪ p'_*))  # block this abstract edge
return results
```

**mununu-shaped Rust sketch.**

```rust
// adapter/sidecar/predicate_image.rs (continuation of #3)
impl PredicateImage<'_> {
    /// All-SMT enumeration of abstract transitions.
    /// Bounded by `cap_edges` to keep enumeration bounded (default 4096).
    pub fn all_abstract_edges(&self, cap_edges: usize)
        -> Vec<AbstractTransition>
    {
        // see pseudocode above; uses z3::Solver's solver.check / model
        // / assert(¬model) idiom that already appears in kripke_smt.rs.
        todo!()
    }
}
```

**Map to mununu.** Stage 4 *algorithm*. Replaces the per-guard
constant-enumeration loop at
[`kripke_smt.rs:enumerate_values`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs#L110)
with an all-SMT predicate-tuple enumeration. Single largest expected
runtime win at the Caliptra scale.

---

## 12. Cimatti, Griggio, Mover, Tonetta, *IC3 Modulo Theories via Implicit Predicate Abstraction* (TACAS 2014) — IC3-IA

**Theoretical core.** Combine IC3 with predicate abstraction *without
ever materialising* the abstract transition relation. Frames are
maintained over the predicate Boolean cube; concrete satisfiability
queries are pushed into the SMT solver lazily during blocking-clause
generation. The result is the runtime profile of IC3 with the precision
of word-level predicate abstraction. **This is the strongest published
alternative to mununu's explicit-state pipeline** — and the natural
target of Phase B.

**Pseudocode.**

```text
input : T(s, s'), property φ, predicate set P
output: verdict

F[0] := init(s) over s_abs := proj_P(s)
F[i] := ⊤ for i ≥ 1
loop:
    if F[k] ∧ ¬φ_abs unsat under T_abs(s_abs, s'_abs)    : return holds
    block bad cubes by generating clauses over P:
        c := generalize_cube(bad)                          # solver-side, lazy
        F[i..k].add(c)
    if blocking impossible : analyze_cex(s_abs trace) → refine P
```

**mununu-shaped Rust sketch.**

```rust
// (Phase B only — there is no Phase-A-portable shape.)
//
// IC3-IA cannot be expressed against mununu's explicit-state CLTS without
// re-enumerating the implicit relation, which defeats its purpose. The
// Phase B integration would be a new adapter that takes BTOR2 + sidecar,
// invokes an IC3-IA engine (own implementation or external — e.g. AVR,
// rIC3, nuXmv), and converts verdict + witness back into a VerifyReport.
// No Rust sketch fits in 20 lines here; the integration point is the
// external-process boundary.
```

**Map to mununu.** Phase B substrate candidate. Cited in Phase A only as
the architectural reason to **not** spend Phase A effort on materialising
larger predicate sets — the asymptotic ceiling is already known.

---

## 13. Lahiri & Bryant, *Predicate Abstraction with Indexed Predicates* (TOCL 2007; arXiv 2004)

**Theoretical core.** Extend predicate abstraction with *quantified*
predicates over arrays / memories: `∀i. mem[i] < threshold`. Without
this, predicate abstraction collapses any structured memory into either
"top" (too coarse, every read can be anything) or one predicate per
concrete index (combinatorial explosion). Relevant to mununu when the
extraction adapter handles C arrays or RTL register files.

**Pseudocode.**

```text
input : transition relation over arrays, candidate quantified predicates Q
output: refined predicate set P including indexed family

for each q = ∀i ∈ I. body(i) in Q:
    instantiate: p_i := body(i_witness) for representative i_witness ∈ I
    if p_i discriminates spurious cex:
        P.add(p_i family)               # one predicate per i, with index tagged
```

**mununu-shaped Rust sketch.**

```rust
// adapter/extraction/array_predicate.rs (NEW; FUTURE)
pub struct IndexedPredicate {
    pub array: String,                  // e.g. "mem" in C; "ram[]" in BTOR2
    pub index_role: IndexRole,          // representative | bounded-range | universal
    pub body: SmtExpr,
}
```

**Map to mununu.** Stage 4 extension for the C extraction path. **Not
Phase A** — gated on the Theory selector (see Risk in architecture doc).

---

## 14. Yang, Goel, Sakallah, *Leveraging Datapath Propagation in IC3 for Hardware Model Checking* (FMCAD 2023; arXiv 2309.14834)

**Theoretical core.** Within IC3, push *word-level constraints* directly
through datapath operators (adders, comparators) instead of bit-blasting
them inside generalize-cube. Each generalisation step retains word-level
structure, dramatically shrinking blocking-clause size on wide
datapaths. Like #12, this is an IC3-internal optimisation; **not
portable to mununu's explicit pipeline**.

**Pseudocode.** (Inside IC3's generalize_cube: replace bit-level pivot
selection with word-level pivot selection on the propagated constraint
graph.)

**mununu-shaped Rust sketch.** None — the algorithm lives below the
`Clts` abstraction. Cite for Phase B substrate selection (AVR-class or
rIC3-class engine).

**Map to mununu.** Phase B engine choice criterion.

---

## 15. Niemetz, Preiner, Wolf, Biere, *Btor2, BtorMC and Boolector 3.0* (CAV 2018)

**Theoretical core.** Spec for the BTOR2 word-level format: a typed,
sorted intermediate language for sequential hardware verification with
arrays and bit-vectors, designed as the successor of Btor and the
word-level peer of AIGER. mununu's BTOR2 reader (
[`adapter/btor2/parser.rs`](../../crates/mununu-core/src/adapter/btor2/parser.rs))
already consumes a subset; the paper defines the surface mununu must
keep honouring as Yosys evolves.

**Pseudocode.** (Format spec; no algorithm.)

**mununu-shaped Rust sketch.**

```rust
// adapter/btor2/parser.rs (EXISTING — cite, do not change)
pub struct Btor2Line { pub nid: u32, pub op: Op, pub sort: SortId, pub args: Vec<NodeId> }

/// Stage 1 ingest. Already implemented; predicate-image work in Stage 4
/// must not assume anything beyond what this parser preserves.
pub fn parse_btor2(text: &str) -> Result<Vec<Btor2Line>, ParseError> { todo!() }
```

**Map to mununu.** Stage 1 substrate. Anchor for "what does the BTOR2
side already promise downstream stages."

---

## 16. *The rIC3 Hardware Model Checker* (arXiv 2502.13605, 2025)

**Theoretical core.** Modern IC3 implementation for BTOR2 with the
post-2014 generalisations (PDR-CTG, IGR, etc.). Empirical baseline on
HWMCC 2024. mununu would not adopt its algorithm; rIC3 is the
**runtime baseline** the decision gate compares against — if Phase A's
explicit enumerator cannot solve Caliptra-class designs within 2× of
rIC3's runtime on the same BTOR2 input, Phase B is mandatory.

**Pseudocode.** (Standard IC3 + 2024-vintage generalisations; out of
scope.)

**mununu-shaped Rust sketch.** None — rIC3 is an external comparator
used in the decision gate.

**Map to mununu.** Phase A→B decision-gate runtime reference.

---

## 17. Hsiao, Trippel, Mulligan, Manerkar, *rtl2µspec: Synthesizing Formal Models of Hardware from RTL for Efficient Microarchitectural Security Verification* (MICRO 2021)

**Theoretical core.** A sibling pipeline: RTL → explicit-state model →
property check. The novelty is the *abstraction policy* — driven by
microarchitectural axioms (Spectre-class data-flow rules) rather than
per-signal user declarations. Same overall shape as mununu's BTOR2 path,
different decision about *what counts as a signal worth keeping*. Worth
citing as the closest non-mununu pipeline that produces an
explicit-state model for security verification, and as evidence that the
explicit-state surface (which mununu also commits to) has a track record
in this exact use case.

**Pseudocode.**

```text
input : Verilog RTL, microarchitectural axiom A
output: explicit-state model M with A-anchored predicates

P := predicates_from_axiom(A)               # e.g. "tainted signal reaches register"
COI := cone_of_influence(P, RTL)
M := build_explicit_model(restrict(RTL, COI), P)
return M
```

**mununu-shaped Rust sketch.** None — the paper's contribution is the
*axiom library*, not an algorithm. The mununu equivalent is the
[`adapter/templates/builtin_templates.json`](../../crates/mununu-core/src/adapter/templates/builtin_templates.json)
catalog plus the (future) microarchitectural-property templates.

**Map to mununu.** Validation that the explicit-state CLTS substrate is
defensible for security verification; informs the template registry's
domain taxonomy.

---

## 18. Caliptra 2.0 / 2.1 Architecture Specs (OCP 2022; Microsoft Tech Community 2024–2025)

**Theoretical core.** Not a paper; the architecture documents for the
empirical target. Defines the integrated root-of-trust IP, the FPV scope
that AMD/Google/Microsoft/NVIDIA contract out to third-party verification
firms, and the publicly-released RTL that mununu's Phase 1 work
exercises. Cite for: (a) the design is independently verified, so
mununu's contribution is a *complementary* abstraction-refinement view
on hand-bounded properties (CWE-1245 reachable UNDEF state, e.g.); (b)
the scope of "what is the modeled system" is constrained by Caliptra's
public release boundary.

**Pseudocode.** N/A.

**mununu-shaped Rust sketch.** N/A. The Caliptra-specific empirical
fixtures live under
[`examples/verify/sv_yosys_caliptra_rtl_150/`](../../examples/verify/sv_yosys_caliptra_rtl_150/)
and `/tmp/caliptra_retry/`.

**Map to mununu.** Empirical target for the decision-gate runtime
measurement.

---

---

# KMTS & 3-valued mu-calculus (entries 19–24)

> **Section context.** The original 18-paper catalog above was assembled for
> the pillow-plan / `auto-extraction-architecture.md` framing, which centred
> on Phase A's explicit-state CLTS pipeline. The KMTS pivot
> ([`native-sv-abstraction.md`](native-sv-abstraction.md)) re-centres the
> abstraction story on **Kripke Modal Transition Systems with 3-valued
> mu-calculus** — the only abstraction framework that is uniformly sound
> for the full mu-calculus (including alternating fixpoints, including
> liveness). The six entries that follow are the load-bearing KMTS
> citations referenced by [`kmts-theory.md`](kmts-theory.md) and the §6 of
> the architecture doc.

## 19. Larsen & Thomsen, *A Modal Process Logic* (LICS 1988)

**Theoretical core.** Original *modal transition systems* (MTS): triples
`(S, R_must, R_may)` over an action alphabet with `R_must ⊆ R_may`. The
intuition is *under-specification*: a must-edge is a transition every
implementation must exhibit; a may-edge is a transition every
implementation may exhibit. The paper introduces the refinement preorder
that lets one MTS be a more concrete specification of another, with the
asymmetry (may shrinks; must grows) that makes it the right notion for
"abstract less informatively."

**Pseudocode.** (Definitional, not algorithmic.)

```text
M = (S, R_must, R_may) over action alphabet Act, with R_must ⊆ R_may

M_2 ≼ M_1 (M_2 refines M_1) iff there exists ≼ ⊆ M_2.S × M_1.S such that
  for all (s_2, s_1) ∈ ≼:
    (a) every may-step of s_1 is matched by a may-step of s_2 (accommodation)
    (b) every must-step of s_2 is matched by a must-step of s_1 (preservation)
```

**mununu-shaped Rust sketch.**

```rust
// crates/mununu-core/src/clts/mod.rs (EXTENSION; post-R.1)
enum TransitionModality { Sharp, MayOnly }   // standard KMTS: must ⊆ may
struct Transition {
    ...existing fields...
    modality: TransitionModality,             // default Sharp on construction
}
```

**Map to mununu.** Foundational. The `TransitionModality` enum on
`Transition` is the direct implementation of the may/must distinction;
the `Sharp` variant corresponds to `R_must ∩ R_may` (both required and
admitted), `MayOnly` to `R_may \ R_must`. R.1 ships this.

---

## 20. Dams, Gerth, Grumberg, *Abstract Interpretation of Reactive Systems* (TOPLAS 1997, Vol. 19 No. 2)

**Theoretical core.** Foundational abstract-interpretation framework
for branching-time temporal logic. Introduces the *mixed transition
system* generalisation that *drops* the `R_must ⊆ R_may` invariant of
Larsen–Thomsen — admitting `must`-without-`may` for
under-approximation-only abstractions. The Galois-connection setup,
the preservation properties of abstract interpretation on
branching-time formulas, and the soundness proof for the modal-mu
fragment under either over- or under-approximation are all defined
here. Mononu's standard-KMTS shape (the `Sharp + MayOnly` two-variant
enum) is the restricted form of this framework — predicate-image
construction guarantees the invariant by definition, so the
mixed-system generalisation is not needed for the BTOR2 lifter.

**Pseudocode.** (Framework definition; no single algorithm.)

**mununu-shaped Rust sketch.** (See #19; the same `TransitionModality`
enum, but consciously *omitting* a third `MustOnly` variant — the
mixed-system extension is deferred work, not currently needed.)

**Map to mununu.** Theoretical justification for the two-variant
choice in §6.3 of the architecture doc. Cited in
[`kmts-theory.md`](kmts-theory.md) §2.3 as the generalisation we do not
adopt; structurally excluded from the data model by the predicate-image
construction.

---

## 21. Bruns & Godefroid, *Generalized Model Checking: Reasoning about Partial State Spaces* (CONCUR 2000)

**Theoretical core.** The 3-valued mu-calculus semantics. Verdicts
in `{T, F, ⊥}` with `⊥` ("unknown") as the third value of Kleene's
strong 3-valued logic. The modal operators read both relations: `[a]φ`
is `T` iff every may-`a`-successor satisfies `φ` as `T`; `F` iff some
must-`a`-successor has `F`. The preservation theorem: a `T`/`F`
verdict on the abstract transfers to the concrete *for the full
mu-calculus including alternating fixpoints*; only `⊥` requires
refinement. This is the result that makes KMTS the right framework
for *any* sound mu-calculus abstraction — not just for safety, not
just for liveness, but for both under a single abstract model.

**Pseudocode.**

```text
⟦[a]φ⟧_M(s) = T   iff   for every s' with (s, a, s') ∈ R_may : ⟦φ⟧_M(s') = T
              F   iff   exists s' with (s, a, s') ∈ R_must : ⟦φ⟧_M(s') = F
              ⊥   otherwise

⟦⟨a⟩φ⟧_M(s) = T   iff   exists s' with (s, a, s') ∈ R_must : ⟦φ⟧_M(s') = T
              F   iff   for every s' with (s, a, s') ∈ R_may : ⟦φ⟧_M(s') = F
              ⊥   otherwise

Fixpoints: Kleene iteration over the *information order*
  ⊥ ⊑_i F,  ⊥ ⊑_i T,  F and T incomparable
```

**mununu-shaped Rust sketch.**

```rust
// Illustrative per-element sketch. As-built diverged: the production trait is
// the BULK `EvalDomain` (associated `Valuation` type = whole-state-set
// BitVec | TritSet) in crates/mununu-core/src/mu_calculus/evaluator.rs; this
// per-element `truth_domain` trait was an R.1 artifact the R.3 evaluator
// bypassed, retired in P2.4. See evaluator-domain-unification.md.
trait TruthDomain {
    type Element: Clone + Eq;
    fn truth_top(&self) -> Self::Element;
    fn truth_join(&self, a: &Self::Element, b: &Self::Element) -> Self::Element;
    fn info_bot(&self)  -> Self::Element;   // false in Bool, KleeneBot in Kleene
    fn info_join(&self, a: &Self::Element, b: &Self::Element) -> Self::Element;
    fn box_modality(&self, may: &[Self::Element], must: &[Self::Element]) -> Self::Element;
    fn diamond_modality(&self, may: &[Self::Element], must: &[Self::Element]) -> Self::Element;
    // (truth_bot, truth_meet, truth_negate, info_leq elided for brevity)
}
struct KleeneDomain;   // 3-valued instantiation; truth-order ≠ info-order
```

**Map to mununu.** §6.2 of the architecture doc adopts these semantics
verbatim. R.3 ships `KleeneDomain`; the dual-lattice trait shape
(§6.4 of the architecture doc) is the operational consequence of the
distinction between truth-order operations (formula semantics) and
information-order operations (fixpoint convergence) the Bruns–Godefroid
result requires.

---

## 22. Huth, Jagadeesan, Schmidt, *Modal Transition Systems: A Foundation for Three-Valued Program Analysis* (TACAS 2001)

**Theoretical core.** The KMTS definition itself: a 5-tuple `(S, S_0,
R_must, R_may, L)` with `L: S × AP → {T, F, ⊥}` — Larsen–Thomsen MTS
extended with 3-valued state labelling. Proves that 3-valued mu-calculus
model checking on KMTSes is sound, complete (relative to the lattice's
expressiveness), and *decidable for finite KMTSes*. The compositional
case: composition is pointwise on may and must (per-axis conjunction on
synchronisation); refinement is congruential under composition. This is
the result that makes the §7 "structural free lunch" of the architecture
doc a theorem, not a hope.

**Pseudocode.**

```text
KMTS M = (S, S_0, R_must, R_may, L)
L: S × AP → {T, F, ⊥}     (with L(s, p) = T iff p holds on every concretisation of s,
                            L(s, p) = F iff p fails on every concretisation,
                            L(s, p) = ⊥ otherwise)

Composition M_1 ∥ M_2 over Sync ⊆ Act:
  for each capability c ∈ {must, may}:
    synchronising step: ((s_1, s_2), a, (s_1', s_2')) ∈ R_c iff
        a ∈ Sync ∧ (s_1, a, s_1') ∈ M_1.R_c ∧ (s_2, a, s_2') ∈ M_2.R_c
    interleaving step:  ((s_1, s_2), a, (s_1', s_2)) ∈ R_c iff
        a ∉ Sync ∧ (s_1, a, s_1') ∈ M_1.R_c    (symmetric for right side)
```

**mununu-shaped Rust sketch.**

```rust
// crates/mununu-core/src/composition/mod.rs (EXTENSION; post-R.1)
fn merge_modality(left: TransitionModality, right: TransitionModality) -> TransitionModality {
    // per-axis conjunction: has_may(L) ∧ has_may(R) ; has_must(L) ∧ has_must(R)
    match (left, right) {
        (Sharp,   Sharp)   => Sharp,
        (Sharp,   MayOnly) | (MayOnly, Sharp) => MayOnly,
        (MayOnly, MayOnly) => MayOnly,
    }
}
```

**Map to mununu.** §6.5 of the architecture doc derives the modality-merge
table from this paper's per-axis conjunction rule. R.1 ships the merge;
the audit of the `composition/mod.rs` shared-label rendezvous is the
operational corollary.

---

## 23. Godefroid & Jagadeesan, *Automatic Abstraction Using Generalized Model Checking* (TACAS 2003)

**Theoretical core.** The CEGAR-style refinement loop for KMTS — how
to respond to `⊥` verdicts by extracting refinement predicates from
spurious abstract counterexamples. Key insight: a `⊥` verdict admits
an abstract counterexample that may or may not concretise; the
spuriousness check decides which, and if spurious, the SMT UNSAT core
identifies which predicate to add. Generalises CEGAR from the 2-valued
setting (where refinement triggers on a spurious abstract `F` verdict)
to the 3-valued setting (where refinement triggers on `⊥`).

**Pseudocode.**

```text
input : KMTS M_0, property φ
output: verdict in {T, F} or ⊥ at refinement cap

M := M_0
for round in 1..K:
    verdict, cex := evaluate_3valued(M, φ)
    if verdict ∈ {T, F}: return verdict
    spurious := discharge_concretely(cex)
    if SAT: return F                       # cex is real
    core := unsat_core(spurious)
    new_predicates := extract_interpolant(core)
    M := M.with_predicates(M.predicates ∪ new_predicates)
return ⊥                                    # cap reached or stalled
```

**mununu-shaped Rust sketch.**

```rust
// crates/mununu-core/src/adapter/btor2/kmts_lift.rs::refine (NEW; post-R.5)
pub fn refine(
    mut model: Kmts,
    formula: &Formula,
    max_rounds: u32,
) -> (KleeneVerdict, Option<AbstractCounterexample>) {
    for _round in 0..max_rounds {
        let (verdict, cex) = evaluate_kleene(&model, formula);
        if matches!(verdict, KleeneT | KleeneF) { return (verdict, cex); }
        match discharge_concrete(&cex) {
            ConcreteWitness(_) => return (KleeneF, Some(cex)),
            UnsatProof(core) => {
                let new_preds = interpolate(&core);
                if new_preds.is_empty() { break; }   // stall
                model = model.with_predicates(new_preds);
            }
        }
    }
    (KleeneBot, None)
}
```

**Map to mununu.** §4 of [`predicate-abstraction-recipe.md`](predicate-abstraction-recipe.md)
adopts this algorithm. R.5 ships the refinement loop;
R.5b extends it with two-axis (predicate + UF) refinement per the
architecture doc §6.10.

---

## 24. Larsen, Nyman, Wąsowski, *Modal I/O Automata for Interface and Product Line Theories* (FoSSaCS 2007)

**Theoretical core.** Compositional theory of KMTSes with input/output
asymmetry. Proves that modal refinement is *congruential* under
parallel composition: `M_1 ≼ M_1' ⇒ M_1 ∥ M_2 ≼ M_1' ∥ M_2`. The
operational consequence for mununu: refining one module's KMTS (e.g.
via CEGAR predicate addition) refines the composed KMTS *without
re-composing* — the per-module refinement step is local. This is the
result that closes the §7 architecture-doc claim that compositional
KMTS is the "structural free lunch": composition is sound, refinement
is local, no AGR machinery needed.

**Pseudocode.** (Theorem statement, not algorithmic.)

```text
Theorem (Larsen–Nyman–Wąsowski 2007, congruence of refinement under ∥):
  If M_2 ≼ M_1 then for every modal I/O automaton N over a compatible alphabet:
    M_2 ∥ N ≼ M_1 ∥ N
```

**mununu-shaped Rust sketch.** None — this is a meta-theoretic property
that holds automatically for the modality-merge implementation
(#22 sketch). The mununu test suite asserts the property empirically
on hand-built KMTS pairs.

**Map to mununu.** §5.2 of [`kmts-theory.md`](kmts-theory.md) and §7
of the architecture doc cite this paper for the local-refinement
property. Operational meaning: the §6.10 CEGAR loop's per-module
refinement is sound under composition without needing a global proof
obligation.

---

# Assume-Guarantee Reasoning (entries 25–28)

> **Section context.** AGR is the classical compositional-verification
> framework: rather than verifying a global property over a composed
> system directly, decompose it into per-module obligations
> (*assumptions* about the environment, *guarantees* about the module)
> and discharge them with a circular proof rule. KMTS composition
> (#22, #24) makes AGR *unnecessary* for mu-calculus verification —
> the §7 "structural free lunch" replaces the AGR ladder. The four
> entries below are catalogued for two reasons: (a) they are the
> foundational citations any compositional-verification literature
> review must include; (b) the optional sidecar `assumptions: Vec<MuFormula>`
> field (§7.3 of the architecture doc) is a degenerate AGR — environment
> over-approximation as user-supplied assertions — that retains the AGR
> *interface* without the AGR *discharge cost*. Future product-line or
> contract-oriented work on mununu may revisit these.

## 25. Pnueli, *In Transition from Global to Modular Temporal Reasoning about Programs* (in *Logics and Models of Concurrent Systems*, ed. Apt, NATO ASI Series 1985)

**Theoretical core.** The original assume-guarantee framework. To
verify that a composed system `M_1 ∥ M_2` satisfies a global property
`φ`, decompose `φ` into per-module obligations of the form
`A_i ⇒ G_i` — module `i`'s guarantee `G_i` holds under assumption `A_i`
about its environment. The composition is sound if every assumption is
discharged by the other modules' guarantees. The asymmetry (module
guarantees its outputs assuming inputs behave) is what makes the
decomposition tractable.

**Pseudocode.**

```text
input : modules M_1, …, M_n; global property φ
output: verdict via per-module obligations

decompose: φ ≡ (A_1 ⇒ G_1) ∧ … ∧ (A_n ⇒ G_n)   (user-supplied or synthesised)
for each i:
    verify M_i ⊨ (A_i ⇒ G_i)                    (local model check)
discharge: for each i, A_i must follow from {G_j : j ≠ i}
if all discharges succeed: M_1 ∥ … ∥ M_n ⊨ φ
```

**mununu-shaped Rust sketch.**

```rust
// (No direct mununu landing — superseded by KMTS composition.)
// The optional sidecar `assumptions: Vec<MuFormula>` field (§7.3 of the
// architecture doc) is the degenerate-AGR interface: environment
// over-approximation as user-supplied entry assertions, *without* the
// circular discharge. The lifter treats assumptions as `must`-true on
// entry; soundness is by environment over-approximation, not by AGR
// discharge.
struct ModuleAnnotation {
    ...existing fields...
    assumptions: Vec<MuFormula>,   // post-S.3 schema
}
```

**Map to mununu.** §7.3 of the architecture doc. The classical AGR
framework is not adopted — KMTS composition's structural soundness
replaces it. The `assumptions` sidecar field provides the AGR
*interface* (declare environment behaviour) without the AGR
*discharge engine*.

---

## 26. McMillan, *Verification of an Implementation of Tomasulo's Algorithm by Compositional Model Checking* (CAV 1998)

**Theoretical core.** Circular AGR with inductive invariants. To break
the chicken-and-egg circularity of "module A assumes module B's
behaviour; module B assumes module A's behaviour," McMillan proves
both assumptions *simultaneously* by induction on time: at each time
step, both assumptions hold if they held at all previous steps. The
inductive proof rule is sound under reasonable conditions on the
temporal operators involved. Demonstrated on the Tomasulo out-of-order
execution algorithm — a non-trivial industrial application.

**Pseudocode.**

```text
input : modules M_1, M_2; mutual assumptions A_1, A_2 about each other
output: verdict on whether A_1, A_2 hold jointly

base case (t = 0): show A_1(0) and A_2(0) hold initially
inductive step:    assuming A_1(t), A_2(t) hold for all t ≤ T,
                   show M_1 ⊨ A_2(T+1) and M_2 ⊨ A_1(T+1)
if both inductive steps succeed: A_1, A_2 hold for all time
```

**mununu-shaped Rust sketch.** None — circular AGR is genuinely new
machinery that mununu does not have. The §7 of the architecture doc
explicitly argues that the KMTS framework's compositional soundness
makes this *not* necessary; if a future use case demands circular AGR
(e.g. modelling a multi-IP SoC where each IP makes assumptions about
its neighbours), the relevant data structure would be a pair of
mutually-recursive `(assumption, guarantee)` KMTSes per module, with
a discharge engine that iterates the inductive proof.

**Map to mununu.** Deferred per §11 of the architecture doc. Cited
here as the canonical circular-AGR reference; not adopted.

---

## 27. Cobleigh, Giannakopoulou, Păsăreanu, *Learning Assumptions for Compositional Verification* (TACAS 2003)

**Theoretical core.** Automatic *learning* of AGR assumptions via
Angluin's L* algorithm for regular-language inference. Rather than
hand-authoring the per-module assumption `A_i`, treat it as a regular
language over the module's interface alphabet and have the model
checker iteratively refine it through membership and equivalence
queries. Demonstrated on industrial JPL software with significant
reduction in human-authoring effort. Particularly relevant when the
right assumption is non-obvious (e.g. emerges from the protocol's
deadlock-freedom requirement).

**Pseudocode.**

```text
input : module M, property φ, interface alphabet Σ
output: learned assumption A such that M ⊨ (A ⇒ φ)

A := L*_learner.initial_hypothesis()
loop:
    if M ⊨ (A ⇒ φ): return A
    cex := find_counterexample(M, A ⇒ φ)
    if cex.violates_φ_on_real_runs(M):       # not just a spurious A-violation
        return UNSAT                          # property fails on M
    refine A with the witness from cex
```

**mununu-shaped Rust sketch.** None — L* learning is out of scope.
The architecture doc's §11 deferred section lists it explicitly. A
mununu implementation would be a new module
`crates/mununu-core/src/verify/assumption_learning.rs` consuming a
KMTS, a property, and an alphabet; emitting a learned assumption
formula.

**Map to mununu.** Deferred per §11. The user-supplied
`assumptions: Vec<MuFormula>` sidecar field (#25 sketch) is the manual
counterpart: when the user knows the right assumption, sidecar entry
suffices; L* would synthesise it automatically. Trigger condition for
adopting: a fixture where the right assumption is genuinely non-obvious.

---

## 28. Cimatti & Tonetta, *A Property-Based Proof System for Contract-Based Design* (FMCAD 2012; full system: OCRA tool, NFM 2013)

**Theoretical core.** *Contract-based design*: each component carries
a contract `(assumption, guarantee)` pair, refinement of contracts is
structural, and system-level properties are verified via a hierarchical
discharge that decomposes along the contract structure. OCRA (Othello
Contracts Refinement Analysis) is the tool implementation. Most
sophisticated of the AGR-family papers — adds compositional contract
refinement on top of the McMillan-style circular discharge. Useful
reference for product-line and modular SoC verification.

**Pseudocode.**

```text
input : component hierarchy H with per-component contracts {(A_i, G_i)}
output: verdict on whether system contract is satisfied

for each component C_i in H:
    verify M_i ⊨ (A_i ⇒ G_i)                  (per-component check)
for each parent-child relation C_i ⊏ C_j in H:
    verify contract refinement: A_i ⊨ A_j' ∧ G_j' ⊨ G_i
                                (where the primed contract is C_j's at C_i's interface)
if all checks pass: system contract holds
```

**mununu-shaped Rust sketch.** None — contract-based design is out
of scope. The natural mununu landing would be a per-source contract
declaration in `verify.toml` and a discharge engine in
`crates/mununu-core/src/verify/contract.rs`; both deferred.

**Map to mununu.** Deferred per §11. Cited as the most-developed
AGR-family tool for contract-style modular verification; revisit if a
multi-IP-SoC use case surfaces where contract refinement is the right
proof structure.

---

# Game-based 3-valued abstraction & GKMTS (entries 29–30)

> These two papers extend the KMTS line (entries 19–24) in the two
> directions the R.4.5 / R.5.0 / R.6 tracks depend on: from systems to
> *games* (per-player may/must), and from plain KMTS to *generalized*
> KMTS (hyper-must transitions). Numbered after the AGR block to avoid
> renumbering the by-number internal cross-references.

## 29. de Alfaro, Godefroid, Jagadeesan, *Three-Valued Abstractions of Games: Uncertainty, but with Precision* (LICS 2004)

**Theoretical core.** Extends may/must (3-valued) abstraction from
transition systems to two-player *games*, tracking the may/must
distinction **per player**. A definite abstract game value ("controller
wins" / "controller loses") transfers to the concrete game — the game
analogue of the Bruns–Godefroid (#21) preservation theorem. The
load-bearing asymmetry: a definite *controller win* requires *must*-moves
for the controller (it can actually force them) and *may*-moves for the
environment (the abstraction must admit every adversary move).

**Map to mununu.** The governing theorem for the **R.6
controllability-aware KMTS** track: once a model carries *both*
controllable labels *and* may/must edges, single-agent Bruns–Godefroid
(#21) no longer covers it — the per-player rule of this paper does.
[`kmts-theory.md`](kmts-theory.md) §7 builds the 2×2 (controllability ×
modality) rule on it. The production verdict path now **implements** the
per-player game semantics: **PO-3 / R.6.8 CLOSED 2026-06-23** —
[`evaluate_tri`](../../crates/mununu-core/src/mu_calculus/evaluator.rs)
routes `Control::{Controllable, Environment}` through `modal_trit_from_target`
(the §7.2 per-player rule), so a *definite* controllability verdict is now
sound (the audit empirically caught + fixed the two-pass over-claim; see
[`cube-modal-soundness-2026-06-23-full-audit.md`](../archive/reviews/cube-modal-soundness-2026-06-23-full-audit.md), published under `docs/archive/`). The advisory in
[`predicate-abstraction-recipe.md`](predicate-abstraction-recipe.md) §4.9
+ [`cube_modality_soundness_warnings`](../../crates/mununu-core/src/mu_calculus/mod.rs)
now flags only the *structural* caveat (a plain verification cube's
controllability partition is a build-time default).

## 30. Shoham & Grumberg, *A Game-Based Framework for CTL Counterexamples and 3-Valued Abstraction-Refinement* (LMCS 2007; CAV 2003 precursor)

**Theoretical core.** Casts 3-valued model checking as a game whose
positions are `(state, subformula)` pairs; the *indefinite* (`⊥`)
positions are exactly those whose resolution depends on a
may-but-not-must transition. Introduces **generalized KMTS (GKMTS)** with
*hyper-must* transitions (a must-edge into a *set* of abstract states),
which restore the monotonicity of refinement on alternating fixpoints
that plain KMTS lacks. The *failure subgame* extracted from the
indefinite positions localizes refinement.

**Map to mununu.** The basis for **R.4.5** (the
`MustHyperOnly { targets }` GKMTS variant), **R.5.0** (the 3-valued
parity-game evaluator + `FailureSubgame` extraction at
[`parity_game_3v.rs`](../../crates/mununu-core/src/mu_calculus/parity_game_3v.rs)),
and **R.5** (failure-subgame-driven CEGAR). [`kmts-theory.md`](kmts-theory.md)
§7.4 cites it for hyper-must completeness; the soundness warning at
[`cegar.rs`](../../crates/mununu-core/src/adapter/btor2/cegar.rs) (B.3.b)
that flags alternating-fixpoint verdicts on a non-hyper-must cube is the
practical consequence.

---

## Cross-reference matrix

This matrix is the contract between this doc and
[`auto-extraction-architecture.md`](auto-extraction-architecture.md) §2 (stage
mapping) and §5 (current-vs-proposed comparison). Entries 1–18 cover
the original pillow-plan / explicit-state pipeline. Entries 19–30 (the
KMTS, AGR, and game-based / GKMTS sections appended in the D.0 deliverable of
`you-are-a-formal-vast-lake.md` (a planning note, not tracked))
cover the KMTS pivot's literature anchors and lift verbatim into
[`native-sv-abstraction.md`](native-sv-abstraction.md),
[`kmts-theory.md`](kmts-theory.md), and
[`predicate-abstraction-recipe.md`](predicate-abstraction-recipe.md).

| # | Paper | mununu stage | Existing module touched | New module introduced | Phase |
|---|---|---|---|---|---|
| 1 | Kurshan 1994 — Localization | Stage 2 | [`adapter/domain.rs`](../../crates/mununu-core/src/adapter/domain.rs) (`Ignored` variant) | [`adapter/partition/`](../../crates/mununu-core/src/adapter/partition/) — **live (A.3)** | A.3 ✓ |
| 2 | Clarke et al. 2000 — CEGAR | Stage 6 | [`verify/orchestrator.rs`](../../crates/mununu-core/src/verify/orchestrator.rs) | `verify/refine.rs` | A.5 |
| 3 | Graf–Saidi 1997 — Predicate Abstraction | Stage 4 | [`kripke_smt.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs) | `adapter/sidecar/predicate_image.rs` | A.4 |
| 4 | Jain–Kroening TCAD 2008 — Word-Level | Stage 3 + 4 refinement | [`kripke_smt.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs) | `adapter/systemverilog/wp_refine.rs` | A.4–A.5 |
| 5 | VCEGAR TACAS 2007 | Stage 6 driver shape | [`crates/mununu-cli/src/main.rs`](../../crates/mununu-cli/src/main.rs) | `mununu verify --cegar` flag | A.5 |
| 6 | AVR NFM 2019 — SyGuS seed | Stage 3 | [`kripke.rs:scan_significant_constants`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs) | `adapter/sidecar/predicate_seed.rs` | A.4 |
| 7 | AVR TACAS 2020 — tool | Phase B reference | n/a | `adapter/btor2/avr_bridge.rs` | B (conditional) |
| 8 | Goel–Sakallah VMCAI 2021 — SyGuS lemmas | Future | n/a | `adapter/sidecar/sygus_extend.rs` | post-B |
| 9 | Andraus–Sakallah Reveal 2008 — Datapath | Stage 2 | [`adapter/domain.rs`](../../crates/mununu-core/src/adapter/domain.rs) | [`adapter/partition/mod.rs::DatapathUf`](../../crates/mununu-core/src/adapter/partition/mod.rs) (type shipped, heuristic deferred) | A.3 follow-up — see `phase-a3-followup-datapath-uf.md` (a planning note, not tracked) |
| 10 | Bryant–Kroening TACAS 2007 — BV abstraction | Stage 4 inner | [`kripke_smt.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs) | `adapter/sidecar/bv_abstraction.rs` | A.4 |
| 11 | Hoder–Bjørner–de Moura CAV 2006 — Fast PA | Stage 4 algorithm | [`kripke_smt.rs:enumerate_values`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs#L110) | `predicate_image.rs::all_abstract_edges` | A.4 |
| 12 | Cimatti IC3-IA TACAS 2014 | Phase B substrate | n/a | external | B (conditional) |
| 13 | Lahiri–Bryant TOCL 2007 — Indexed | Stage 4 extension (C) | n/a | `adapter/extraction/array_predicate.rs` | post-A |
| 14 | Yang–Goel–Sakallah FMCAD 2023 — Datapath in IC3 | Phase B engine choice | n/a | n/a | B (conditional) |
| 15 | BTOR2 / Boolector CAV 2018 | Stage 1 substrate | [`adapter/btor2/parser.rs`](../../crates/mununu-core/src/adapter/btor2/parser.rs) | n/a | A (cite) |
| 16 | rIC3 arXiv 2025 | Decision-gate baseline | n/a | n/a | A (gate) |
| 17 | rtl2µspec MICRO 2021 | Validation / template taxonomy | [`builtin_templates.json`](../../crates/mununu-core/src/adapter/templates/builtin_templates.json) | n/a | A (template doc) |
| 18 | Caliptra 2.0/2.1 spec | Empirical target | [`/tmp/caliptra_retry/`](file:///tmp/caliptra_retry/) | n/a | A.4–A.5 fixtures |
| 19 | Larsen–Thomsen 1988 — MTS | KMTS data model | [`clts/mod.rs`](../../crates/mununu-core/src/clts/mod.rs) (`Transition`) | `TransitionModality` enum addition | R.1 |
| 20 | Dams–Gerth–Grumberg 1997 — Mixed TS | KMTS data model (theoretical) | [`clts/mod.rs`](../../crates/mununu-core/src/clts/mod.rs) | n/a (mixed-system extension consciously omitted) | R.1 (cite) |
| 21 | Bruns–Godefroid CONCUR 2000 — 3-valued mu-calculus | KMTS evaluator | [`mu_calculus/evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs) | `mu_calculus/evaluator.rs` (bulk `EvalDomain` trait + `BoolDom` / `KleeneDom` instantiations, unified P2.2/P2.3) | R.3 |
| 22 | Huth–Jagadeesan–Schmidt TACAS 2001 — KMTS | KMTS evaluator + composition | [`composition/mod.rs`](../../crates/mununu-core/src/composition/mod.rs), [`clts/mod.rs`](../../crates/mununu-core/src/clts/mod.rs) | `state_3valued_predicates`, `TransitionModality` merge in `composition` | R.1, R.3 |
| 23 | Godefroid–Jagadeesan TACAS 2003 — CEGAR for KMTS | KMTS lifter refinement | n/a (new) | `adapter/btor2/kmts_lift.rs::refine` | R.5 |
| 24 | Larsen–Nyman–Wąsowski FoSSaCS 2007 — Modal I/O automata | Compositional KMTS soundness | [`composition/mod.rs`](../../crates/mununu-core/src/composition/mod.rs) | n/a (theorem; no algorithmic code) | R.1 (cite) |
| 25 | Pnueli 1985 — AGR | Optional sidecar assumptions | n/a | `SvAnnotation.assumptions: Vec<MuFormula>` | S.3 |
| 26 | McMillan CAV 1998 — Circular AGR | Deferred | n/a | n/a | deferred (§11) |
| 27 | Cobleigh–Giannakopoulou–Păsăreanu TACAS 2003 — L* AGR | Deferred | n/a | `verify/assumption_learning.rs` (hypothetical) | deferred (§11) |
| 28 | Cimatti–Tonetta FMCAD 2012 — OCRA contracts | Deferred | n/a | `verify/contract.rs` (hypothetical) | deferred (§11) |
| 29 | de Alfaro–Godefroid–Jagadeesan LICS 2004 — 3-valued games | Stage 6 (controllability verdict) | [`evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs) (`modal_trit_from_target` per-player rule) | per-player game audit (PO-3 / R.6.8 CLOSED 2026-06-23) | R.6 (closed) |
| 30 | Shoham–Grumberg LMCS 2007 — GKMTS + failure subgame | Stage 6 (refinement) | [`parity_game_3v.rs`](../../crates/mununu-core/src/mu_calculus/parity_game_3v.rs) | `MustHyperOnly` (R.4.5) + `FailureSubgame` (R.5.0) — **live** | R.4.5 / R.5.0 ✓ |

### Reading order if you only have time for three

For the original pillow-plan (explicit-state / Phase A) framing:

1. **AVR NFM 2019 (#6)** — closest published comparator to mununu; sets
   the publication-positioning baseline.
2. **Hoder–Bjørner–de Moura CAV 2006 (#11)** — concrete algorithm for
   the Stage 4 hot loop; biggest expected runtime win at Caliptra scale.
3. **Andraus–Sakallah Reveal LPAR 2008 (#9)** — closest formalisation of
   the `wait_count`-collapse pattern that the Caliptra abstraction
   analysis hand-derived; parent of Stage 2.

For the **KMTS pivot** framing (`native-sv-abstraction.md` + R.0–R.5):

1. **Bruns–Godefroid CONCUR 2000 (#21)** — the 3-valued mu-calculus
   preservation theorem; the load-bearing soundness argument that the
   entire KMTS pipeline rests on. Read this first.
2. **Huth–Jagadeesan–Schmidt TACAS 2001 (#22)** — KMTS definition +
   compositional model checking. The structural-free-lunch result is
   here, with the per-axis composition rule that mununu's modality
   merge implements verbatim.
3. **Godefroid–Jagadeesan TACAS 2003 (#23)** — CEGAR for KMTS. The
   `KleeneBot → refinement` loop's algorithmic core; §4 of
   [`predicate-abstraction-recipe.md`](predicate-abstraction-recipe.md)
   adopts this directly.

### Out of Phase A scope (cite, do not adopt)

- IC3-IA (#12), Datapath-in-IC3 (#14), AVR-tool (#7), rIC3 (#16) — all
  live inside an IC3 prover, which mununu does not have and does not
  intend to build in Phase A.
- SyGuS lemma generation (#8) — gated on Stage 3 producing stable
  syntactic seeds first.
- Indexed predicates (#13) — gated on the Stage 4 SMT-theory selector
  (`Theory::BvUfArray`).

### Out of KMTS-pivot scope (cite, do not adopt)

- Circular AGR (#26), L*-learning AGR (#27), OCRA contracts (#28) —
  classical compositional-verification machinery superseded by KMTS
  composition (§7 of the architecture doc). Cited as the foundational
  AGR references; not adopted because KMTS makes them unnecessary for
  mu-calculus verification. The optional sidecar `assumptions: Vec<MuFormula>`
  field (#25 sketch) is the degenerate-AGR interface mununu *does* offer.
- Mixed transition systems (#20) — the `R_must ⊆ R_may`-violating
  generalisation. Cited as theoretical context; mununu's standard-KMTS
  shape excludes it by construction (the predicate-image construction
  guarantees the invariant).
