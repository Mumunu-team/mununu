# Auto-Extraction Architecture — Current State, Proposed Unification, Roadmap

> **Status: planning.** §1 sub-sections that anchor live code carry their
> own `Source of truth:` lines; the architectural proposals in §2–§7 do
> not, since they describe code that does not yet exist. Once Phase A.3
> lands, §2 sub-sections graduate to live anchors. Companion doc:
> [`abstraction-literature.md`](abstraction-literature.md).

## Context

The Caliptra `soc_ifc_boot_fsm` proof-by-fire effort (see
[`caliptra-abstraction-analysis.md`](caliptra-abstraction-analysis.md))
walked into a 33.5 M-transition enumeration that the bit-blaster could
not complete in 22 minutes / 4.6 GB RSS. Phase 1.6 cleared the input-bit
cap; the remaining wall — wide state-cell enumeration of bit-vectors
that the property does not care about (e.g. `wait_count[7:0]` matters
only as `== 0` vs. `!= 0`) — is the same wall every hardware-verification
group has hit since the 1990s.

[`abstraction-literature.md`](abstraction-literature.md) catalogs the
25-year literature that formalises the solution: predicate abstraction,
datapath abstraction, cone-of-influence, CEGAR, SMT-driven predicate-image
enumeration, syntax-guided seeding, and (for the IC3 family) implicit
abstraction. mununu already implements partial, hand-driven versions of
most of these — `AbstractionType::Ignored` is localization, the
`discovered_values` sidecar field is a manually-populated predicate set,
`mununu sv discover` is a primitive predicate-image step. This doc
turns those scattered primitives into a unified six-stage pipeline,
declares which automation is mandatory and which remains a hand-authored
escape hatch, and proposes a roadmap with a measurable decision gate
between the incremental adaptation path (Phase A) and the clean-slate
alternative-engine path (Phase B).

## §1 Current mununu architecture (refresh)

### 1.1 Pipeline shape

> Source of truth: [`crates/mununu-core/src/verify/orchestrator.rs`](../../crates/mununu-core/src/verify/orchestrator.rs#L79) — surface: CLI+API

The current pipeline is:

```text
            ┌──────────────────┐    ┌──────────────────┐    ┌────────────────┐
source ───▶ │ FormatAdapter    │───▶│ AdapterIR        │───▶│ emit CTXDSL    │──┐
            │ (per-format)     │    │ + value-name map │    │ (round-trip)   │  │
            └──────────────────┘    └──────────────────┘    └────────────────┘  │
                                                                                ▼
            ┌──────────────────┐    ┌──────────────────┐    ┌────────────────┐
verdict ◀── │ mu-calculus eval │◀───│ composed Clts    │◀───│ realize_context│
            │ + synthesis      │    │ (after composition)│  │ (parse+merge)  │
            └──────────────────┘    └──────────────────┘    └────────────────┘
```

`verify_project()` (orchestrator.rs#L79) consumes `verify.toml`,
dispatches per-`[[sources]]` adapter, applies alphabet bindings, assembles
a unified CTXDSL string, parses it, realizes it into a `Context`, then
evaluates each `[[properties]]` entry.

### 1.2 Adapters

> Source of truth: [`crates/mununu-core/src/adapter/mod.rs`](../../crates/mununu-core/src/adapter/mod.rs#L62) — surface: CLI+API+UI

mununu ships eleven adapters that all implement `FormatAdapter`:

| Adapter | Role |
|---|---|
| `aiger` | AIGER bit-vector games → turn-based explicit automaton |
| `btor2` | BTOR2 word-level → bit-blaster → cross-product enumeration |
| `systemverilog` | Custom SV-AST parser + Kripke builder + optional SMT discovery |
| `extraction` | `.espec.json` source-anchored specs → explicit automata |
| `promela` | Spin models → bounded-variable explicit automata |
| `tlsf` | TLSF synthesis specs → turn-based signal-state encoding |
| `xstate` | XState v5 JSON → explicit automaton |
| `crewai` | Multi-agent workflows → automaton (agentic) |
| `langgraph` | LangGraph workflows → automaton (agentic) |
| `microcode` | Microprogram assembly → automaton |
| `yosys` | Driver/frontend that calls Yosys (yosys-slang `read_slang`), emits BTOR2 |

Only `ctxdsl` and `xstate` dispatch through the verify orchestrator
today; the others run via per-format CLI sub-commands and feed into
verify by emitting CTXDSL that the user references explicitly.

### 1.3 Abstraction primitives

> Source of truth: [`crates/mununu-core/src/adapter/domain.rs`](../../crates/mununu-core/src/adapter/domain.rs#L22) — surface: CLI+API

The `AbstractionType` enum has five variants today:

| Variant | Cardinality | Preserves |
|---|---|---|
| `Boolean` | 2 | true / false distinction |
| `Presence` | 2 | "value is present" vs. absent |
| `BoundedCounter` | `bound − lower + 1` | concrete value in range |
| `EnumValues` | `variants.len()` | named variant set (with optional value map) |
| `Ignored` | 1 (pinned) | nothing — equivalent to Kurshan localization for that field |

`FieldDomain` (domain.rs#L63) wraps the variant with bounds, initial
value, and an optional `value_map`. `SignalAbstraction::Discover` (the
sidecar's sixth pseudo-variant) is *not* an `AbstractionType` — it is a
*directive* that gets resolved into `EnumValues` once `discovered_values`
is populated by either SMT discovery or hand-edit; the resolver is at
[`adapter/sidecar/mod.rs:130`](../../crates/mununu-core/src/adapter/sidecar/mod.rs#L130).

### 1.4 State enumeration

> Source of truth: [`crates/mununu-core/src/adapter/state_enum.rs`](../../crates/mununu-core/src/adapter/state_enum.rs#L14) — surface: CLI+API

`enumerate_cross_product(&[&FieldDomain]) -> Vec<AbstractState>`
materialises the Cartesian product of per-field value sets, dropping
`Ignored` fields. Both the SV-AST Kripke builder and the BTOR2
bit-blaster use it. BTOR2 caps: `MAX_STATE_BITS=20`,
`MAX_INPUT_BITS=10` (raised from 16 → 20 in commit
[7837ef0](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs)).

### 1.5 Sidecar schema

> Source of truth: [`crates/mununu-core/src/adapter/systemverilog/annotation.rs`](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs#L30) — surface: CLI+API

`SvAnnotation` (`mununu_sv_annotation_v1`) carries:

- `signals[]` — per-state-cell `SignalAnnotation` (name, abstraction,
  bound, variants, value_map, combinational flag)
- `inputs[]` — per-input `InputAnnotation` (same shape)
- `controllable[]` — labels marked controllable
- `properties[]` — `PropertyAnnotation` (id, formula, role, template_ref)
- `discovered_values{}` — `name → { values[], catch_all }`
- `parameters{}` — module parameter overrides

`MultiModuleSvAnnotation` (`mununu_sv_multi_v1`) adds `connections[]`
for cross-module wiring.

The same resolver consumes both the SV-AST path and the BTOR2 path —
the schema is **format-agnostic by construction**.

### 1.6 SMT discovery (today)

> Source of truth: [`crates/mununu-core/src/adapter/sidecar/predicate_image/all_smt.rs`](../../crates/mununu-core/src/adapter/sidecar/predicate_image/all_smt.rs) — surface: CLI (`mununu btor2 discover`)

`discover_design_values(&Btor2File, …)` walks the BTOR2 IR, asks Z3 for
satisfying constants per state cell (SMT predicate-image), and writes the
result into the sidecar's `discovered_values` map. (S.2b removed the
native-SV-AST predecessor, `kripke_smt::discover_significant_values`,
which required the hand-rolled SV parser; the BTOR2-IR path operates on
the same IR the verify pipeline uses and is therefore the canonical
discovery for the `sv-yosys` flow.)

### 1.7 Mu-calculus

> Source of truth: [`crates/mununu-core/src/mu_calculus/mod.rs`](../../crates/mununu-core/src/mu_calculus/mod.rs#L264) — surface: CLI+API

The `Node` enum carries `True | False | Predicate | Variable | Not |
And | Or | Modal | Mu | Nu`. The `Guard { labels, current, next,
control, max_steps }` (mu_calculus/mod.rs#L323) constrains modalities
across five axes; `Control` is `All | Controllable | Environment`.
The evaluator at
[`mu_calculus/evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs#L348)
operates on the composed `Clts` and returns a `BitVec` per fixpoint
variable, with `IterationRanks` for signature-based controller
extraction.

### 1.8 Composition

> Source of truth: [`crates/mununu-core/src/composition/mod.rs`](../../crates/mununu-core/src/composition/mod.rs#L389) — surface: CLI+API

`CompositionSemantics { Synchronous | Asynchronous | Superset }` and
`compose(left, right, options)` (composition/mod.rs#L440) produce a
BFS-reachable product CLTS. The realize pipeline at
[`context_dsl/realize.rs:compose_and_register`](../../crates/mununu-core/src/context_dsl/realize.rs#L1180)
chains composition operations declared in the CTXDSL `compositions {}`
block.

### 1.9 Property templates

> Source of truth: [`crates/mununu-core/src/adapter/templates/mod.rs`](../../crates/mununu-core/src/adapter/templates/mod.rs#L210) — surface: CLI (`mununu templates`) + API (`/api/templates`)

`TemplateRegistry::builtin()` ships ~15 universal templates
(`no_deadlock`, `reachable`, `never`, `always_eventually`, `bounded`,
`response`, `mutual_exclusion`, …) plus three agentic templates
(`bounded_handoff`, `no_delegation_cycle`, `eventual_completion`).
Templates use `${PARAM}` placeholders in `formula_pattern` and are
instantiated via `TemplateRegistry::instantiate(&TemplateRef)`.

### 1.10 Verify orchestrator

> Source of truth: [`crates/mununu-core/src/verify/orchestrator.rs`](../../crates/mununu-core/src/verify/orchestrator.rs#L79) — surface: CLI (`mununu verify`)

`VerifyConfig` (config.rs#L70): `[project]`, `[[sources]]`,
`[alphabet { direct | renamings | register_map }]`, `[composition]`,
`[[properties]]`. Alphabet bindings rewrite labels across composed
sources via word-boundary textual substitution
([`verify/binding.rs:34`](../../crates/mununu-core/src/verify/binding.rs#L34)).
`RegisterMap` strategy drives firmware-peripheral rendezvous labels via
[`coupling::rendezvous_label_name`](../../crates/mununu-core/src/coupling/mod.rs).

---

## §2 Proposed unified auto-extraction architecture

The proposed pipeline preserves the explicit-state CLTS surface (which
gives mununu its mu-calculus and synthesis distinctiveness) and adds
five new stages that automate what is today hand-driven. The pipeline
remains:

```text
source ─▶ Stage1 ─▶ Stage2 ─▶ Stage3 ─▶ Stage4 ─▶ Stage5 ─▶ Stage6 ─▶ verdict
         ingest    partition  seed     image     resolve   enumerate
         + AST     (COI+      (syntactic (SMT     to Field  + emit
                   datapath)  preds)    preds)    Domain    + CEGAR
```

### Stage 1 — Source ingest & frontend AST

**No change.** Per-format frontends produce a typed IR
(`adapter::systemverilog::ast::Module`, `adapter::btor2::Btor2File`,
`adapter::extraction::ExtractionAst`, `xstate::Machine`, etc.). This is
the substrate every later stage reads.

**Landing.** [`adapter/`](../../crates/mununu-core/src/adapter/) (existing).
**Papers.** BTOR2/Boolector CAV 2018 (#15); spec/format contract only.

### Stage 2 — Cone-of-influence + control/data partition

> Source of truth: [`adapter/partition/mod.rs`](../../crates/mununu-core/src/adapter/partition/mod.rs)
> — surface: CLI+API (transparent; `partition_summary` field in
> `VerifyReport`).
> **Status: live (Phase A.3 step 3.5).** COI half implemented and wired into
> SV + BTOR2 adapters. Datapath UF substitution deferred to
> `phase-a3-followup-datapath-uf.md` (a planning note, not tracked).

The `adapter::partition` module computes per-signal classifications
(`Kept` / `Dropped { reason }` / `Datapath { uf_symbol }`) via the
[`DepGraphBuilder`](../../crates/mununu-core/src/adapter/partition/dep_graph.rs)
trait. SV ([`Module`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs))
and BTOR2 ([`Btor2File`](../../crates/mununu-core/src/adapter/btor2/dep_graph.rs))
implement the trait against their frontend IRs; the extraction adapter
ships a preview-only impl ([`ExtractionSpec`](../../crates/mununu-core/src/adapter/extraction/dep_graph.rs)).
The `Dropped` classifications convert to
[`AbstractionType::Ignored`](../../crates/mununu-core/src/adapter/domain.rs)
in the sidecar resolver via [`apply_partition_drops`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs)
on BTOR2 and the inline rule in
[`kripke::build_kripke_with_config`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs)
on SV.

**Live Rust shape** (matches the shipped types — see
[`adapter/partition/mod.rs`](../../crates/mununu-core/src/adapter/partition/mod.rs)):

```rust
// adapter/partition/mod.rs
pub enum PartitionClass {
    Kept,
    Dropped { reason: &'static str },
    Datapath { uf_symbol: String },  // reserved; not produced in A.3
}

pub struct Partition {
    pub classes: BTreeMap<String, PartitionClass>,
    pub datapath_uf: BTreeMap<String, DatapathUf>,  // empty in A.3
}

pub fn classify<B: DepGraphBuilder>(
    builder: &B,
    property_atoms: &HashSet<String>,
    opts: &PartitionOptions,
) -> Partition;
```

**Composition rule.** User wins on collision: any signal explicitly
listed in the sidecar (keyed by name in
[`MergedConfig::signal_domains`](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs)
on SV; present in the resolver's `cell_domains` / `input_domains` map
on BTOR2) is immune to auto-drop. Empty-seeds short-circuit (when no
property atoms are extractable) keeps everything to avoid silent
soundness regressions.

**Telemetry.** Each adapter populates
[`AdapterOutput.partition_summary`](../../crates/mununu-core/src/adapter/mod.rs)
with `PartitionSummary { total_signals, kept, dropped_coi, datapath_uf,
state_bits_before, state_bits_after }` (widths populated by BTOR2,
`None` on SV). The orchestrator threads this through
[`SourceSummary.partition_summary`](../../crates/mununu-core/src/verify/report.rs)
in `VerifyReport`.

**Papers.** Kurshan 1994 (#1), Andraus–Sakallah Reveal LPAR 2008 (#9).

### Stage 3 — Predicate seeding (syntax-guided)

**New.** A new module `adapter/sidecar/predicate_seed.rs` scans the
frontend AST for sub-expressions that should become predicates: case
labels, RHS of `signal == constant`, width slices, comparator
operands. Pure-syntactic; no SMT calls. Emits a `Vec<PredicateSeed>`
keyed by signal name, which Stage 4 consumes as its initial predicate
bank.

**Rust shape.**

```rust
// adapter/sidecar/predicate_seed.rs (NEW)
pub struct PredicateSeed {
    pub signal: String,                 // → FieldDomain.name
    pub witnesses: Vec<i64>,            // becomes DiscoveredValue list
    pub rationale: &'static str,        // "case label" | "== const" | …
}

pub fn collect_syntactic_predicates<F: FrontendIR>(ir: &F) -> Vec<PredicateSeed>;
```

**Landing.** Folded into existing
[`kripke::scan_significant_constants`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs)
(SV) and a new sibling on the BTOR2 side
(`adapter/btor2/predicate_seed.rs`).

**Papers.** AVR NFM 2019 (#6); syntactic half of Jain–Kroening TCAD 2008 (#4).

### Stage 4 — SMT predicate-image discovery

**New algorithm; same data contract.** Today
[`kripke_smt.rs:enumerate_values`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs#L110)
runs one Z3 query per guard per signal, extracting constants — *per
signal in isolation*. The new algorithm (per Hoder–Bjørner–de Moura CAV
2006) does an **all-SMT enumeration of abstract predicate tuples** under
the transition relation, sharing clause learning across the full
predicate set. The output schema (`discovered_values`) is unchanged —
downstream code (Stage 5+) sees no difference.

**Two critical sub-features.**

1. **BTOR2 generalisation.** A new `Btor2PredicateBank` builder lets the
   BTOR2 path run Stage 4 *without* requiring the custom SV parser.
   This is what unblocks Caliptra after sv2v preprocessing.
2. **SMT theory selector.** Stage 4 must accept a
   `Theory { BvOnly, BvUfArray }` parameter. SV / BTOR2 callers pass
   `BvOnly`; the C-extraction path must wait for `BvUfArray` support
   before consuming Stage 4 (see §7 risk).

**Rust shape.**

```rust
// adapter/sidecar/predicate_image.rs (NEW)
pub enum Theory { BvOnly, BvUfArray }

pub struct PredicateImage<'ctx> {
    pub theory:       Theory,
    pub solver:       z3::Solver<'ctx>,
    pub predicates:   Vec<(String, z3::ast::Bool<'ctx>)>,
    pub transition:   z3::ast::Bool<'ctx>,
}

impl<'ctx> PredicateImage<'ctx> {
    pub fn all_abstract_edges(&self, cap_edges: usize)
        -> Vec<AbstractTransition>;   // (from_assignment, to_assignment) pairs
}
```

**Landing.** Replaces the per-guard loop in `kripke_smt.rs`; new
sibling for BTOR2 at `adapter/btor2/predicate_image.rs` that builds the
SMT context from the BTOR2 IR rather than the SV AST.

**Papers.** Hoder–Bjørner–de Moura CAV 2006 (#11), Graf–Saidi CAV 1997
(#3), Jain–Kroening TCAD 2008 (#4); Bryant–Kroening TACAS 2007 (#10) as
the inner-loop under/over-approximation fallback when queries saturate.

### Stage 5 — Domain resolution

**No structural change.** The existing
[`sidecar::resolve_to_field_domain`](../../crates/mununu-core/src/adapter/sidecar/mod.rs#L49)
and
[`btor2_resolver::build_field_domains_for_btor2`](../../crates/mununu-core/src/adapter/sidecar/btor2_resolver.rs)
already convert sidecar entries into `FieldDomain`. The only additive
change: a new `AbstractionType::Predicate { name, witness }` variant in
[`adapter/domain.rs`](../../crates/mununu-core/src/adapter/domain.rs)
*if* Stage 4 ever needs to carry a predicate as a first-class field
domain (rather than encoding it as `EnumValues` via the
`discovered_values` round-trip).

**Default path** for Phase A: do not add a new variant; let Stage 4 write
into `discovered_values` and reuse the `Discover → EnumValues` resolver
unchanged. This minimises downstream impact.

**Papers.** N/A (no algorithmic content).

### Stage 6 — Enumeration + CLTS emit + CEGAR loop

**Existing enumeration unchanged; new refinement wrapper.** The
cross-product enumerator at
[`state_enum.rs:enumerate_cross_product`](../../crates/mununu-core/src/adapter/state_enum.rs#L14)
and the realize pipeline at
[`context_dsl/realize.rs:realize_context`](../../crates/mununu-core/src/context_dsl/realize.rs#L1464)
remain the load-bearing primitives. A new `verify::refine` module wraps
[`verify::orchestrator::verify_project`](../../crates/mununu-core/src/verify/orchestrator.rs#L79)
in a CEGAR driver:

```rust
// verify/refine.rs (NEW)
pub enum CegarStep {
    Decided(VerifyReport),
    Spurious { new_predicates: Vec<PredicateSeed> },
}

pub fn verify_with_refinement(
    config: &VerifyConfig,
    opts: CegarOptions,                 // max_iterations, wp_refine, seed_strategy
) -> Result<VerifyReport, VerifyError>;
```

On a `False` mu-calculus verdict, the driver: (a) lifts the witness from
the report; (b) runs the WP-style refinement rule of Jain–Kroening TCAD
2008 over the witness trace to propose new predicate atoms; (c) merges
them into the sidecar's `discovered_values`; (d) re-realises and
re-evaluates. Iteration cap (default 5) terminates non-convergent loops.

**Papers.** CEGAR CAV 2000 (#2), VCEGAR TACAS 2007 (#5), WP-refinement
half of Jain–Kroening TCAD 2008 (#4).

---

## §3 What stays hand-authored

Per the user's automation policy (decided in Phase 1.5 of the
governing plan):

| Artefact | Authorship | Rationale |
|---|---|---|
| Behaviour models (CLTS) | **Adapter + sidecar only** | The whole point of the unified pipeline |
| Stub modules for black-box peers / unmodelled environments | **Hand-authored CTXDSL allowed** | Matches [`black-box-modules.md`](black-box-modules.md); chaotic-stub default still applies |
| Sidecar JSON (`.mununu.json` / `.espec.json`) | **Configuration; written by hand or by `mununu sv init`** | Declarative; not a model |
| Mu-calculus properties | **Hand-written OR template-instantiated** | Both flows supported today; no change |
| `verify.toml` orchestration | **Hand-authored** | Project-level glue; small surface |
| Alphabet bindings (`renamings` / `register_map`) | **Hand-authored or auto-derived from register map** | Same as today |

The boundary "stub module for black-box peer" is the **only** place
hand-authored CTXDSL behaviour persists in the new pipeline. Everything
that has a source artefact (SV, BTOR2, C, XState, etc.) must flow
through adapter + sidecar. The realize pipeline is unchanged, so this
is a *policy* boundary, not a code-path boundary.

---

## §4 Where the literature does *not* fit

mununu commits to an **explicit-state CLTS** as the substrate for
mu-calculus evaluation, controller synthesis, and composition. That
commitment is load-bearing — it's how mununu gets `ControllerMode`,
`Guard { control, … }`, multi-label transitions, and signature-based
strategy extraction. Three families of techniques from the literature
do *not* fit this substrate and must be cited only as Phase B
alternatives:

**IC3-IA (Cimatti et al. TACAS 2014, #12).** Implicit predicate
abstraction's whole point is that the abstract transition relation is
*never materialised*. mununu's `AbstractState = BTreeMap<String,
AbstractValue>` and `enumerate_cross_product` are explicit
materialisation. Adopting IC3-IA in Phase A would require either (a) a
wrapper that re-enumerates the implicit relation (defeating the
purpose), or (b) abandoning the explicit-state CLTS for IC3-IA queries
(abandoning mu-calculus and synthesis). Neither is acceptable. Cite as
Phase B substrate.

**Datapath propagation in IC3 (Yang–Goel–Sakallah FMCAD 2023, #14).**
Same blocker — the algorithm lives inside an IC3 prover's
generalise-cube step. The Andraus–Sakallah Reveal *partition* (replace
ALU with UF) *is* portable to mununu's BTOR2 path and lands in Stage 2;
the FMCAD 2023 propagation rule is not.

**AVR's full refinement loop (Goel–Sakallah NFM 2019, #6).** AVR refines
predicates *inside* an IC3 inductive proof. mununu has no inductive
prover; the mu-calculus evaluator
([`evaluator.rs:348`](../../crates/mununu-core/src/mu_calculus/evaluator.rs#L348))
is a fixpoint over the materialised graph. Borrow Stage 3's *syntactic
seed* (the SyGuS sub-term collection is theory-agnostic) but drive
refinement from mu-calculus counterexample traces, not from IC3 frame
analysis.

---

## §5 Comparison table — current vs Phase A vs Phase B

| Capability | Today | Phase A (incremental) | Phase B (clean-slate, conditional) |
|---|---|---|---|
| Cone-of-influence | Hand-marked `Ignored` | **Automatic via `adapter::partition`** | Same as A (Phase B reuses) |
| Datapath abstraction | Hand-collapsed (e.g. `wait_count` enum) | **Automatic UF substitution for wide arithmetic** | Same as A |
| Predicate seeding | Case-label scrape (SV only) | **AST sub-term collection (SV + BTOR2)** | Same as A |
| Predicate-image discovery | Per-guard constant enumeration (SV-AST only) | **All-SMT predicate-tuple enumeration; BTOR2 generalised** | IC3-IA implicit; mununu acts as front-end |
| State enumeration | Cross-product over `FieldDomain` | Same; uses richer domains from Stage 4 | Skipped — IC3 reasons over implicit relation |
| CEGAR refinement | None | **WP-driven refinement loop in `verify::refine`** | IC3-IA-internal refinement |
| Mu-calculus evaluation | `evaluator.rs` over materialised CLTS | Unchanged | Verdict comes from external IC3-IA engine; mununu lifts trace back to CLTS for synthesis |
| Controller synthesis | 6 `ControllerMode` variants | Unchanged | Unchanged once verdict + invariant land back in mununu |
| Property templates | 15 universal + 3 agentic + N RTL | Same, plus optional WP-derived candidate templates for CEGAR seeds | Same |
| BTOR2 state-bit cap | 20 | 20 in Phase A.3; expected drop after partition shrinks effective bits | n/a (IC3 not bit-blast-bounded) |
| Caliptra `soc_ifc_boot_fsm` end-to-end | 22 min / 4.6 GB / killed | **Target: < 1 min** after Stage 2 + Stage 4 land | (Conditional) IC3-IA verdict in seconds |

---

## §6 Roadmap & decision gate

| Phase | Deliverable | Effort | Done-criterion |
|---|---|---|---|
| **A.1** | This file + [`abstraction-literature.md`](abstraction-literature.md) | 1–2 d | Cross-reference matrix consistent; `/docs-traceability` passes |
| **A.2** | (Folded into A.1 — single doc-writing phase) | — | — |
| **A.3** | Implement Stage 2 (`adapter::partition`) | 1 wk | Caliptra `soc_ifc_boot_fsm` Yosys-emitted BTOR2: COI shrinks `wait_count`'s data-flow ancestors to `Ignored` automatically; integration test against `soc_ifc_boot_fsm_pre_fix.sv` passes |
| **A.4** | Implement Stage 4 (`predicate_image.rs`) for BTOR2 (`Theory::BvOnly`) | 2 wk | `mununu sv discover --engine predicate-image` extended; ≥ 80% recall vs hand-curated significant values on a 10-design benchmark; SOUNDNESS notes per policy |
| **A.5** | CEGAR refinement driver in `verify::refine` | 1 wk | On `False` verdict, refinement converges in ≤ 5 iterations on benchmark; new SOUNDNESS annotation block |
| **Decision gate** | Measurement: Caliptra runtime + Stage 4 recall + CEGAR convergence | 1 d | If all gates pass, Phase B is deferred indefinitely; if any fail, B starts |
| **B.0** | (Conditional) New `adapter/btor2/ic3ia_bridge.rs` adapter; coexists with explicit path | 6–10 wk | Outside this plan's scope |

### Decision-gate thresholds

These thresholds determine whether the incremental Phase A pipeline is
sufficient or whether Phase B (an IC3-IA-class external engine) must be
opened. The gate runs *after* A.5 lands; if any one threshold fails,
Phase B starts.

1. **Runtime gate.** Caliptra `soc_ifc_boot_fsm` end-to-end (Yosys →
   BTOR2 → Stage 2 → Stage 3 → Stage 4 → bit-blast → eval) must
   complete in **< 5 min wall-clock** AND **< 2 GB RSS**. Today's
   baseline is 22 min / 4.6 GB / killed; anything not breaking 5 min
   means the algebra is wrong, not the constants.
2. **Discovery recall gate.** On a benchmark of ≥ 10 designs (Caliptra
   subset + `examples/industrial/` + a curated BTOR2 set), Stage 4's
   `discovered_values` must cover **≥ 80%** of the manually-curated
   significant-value set in the corresponding hand-modelled CTXDSL on
   **every** benchmark. Below 50% on any 3+ designs triggers Phase B.
3. **CEGAR convergence gate.** On a property known to be `True`, the
   refinement loop must terminate in **≤ 5 iterations** with monotone
   state-space growth. If any benchmark loops > 10 times or oscillates,
   the explicit-state primitive is the wrong substrate for refinement
   and Phase B is justified.

The benchmark set itself is a sub-task of Phase A.4 — today only
Caliptra has a hand-curated significant-value baseline, and the
80%-recall threshold is unverifiable until 9 more designs join it.

### Comparison baseline for the runtime gate

If a Phase A.4 verdict against Caliptra-class BTOR2 designs takes more
than **2× the runtime of rIC3** (arXiv 2502.13605, #16) on the same
BTOR2 input, that is also sufficient grounds to open Phase B —
mununu's value-add (mu-calculus, synthesis, composition) does not
justify a 2× runtime penalty on the verification core.

---

## §7 Risks & open questions

### Risk 1 — Stage 4 SMT theory mismatch (soundness regression)

A single `Theory::BvOnly` predicate-image engine would silently produce
*unsound* `discovered_values` on the C-extraction path
([`adapter/extraction/`](../../crates/mununu-core/src/adapter/extraction/)).
C programs have pointer comparisons, structs, and signed arithmetic
that need `QF_BV + QF_UF + arrays` (`Theory::BvUfArray`). If Stage 4 is
called from the extraction adapter with `Theory::BvOnly`, the SMT solver
returns spurious values that are then frozen into `discovered_values`
and the downstream enumeration is *incorrect*, not just imprecise.

**Mitigation.** Stage 4's `PredicateImage` constructor takes a `Theory`
parameter; the extraction adapter must pass `BvUfArray` and Stage 4
must hard-error (not warn) if `BvUfArray` is not implemented yet.
Document as a hard precondition for sharing Stage 4 across SV and
extraction adapters.

**Status.** Phase A ships `Theory::BvOnly`. Extraction-adapter callers
into Stage 4 are gated off until `Theory::BvUfArray` lands (post-A,
likely Phase A.4b).

### Risk 2 — Cross-document drift

The cross-reference matrix in
[`abstraction-literature.md`](abstraction-literature.md) is consumed by
§2 (stage mapping) and §5 (comparison table) of this doc. If either
side adds a paper or renames a stage without updating the other, the
docs go out of sync and downstream readers cannot follow the trail.
**Mitigation.** Every edit to either doc must verify the matrix
manually until a `/docs-traceability` extension can validate it.

### Risk 3 — Benchmark scarcity for the decision gate

The 80% recall threshold requires hand-curated significant-value
baselines on ≥ 10 designs. Today only Caliptra has one. Phase A.4 must
include benchmark curation as a sub-task; without it the gate is
unverifiable and Phase B / no-Phase-B becomes a judgment call.

### Risk 4 — Stub-module composition under sidecar abstraction

When a stub module (hand-authored CTXDSL, per §3) composes with an
adapter-extracted module that uses Stage 4 abstractions, the alphabet
binding must still match exactly. **Mitigation.** Reuse the existing
register-map rendezvous path
([`coupling::rendezvous_label_name`](../../crates/mununu-core/src/coupling/mod.rs))
and require stubs to declare their alphabet against the same
canonical names the adapter emits. No new wiring needed; this is a
*policy* enforced by the verify orchestrator's alphabet linter.

### Open question 1 — When does `AbstractionType::Predicate` become first-class?

The Phase A plan keeps Stage 5 unchanged (Stage 4 writes into
`discovered_values`; resolver converts to `EnumValues`). If Stage 4's
predicate set grows beyond ~32 entries per signal (the current
`MAX_VALUES_PER_SIGNAL`), the round-trip through `EnumValues` becomes
inefficient and a new `AbstractionType::Predicate { name, witness }`
variant becomes attractive. Defer until empirical evidence in Phase
A.4 measurement.

### Open question 2 — How does Phase B integrate with synthesis?

Phase B's IC3-IA verdict comes back as `holds` / `fails-with-cex` /
`unknown`. mununu's controller synthesis
([`context/mod.rs:ControllerMode`](../../crates/mununu-core/src/context/mod.rs#L1293))
operates on a CLTS; an IC3-IA verdict gives a witness trace, not a
plant model. **Tentative plan.** If verdict is `holds`, synthesis is
trivial (no controller needed for a safety property). If `fails`,
synthesis is moot. The interesting case — `holds` for a property that
admits a controller — would still require building the explicit CLTS
for synthesis purposes; in that case Phase B saves nothing on the
synthesis side. This needs more thought before Phase B starts.

---

## See also

- [`abstraction-literature.md`](abstraction-literature.md) — paper catalog + cross-reference matrix.
- [`caliptra-abstraction-analysis.md`](caliptra-abstraction-analysis.md) — empirical baseline (Phase 1.7).
- [`proof-by-fire-findings.md`](proof-by-fire-findings.md) — predecessor; documents the three systemic blockers.
- [`auto-extraction-real-bug-gap.md`](auto-extraction-real-bug-gap.md) — scope audit of auto-extraction × real-upstream-bug.
- [`black-box-modules.md`](black-box-modules.md) — stub-module policy (§3 reference).
- [`../abstraction.md`](../abstraction.md) — per-subsystem abstraction recipe; the user-facing companion to this design doc.
- [`../synthesis.md`](../synthesis.md) — controller synthesis modes; the unchanged half of the pipeline.
- [`../policies/claims-integrity.md`](../policies/claims-integrity.md) — applies when Phase A's first Caliptra PoC ships as a public example.
- `create-a-plan-to-enumerated-pillow.md` (a planning note, not tracked) — the governing plan; this doc is its Deliverable B.
