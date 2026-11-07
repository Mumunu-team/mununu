# Native SV → KMTS Architecture

> **Status: planning.** This document describes a unified replacement for mununu's SystemVerilog extraction pipeline. The current native-parser path (§2) is documented here for context and slated for removal (§9). The proposed pipeline (§3–§7) and the validation milestones (§10) are not yet implemented; planning sub-sections do not carry `> Source of truth:` anchors. Sections that document existing code (§2, §5.1 baseline, §9 removal inventory) do anchor live symbols. Companion docs once shipped: [`kmts-theory.md`](kmts-theory.md), [`predicate-abstraction-recipe.md`](predicate-abstraction-recipe.md), [`abstraction-literature.md`](abstraction-literature.md), [`post-rf5-architecture.md`](post-rf5-architecture.md) (the end-to-end engine picture: explicit vs symbolic, IR layering, may/must + over/under/⊥ approximation).

## §1 Context

Two pressures converged on the same architectural answer.

**External technical review.** A reviewer with hardware-verification background argued that mununu's SV extraction should cut the toolchain between *elaboration* and *netlist optimisation* — sv2v normalises SV-2017 to a Verilog-2005 subset (preserving hierarchy and signal names), Yosys runs `read_verilog` + `hierarchy -check` + `proc` + `opt -fast -purge` *without `flatten`* and emits one BTOR2 per submodule, and a per-module lifter produces the verification IR. BTOR2 (not AIGER) because it retains word-level structure, named registers, and named I/O — all of which feed mununu's valuations and labels directly. On top of this frontend, the reviewer proposed predicate abstraction lifted into a **Kripke Modal Transition System** (KMTS; Larsen–Thomsen 1988; Bruns–Godefroid CONCUR 2000) evaluated with **3-valued mu-calculus** (Godefroid–Jagadeesan TACAS 2003) — the only abstraction framework that is uniformly sound for both reachability and liveness under a single abstract model with refinement well-understood.

**User priority shift: synthesis is no longer a priority.** The bulk of mununu's native-SV complexity exists to support synthesis — controller emission ([`emit_controller.rs`](../../crates/mununu-core/src/adapter/systemverilog/emit_controller.rs)), typedef-enum FSM extraction for human-readable state names ([`fsm.rs`](../../crates/mununu-core/src/adapter/systemverilog/fsm.rs)), `value_map` symbolic-constant binding for synthesised-state preservation, OOB sentinel routing for synthesis-safe transitions, internal-net controllability discipline. With synthesis deprioritised, this machinery becomes dead weight.

Together: KMTS is the right verification architecture, the existing SV extraction code is over-engineered for that target, and the two cleanups land together. The Yosys/AVR symbolic-engine bridge described in [`post-b0-architecture.md`](post-b0-architecture.md) is unaffected — it handles cases beyond any explicit-state representation (safety only, no synthesis, no mu-calculus). KMTS-via-BTOR2-per-module is its sibling for the in-process verification path that supports mu-calculus and synthesis (when synthesis is re-prioritised).

**Singular-pipeline commitment.** By the end of the simplification phase (§9) and UI parity phase (§10.1 phase U.0), mununu has *exactly one* SV extraction pipeline — the KMTS path described here. The current native-parser path is deleted, not deprecated; no `--engine native-sv` escape hatch survives. Any SV construct the new pipeline cannot handle becomes a documented gap to close in the lifter (§3), not a reason to keep a parallel path.

## §2 What the native SV adapter does today

This section documents the current implementation for posterity. §9 lists what disappears.

### §2.1 Pipeline shape

> Source of truth: [`SystemVerilogAdapter::translate_with_path`](../../crates/mununu-core/src/adapter/systemverilog/mod.rs#L49) — surface: CLI+API

```text
Single source:
  foo.sv ──► parser::parse_with_warnings ──► Module AST
                       │
                       ▼
       annotation::find_sidecar (foo.mununu.json)
                       │
                       ▼
       annotation::merge_config (sidecar ⊕ // @mununu)
                       │
                       ▼
       kripke::build_kripke_with_config
       ├── build_registers_from_config
       ├── scan_significant_constants       (syntactic seed)
       ├── kripke_smt::discover_significant_values   (z3, per-guard)
       ├── COI prune  (drop unreferenced regs)
       ├── state_enum::enumerate_cross_product
       ├── extract_initial_state
       ├── compute_next_state per (state, input) — OOB → sentinel
       ├── prune_unreachable_states  (BFS)
       └── build_automaton_spec
                       │
                       ▼
       adapter::emit::emit ──► CTXDSL ──► realize ──► Clts ──► eval/synth

Multi-module (mununu_sv_multi_v1):
  *.sv + sidecar.json ──► per-module build_kripke_with_config
                                    │
                                    ▼
                          annotate_driving_output_labels
                          (shared labels on connections)
                                    │
                                    ▼
                          composition directive
                          (sync | async)
                                    │
                                    ▼
                          one AdapterOutput with CompositionSpec
```

### §2.2 Module-by-module surface

> Source of truth: directory [`crates/mununu-core/src/adapter/systemverilog/`](../../crates/mununu-core/src/adapter/systemverilog/) — surface: CLI+API

- [`mod.rs`](../../crates/mununu-core/src/adapter/systemverilog/mod.rs) (~2 348 lines). `SystemVerilogAdapter` adapter struct; three entry points (`translate_with_path`, `translate_multi_module`, `translate_multi_module_content`); sidecar discovery; multi-module orchestration; [`annotate_driving_output_labels`](../../crates/mununu-core/src/adapter/systemverilog/mod.rs#L511) shim for shared-label connectivity.
- [`parser.rs`](../../crates/mununu-core/src/adapter/systemverilog/parser.rs) (~2 539 lines). Hand-rolled recursive-descent parser. No tree-sitter, no `sv-parser`. Tracks `packages`, `type_scope`, `var_struct_types`, `warnings`. Recognises `package`/`typedef enum`/`typedef struct packed`, modules + ports + parameters, `always_ff`/`always_comb`, continuous `assign`, `if`/`case`/`casez`/`casex`, multi-precedence expressions. Skips with `UnsupportedConstruct` warnings: `generate`, `for`/`while`/`repeat`/`forever`, `function`/`task` definitions, `bind`, interfaces, `defparam`, `(* … *)` attributes (other than `keep`), `randc`/`rand`, `$past`/`$rose`/`$fell`/`$stable`, SVA `assert property`/`assume property`/`cover property`, immediate `assert`, real/shortreal, unpacked arrays, memories.
- [`ast.rs`](../../crates/mununu-core/src/adapter/systemverilog/ast.rs) (~272 lines). `Module`, `Port`, `Parameter`, `Declaration { Enum | Logic }`, `AlwaysBlock { AlwaysFF | AlwaysComb }`, `Statement { If | Case | Block | NonblockingAssign | BlockingAssign }`, `Expr { Ident | Number | BinOp | Ternary | BitSelect | BitSlice | Concat | Not | Bool }`, `MununuProperty`, `MununuDomainAnnotation`, `ModuleInstantiation`.
- [`kripke.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs) (~2 705 lines). [`build_kripke_with_config`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L126) is the top-level builder. Register extraction (~L132), syntactic-constant scan (~L135), COI prune (~L164–216), state-space cap check (errors at ≥ 2^18; warns at ≥ 2^12, ~L218–254), input-domain construction (~L257), cross-product enumeration (~L260–283), [`extract_initial_state`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L2205) (reset-branch scan), [`compute_next_state`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L1900) per (state, input), OOB sentinel routing, [`prune_unreachable_states`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L440) BFS prune, automaton-spec assembly. Carries 15+ `// SOUNDNESS:` annotations on its over-approximation decisions.
- [`kripke_smt.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs) (~969 lines). [`discover_significant_values`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs#L20) — z3 BV per-guard *constant* enumeration: for each signal marked `abstraction: "discover"`, gather all guard expressions mentioning that signal, run z3 to enumerate up to 32 satisfying values. This is **not** predicate-image computation — it discovers per-signal constants in guards, not lifted predicates. Cross-module variant at L165–340.
- [`annotation.rs`](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs) (~1 545 lines). `mununu_sv_annotation_v1` and `mununu_sv_multi_v1` sidecar schemas. `SignalAnnotation` per signal: `name`, `preserve`, `abstraction ∈ { Boolean | BitBlast | BoundedCounter | Enum | Discover | Ignored }`, `bound`, `variants`, `value_map`, `combinational`, `note`. Multi-module schema adds `connections`, `composition`, `blackbox_modules`.
- [`fsm.rs`](../../crates/mununu-core/src/adapter/systemverilog/fsm.rs) (~319 lines). Typedef-enum FSM detection helper. `find_enum_state_var`, `extract_fsm`. Used to preserve enum-variant state names in synthesised controllers. Subsumed by predicate seeding in the new pipeline (§6.7).
- [`emit_controller.rs`](../../crates/mununu-core/src/adapter/systemverilog/emit_controller.rs) (~312 lines). Synthesised-controller SV emission — `controller_to_systemverilog`. Synthesis-only output path; removed in §9 Tier A.

### §2.3 Where each existing `AbstractionType` lands

> Source of truth: [`AbstractionType`](../../crates/mununu-core/src/adapter/domain.rs#L22) — surface: CLI+API

| Variant | Where it lands in code | Effect |
|---|---|---|
| `Boolean` | `clamp_to_domain` in [`kripke.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs) ~L800 | 2-valued domain |
| `BitBlast` | Per-bit eval in [`kripke.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs) ~L1700 | N individual 1-bit registers (capped at 4) |
| `BoundedCounter { bound }` | `clamp_to_domain` + OOB sentinel | `{0, 1, …, bound}` with overflow → sentinel |
| `Enum { variants, value_map? }` | [`build_variant_to_numeric`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L636) | Named variant domain with bidirectional numeric binding |
| `Discover` (default) | [`kripke_smt::discover_significant_values`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs#L20) → Enum | SMT-discovered constants become enum variants |
| `Ignored` | COI prune in [`kripke.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs) ~L164 | Register dropped before enumeration |

§9 replaces `BitBlast`, `BoundedCounter`, `Enum`, and `Discover` with a single `predicates: Vec<MuFormula>` sidecar field (§6.7).

## §3 Why a BTOR2-per-module frontend

The reviewer's three-stage frontend maps cleanly onto mununu's existing primitives — once the Yosys script stops flattening.

### §3.1 sv2v as normaliser

sv2v ([github.com/zachjs/sv2v](https://github.com/zachjs/sv2v)) lowers SystemVerilog 2017 to a Verilog-2005 subset. It elaborates `generate` blocks, inlines `function` and `task` definitions, lowers `interface`/`modport` to flat port lists, expands `for`/`while`/`repeat` over compile-time-bounded ranges, normalises `enum` typedefs to integer constants with named aliases, expands packed `struct` access into bit-slices, lowers `always_ff`/`always_comb`/`always_latch` to vanilla `always`, and handles `defparam` overrides. Critically, sv2v *preserves module hierarchy and most signal names* — the property the BTOR2-per-module composition path depends on.

Coverage delta against the current native parser's `UnsupportedConstruct` list:

| Construct | Native parser | sv2v + Yosys |
|---|---|---|
| `generate` blocks | ❌ skipped | ✅ elaborated |
| `for`/`while`/`repeat`/`forever` (bounded) | ❌ skipped | ✅ unrolled |
| `function automatic` / `task` (pure) | ❌ skipped | ✅ inlined |
| `interface` + `modport` | ❌ skipped | ✅ flattened |
| `defparam` | ❌ skipped | ✅ resolved |
| `typedef struct packed` (nested) | ⚠ one-level | ✅ multi-level |
| `casez`/`casex` wildcard match | ⚠ collapsed to exact | ✅ proper wildcard |
| Multi-dimensional arrays of packed vectors | ❌ skipped | ⚠ partial (packed dims work) |

Integration is a subprocess dependency, discovered via the same `locate_*` pattern already used by [`adapter/yosys::locate_yosys`](../../crates/mununu-core/src/adapter/yosys/mod.rs). No bundling, no version pinning beyond a minimum advisory.

### §3.2 Yosys with hierarchy preserved

The current Yosys script at [`yosys/mod.rs::build_script`](../../crates/mununu-core/src/adapter/yosys/mod.rs#L656) runs `read_verilog; hierarchy -top; proc; cutpoint -blackbox; flatten; async2sync; chformal -lower; dffunmap; setundef …; write_btor`. The `flatten` pass inlines every submodule, discarding the structural decomposition mununu needs for per-module abstraction.

The replacement script:

```text
read_verilog -sv elaborated.v          # sv2v output
hierarchy -check -top <top>            # NO -flatten
proc
opt -fast -purge -keepdc               # conservative; keep don't-cares
flatten -nonest                        # OPTIONAL fallback; default off
async2sync
chformal -lower
dffunmap
setundef -anyseq                       # bug-preserving; per pillow A.4a
foreach mod in $(submod -list):
    submod -name <mod>
    write_btor -x design_<mod>.btor    # one BTOR2 per submodule
```

Per-module BTOR2 emission via `submod -name <mod>; write_btor` is ~200 LOC of script-generator change in [`yosys/mod.rs::build_script`](../../crates/mununu-core/src/adapter/yosys/mod.rs#L656) plus multi-output handling in `translate_sv` (the orchestrator that today calls the BTOR2 adapter exactly once). **BTOR2 over AIGER** is non-negotiable: AIGER throws away word-level structure, names, and types, forcing the lifter to do bit-blasting that predicate abstraction (§6) would then have to undo. The pillow plan's [§A.4a finding](../../.claude/plans/phase-a4-predicate-image.md) confirms BTOR2 + named registers gets the predicate-image algorithm correct on Caliptra; AIGER would not.

### §3.3 Per-module lifter from BTOR2 to KMTS-aware `Clts`

New module [`crates/mununu-core/src/adapter/btor2/kmts_lift.rs`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs) (NEW). Inputs: a single submodule BTOR2 file + an optional sidecar carrying predicates / UF-wrapping declarations / assumptions. Outputs: one `Clts<S, L>` per BTOR2, with `TransitionModality` set per §6.5 and `state_3valued_predicates` populated per §6.7. One automaton per SV module instance; the top module's structural netlist (parsed from the top-level BTOR2 or via Yosys's `dump -json -outfile`) drives the composition expression (§4).

### §3.4 Locking down Yosys passes

Yosys's `opt` family will fold logic, propagate constants across module boundaries when allowed, rename anonymous nets, and inline registers it proves constant. Three concrete failure modes for downstream abstraction (reviewer's enumeration; observed in practice on real RTL):

1. **`opt_clean` / `wreduce` rename or shrink predicate-referenced signals.** Mitigation: insert `(* keep = 1 *)` attributes on every signal a sidecar predicate references, *before* Yosys runs. The lifter performs a pre-pass over the sidecar predicate set to compute the keep-list, then injects attributes into the post-sv2v Verilog.
2. **`opt_expr` / `opt_merge` fuse logic across abstraction boundaries.** If a signal is marked for predicate abstraction but Yosys later inlines it, the boundary is gone. Mitigation: same `(* keep = 1 *)` pre-pass.
3. **Constant propagation eliminates entire registers.** If a register becomes provably constant after `opt`, Yosys deletes it. If a property or abstraction predicate references the deleted register, the lifter **must hard-error** rather than silently substitute the constant — silent substitution risks soundness if the user's predicate semantics depend on the register's identity. Lifter behaviour: load the post-Yosys BTOR2, scan the predicate set, and emit a structured error naming every missing signal with a one-line remediation (`add (* keep = 1 *) at <line> in <file>`).

### §3.4.1 Locking arithmetic cells for UF wrapping

Yosys decomposes large arithmetic into shift-and-add networks when `opt_share` and `opt_muxtree` are aggressive. If `$mul`/`$div`/`$mod`/`$pow` cells get transformed before BTOR2 emission, §6.10's UF wrapper has nothing to wrap. Mitigation: `keep_hierarchy` on macro instances containing these cells, and skip aggressive `opt_share`/`opt_muxtree` for any module declaring UF-wrapped operators in its sidecar. `opt_expr -mux_undef -mux_bool` is fine; the decomposing passes are not. This applies only to designs that declare UF wrapping; the default pipeline runs the full `opt` chain.

## §4 Mapping SV semantics onto mununu's automaton model

The mapping below is already mununu's existing convention (per [`CLAUDE.md`](../../CLAUDE.md) §Adapter / Emitter Capability Use). The doc states it explicitly because the BTOR2 lifter (§3.3) re-uses it from scratch — there is no behavioural drift, but the lifter must reach for each primitive deliberately rather than re-encoding source features as state-name suffixes.

### §4.1 Registers → state valuations

Each SV `always_ff`-driven register is persistent state. The natural per-module state space is the product of register valuations — optionally bit-blasted, optionally kept word-level when the predicate set requires word-level granularity (the predicate-abstraction §6.6 case). States are post-clock-edge configurations.

Today's [`build_kripke_with_config`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L126) reads registers from `Declaration::Logic` and `Declaration::Enum` in the parsed AST. The BTOR2 lifter reads them from BTOR2 `state` nodes (which carry the named register identifier and bit width) plus `init` nodes for reset values.

### §4.2 Wires / ports → labels on transitions

Combinational signals and module ports are values held *during* a cycle, not across it. They are the natural witnesses of a single transition. [`Transition::labels`](../../crates/mununu-core/src/clts/mod.rs#L265) is a `SmallVec<[LabelId; 4]>` precisely because a single SV cycle exposes multiple port values simultaneously: `(port_a = v1, port_b = v2, …)`. Per CLAUDE.md, the convention is multi-label edges, not parallel single-label edges between the same source/target pair.

This is theoretically equivalent to a Kripke structure with atomic propositions over both registers and ports, but the labels-on-transitions encoding has two practical wins: label-based synchronization between module automata directly mirrors SV port connectivity (§4.4), and valuations stay internal to each automaton — necessary for compositional reasoning (§7).

### §4.3 Clock as a shared label

A clock is just another label, shared between all synchronously composed modules. Multi-clock designs use asynchronous composition between clock domains, with each domain treated as an independent label alphabet. CDC primitives (synchronizers) are modelled as small automata that bridge the two alphabets — but proving liveness under multi-clock requires a fairness assumption that each clock ticks infinitely often (§11 deferred — multi-clock fairness encoding).

### §4.4 Composition driven by the top module

The top module's structural netlist (post-sv2v, pre-flatten) is a bipartite graph of instances and nets. Translate it into a composition expression:

- For each instance `M u(.p1(n1), .p2(n2), …)`: take the automaton for `M` and rename its port labels to the net names `n1, n2, …`. This is the existing per-source label-renaming machinery in [`verify/binding.rs::apply_renamings_to_ctxdsl`](../../crates/mununu-core/src/verify/binding.rs); the lifter populates it from the top-module netlist.
- Synchronously compose all sibling instances on the resulting shared label alphabet.
- Top-level inputs are labels in the alphabet but not synchronized with anything internal — free environment events.

Synchronization is value-equality on shared labels, which is SV semantics for a connected net. Today's [`composition::compose`](../../crates/mununu-core/src/composition/mod.rs#L440) shared-label rendezvous at L518 (`labels_have_intersection`) is name-equality only; the native multi-module path approximates value-equality via a label-suffix shim in [`mod.rs::annotate_driving_output_labels`](../../crates/mununu-core/src/adapter/systemverilog/mod.rs#L511). The BTOR2-per-module pipeline needs the rendezvous itself to support value-equality (R.1 audit; §6.5).

### §4.5 Driver-side controllability

[`LabelControllability { Controllable | Internal | Uncontrollable }`](../../crates/mununu-core/src/clts/mod.rs#L248) currently encodes the *synthesis-level* (game-theoretic) controllability split — top inputs are uncontrollable, top outputs are controllable. There is a second, *composition-level* (driver) flavour: inside a hierarchy, every internal net has a unique driver. The driver's automaton "controls" that net; the consumer is "driven by" it. The two flavours can co-exist on the same enum — the driver discipline becomes the lifter's default for internal nets (producer-side controllable, consumer-side uncontrollable). This is adapter discipline, not a data-model change.

## §5 Cone of influence: the exact / free / sound abstraction

COI is the cheapest abstraction in the stack and the only one that is *exact*: for a mu-calculus formula φ whose atomic propositions reference a signal set Σ_φ, the structural cone — the transitive fan-in of Σ_φ through combinational logic and registers — defines a sub-netlist whose behaviour, *projected onto Σ_φ*, is bisimilar to the full system's. So COI is sound and complete for the full mu-calculus over Σ_φ — including liveness, including alternating fixpoints. No approximation, no refinement.

### §5.1 What mununu has today

> Source of truth: [`adapter::partition`](../../crates/mununu-core/src/adapter/partition/) — surface: CLI+API

The pillow plan's Phase A.3 shipped signal-level cone-of-influence as the [`adapter::partition`](../../crates/mununu-core/src/adapter/partition/) module. Per-format `DepGraphBuilder` implementations cover SV, BTOR2, and the extraction adapter. The Caliptra fixture confirmed a 128× state-space reduction (raw 2^19 → 4 096 enumerated states) — empirical confirmation of "exact" being also "valuable" in practice.

### §5.2 Property clustering

The interesting design choice is *property clustering* when a batch of properties {φ_1, …, φ_n} is verified together. Three strategies:

- **Naive joint COI** — union all Σ_{φ_i}, take the cone once. Simple, but a single wide-fanin property (e.g. one involving a fairness predicate over many registers) pollutes the cohort.
- **Per-property COI** — one cone per φ_i. Maximum reduction per check, but model-construction cost paid n times; no reuse of intermediate fixpoints.
- **Clustered COI** (recommended) — partition properties by Jaccard similarity on cone signal sets, or on cone *module* sets when compositional structure is being preserved. Cluster, then verify each cluster on its joint cone. Usually the sweet spot.

Clause clustering — grouping properties that share many atomic propositions — is a useful proxy that is cheaper to compute than the cones themselves: do it first as a coarse filter, then refine by actual fan-in overlap. Watch for properties that look syntactically dissimilar but share a deep dependency (e.g. both depend on a single FSM state register that controls everything else); pure clause clustering misses these. Mitigation: a two-pass cluster — coarse cluster from clauses, then re-cluster after cone computation.

R.4 ships this as an extension of [`adapter::partition`](../../crates/mununu-core/src/adapter/partition/). M.3 validates it on a multi-module OpenTitan fixture.

### §5.3 Where COI does *not* help

COI is exact only when the property's atomic-proposition set is well-defined. Two cases where the cone is the whole system: (a) properties over global fairness (`□ ◇ clk_tick`) — the cone includes every register the clock controls, which is everything; (b) properties with deep abstract dependencies that surface as `KleeneBot` in 3-valued evaluation — refining them adds predicates that may pull more signals into the cone. The §6 KMTS path handles both; COI is the *first* reduction, not the only one.

## §6 KMTS + 3-valued mu-calculus: the principled abstraction

This is the doc's central contribution. Sub-sections §6.1–§6.10 build the framework from theoretical foundations through evaluator integration to operator-level abstraction.

### §6.1 Theoretical framing — why 2-valued abstraction fails for liveness

A single-relation over-approximation is sound only for ∀-fragments / pure greatest fixpoints — typical safety/invariance. Soundness is lost for least fixpoints (reachability/liveness) without a companion under-approximation. Counterexample: take an abstract transition relation that adds spurious edges to satisfy safety; the same edges create spurious progress in a `μX. (φ ∨ ◇X)` formula, flipping a false verdict to true.

KMTS carries *both* relations in one structure: a *may* edge `(b, b') ∈ R_may ⟺ ∃ s ⊨ b, s' ⊨ b'. (s, s') ∈ R` (over-approximation) and a *must* edge `(b, b') ∈ R_must ⟺ ∀ s ⊨ b. ∃ s' ⊨ b'. (s, s') ∈ R` (under-approximation), with the invariant `R_must ⊆ R_may`. The classical preservation theorem (Bruns–Godefroid CONCUR 2000; Godefroid–Jagadeesan TACAS 2003):

- A formula evaluates to `KleeneT` on the abstract ⇒ `true` on the concrete (regardless of formula polarity — including liveness).
- A formula evaluates to `KleeneF` on the abstract ⇒ `false` on the concrete.
- A formula evaluates to `KleeneBot` ⇒ refine.

Full proof sketches and the precise modal semantics live in [`kmts-theory.md`](kmts-theory.md) (companion doc, D.0 deliverable).

### §6.2 3-valued mu-calculus semantics

Verdicts in `{ KleeneT, KleeneF, KleeneBot }`. Modal operators read both relations:

- `[a]φ` is `KleeneT` iff *all* may-`a`-successors satisfy φ as `KleeneT`; `KleeneF` iff *some* must-`a`-successor has `KleeneF`; else `KleeneBot`.
- `⟨a⟩φ` is `KleeneT` iff *some* must-`a`-successor has `KleeneT`; `KleeneF` iff *all* may-`a`-successors are `KleeneF`; else `KleeneBot`.

Note the asymmetry: `KleeneT` claims on `[a]` require all may-successors (over-approximation must cover all possibilities); `KleeneF` claims require a must-successor witness (the underapproximation guarantees the bad behaviour really exists). Inverted for `⟨a⟩`.

Fixpoint computation: Kleene iteration over the **information order** `KleeneBot < KleeneF`, `KleeneBot < KleeneT`, with `KleeneF` and `KleeneT` incomparable (not the truth order). Convergence means "becoming more defined." Monotonicity in the information order is what allows least and greatest fixpoints to coexist with the 3-valued lattice; iterating in the truth order would oscillate.

### §6.3 Data-model changes (additive, default-preserving)

```rust
// crates/mununu-core/src/clts/mod.rs (EXTENSION)
enum TransitionModality { Sharp, MayOnly }  // standard KMTS: must ⊆ may
struct Transition {
    ...existing fields...
    modality: TransitionModality,  // default Sharp on construction
}

enum Tristate { KleeneT, KleeneF, KleeneBot }
struct Clts<S, L> {
    ...existing fields...
    state_3valued_predicates: Option<BTreeMap<(StateId, PredId), Tristate>>,
}
```

**Two variants, not three.** Standard KMTS enforces `must ⊆ may`; a transition is either in both relations (`Sharp`) or in only `may` (`MayOnly`). A `MustOnly` variant would violate the invariant. The mixed-transition-system extension of Dams–Gerth–Grumberg TOPLAS 1997 allows `must`-without-`may` for under-approximation-only abstractions, but the BTOR2 lifter does not produce such transitions, so the enum stays at the standard shape.

**`Tristate` variants prefixed with `Kleene`** to avoid collision with Rust's `bool` mental model and to read unambiguously across the thousands of match arms this enum will touch. The prefix invokes the theoretical lattice and namespaces the variants.

**Strict additivity.** Every existing adapter constructs `Sharp` transitions and `None` for `state_3valued_predicates` — no migration. The 2-valued semantics is preserved by `BoolDomain` (§6.4) as a strict specialisation: Kleene evaluator on a Sharp-only KMTS produces verdicts in `{ KleeneT, KleeneF }` with no `KleeneBot`, isomorphic to today's `{ true, false }` outcomes.

### §6.4 Evaluator changes — `trait TruthDomain` over two lattice structures

> **As-built note (P2.4, 2026-06-19).** The per-element `truth_domain::TruthDomain`
> trait sketched below was an R.1 design artifact that the R.3 evaluator **bypassed** —
> it was pub-exported but only ever exercised by its own unit tests, and the production
> evaluator kept two hand-written bulk bodies (`eval_node` over `BitVec`, `eval_node_tri`
> over `TritSet`). The IR-unification track's **P2** collapsed those two bodies onto one
> generic body over a NEW **bulk** trait — `EvalDomain` (associated `Valuation` type =
> whole-state-set `BitVec` | `TritSet`; `BoolDom` / `KleeneDom` impls) in
> [`crates/mununu-core/src/mu_calculus/evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs).
> The dual-lattice reasoning below is still correct theory, but in the shipped code it is
> absorbed into whole-`Valuation` ops (and, for the 3-valued case, into `TritSet`'s
> must/may pair) rather than the per-`Element` method split sketched here. The dead
> `truth_domain` module was retired in P2.4. See
> [`evaluator-domain-unification.md`](evaluator-domain-unification.md) for the full
> rationale + the perf-gate evidence.

3-valued model checking involves **two distinct lattice structures over the same element set**, and conflating them is the most common implementation pitfall:

- **Truth lattice** — formula semantics (`∧`, `∨`, `¬`). In `BoolDomain` this is `false < true`. In `KleeneDomain`, `false < true` with `KleeneBot` *incomparable* to both: `KleeneBot ∨ KleeneT = KleeneT`, `KleeneBot ∧ KleeneF = KleeneF`, `KleeneBot ∧ KleeneT = KleeneBot`, `KleeneBot ∨ KleeneF = KleeneBot`.
- **Information lattice** — fixpoint convergence. In `BoolDomain`, coincides with the truth lattice. In `KleeneDomain`, `KleeneBot < KleeneF`, `KleeneBot < KleeneT`, with `KleeneF` and `KleeneT` incomparable. Convergence means "becoming more defined."

```rust
// crates/mununu-core/src/mu_calculus/truth_domain.rs (NEW)
trait TruthDomain {
    type Element: Clone + Eq;
    // Truth-order operations (formula semantics)
    fn truth_bot(&self) -> Self::Element;                                              // false in both
    fn truth_top(&self) -> Self::Element;                                              // true in both
    fn truth_join(&self, a: &Self::Element, b: &Self::Element) -> Self::Element;       // ∨
    fn truth_meet(&self, a: &Self::Element, b: &Self::Element) -> Self::Element;       // ∧
    fn truth_negate(&self, a: &Self::Element) -> Self::Element;                        // ¬
    // Information-order operations (fixpoint convergence)
    fn info_bot(&self) -> Self::Element;                                               // false in Bool, KleeneBot in Kleene
    fn info_join(&self, a: &Self::Element, b: &Self::Element) -> Self::Element;        // refinement-toward-more-defined
    fn info_leq(&self, a: &Self::Element, b: &Self::Element) -> bool;                  // convergence predicate
    // Modal operators (per §6.2)
    fn box_modality(&self, may: &[Self::Element], must: &[Self::Element]) -> Self::Element;
    fn diamond_modality(&self, may: &[Self::Element], must: &[Self::Element]) -> Self::Element;
}
struct BoolDomain;    // existing semantics; Sharp transitions only; truth-order ≡ info-order
struct KleeneDomain;  // 3-valued; may/must split per §6.2; truth-order ≠ info-order
```

If both axes are not exposed in the trait, R.3 will discover the divergence mid-implementation and the API will churn. Surfacing it now means `BoolDomain` aliases the two lattices (`info_bot ≡ truth_bot`, `info_join ≡ truth_join`); `KleeneDomain` distinguishes them — caller code chooses based on whether it is evaluating a formula or iterating a fixpoint.

### §6.5 Composition changes — pointwise meet on capability lattice

Modalities form a capability lattice. Each transition has two independent capabilities: `may` (over-approximation witness — the transition is admitted) and `must` (under-approximation witness — the transition is required). Standard-KMTS valid capability sets are `{may}` (= `MayOnly`) and `{may, must}` (= `Sharp`, lattice top). The empty set means no transition exists on the label.

Composition derives from Larsen–Larsen–Wąsowski FoSSaCS 2007 (modal I/O automata): each capability composes by conjunction across sides. A composed transition has capability `c` iff *both* sides have a transition with capability `c` on the synchronizing label:

```text
has_may (left ⊗ right) = has_may (left) ∧ has_may (right)
has_must(left ⊗ right) = has_must(left) ∧ has_must(right)
```

Set-intersection on each axis, independently. The merge table is a corollary:

```text
Sharp   ⊗ Sharp   = Sharp     (both have may; both have must)
Sharp   ⊗ MayOnly = MayOnly   (both have may; only left has must → composed has may but not must)
MayOnly ⊗ MayOnly = MayOnly   (both have may; neither has must)
```

Three cases, not six — the standard-KMTS invariant eliminates the `MustOnly` rows. There is no "weakening": the must side simply has no witness to contribute when the other side has only a may-edge, so the composed transition has no must-capability there.

**Two R.1 audits on [`composition/mod.rs`](../../crates/mununu-core/src/composition/mod.rs) are blockers, not nice-to-haves:**

1. **Value-equality on shared labels** (the visible behavioural change). The shared-label rendezvous at L518 (`labels_have_intersection`) compares label *names*. SV port connectivity requires value-equality on the connected net. The native multi-module path approximates this via [`annotate_driving_output_labels`](../../crates/mununu-core/src/adapter/systemverilog/mod.rs#L279); the BTOR2-per-module pipeline needs the rendezvous itself to support value-equality.

2. **Modality-meet on the rendezvous** (the **silent soundness bug**). Today's `compose()` does not carry modality at all (every transition is implicitly Sharp), so the meet above is a no-op pre-KMTS. Post-KMTS, the rendezvous must execute `has_may ∧ has_may` and `has_must ∧ has_must` per composed transition. Missing this is *not* a verdict-quality bug — it is a **soundness bug**: a composed transition incorrectly marked `Sharp` when only one side has `must` gives the evaluator a fabricated must-witness, which makes `KleeneF` verdicts unsound.

Both audits land in the same R.1 PR as the `TransitionModality` field addition.

**Central architectural payoff.** KMTS composition is structural and SMT-free. The pillow plan §1.3 finding — sidecar metadata loss at the product boundary — disappears, because the 3-valued AP labels and modalities are part of the `Clts` data structure (not the sidecar) and survive composition by construction.

### §6.6 Predicate-image construction from BTOR2

Extension of the existing [`kripke_smt::discover_significant_values`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs#L20) (z3 BV, per-guard constant enumeration) to *predicate* image: `∃ s. ϕ(s) ∧ p_i(s)` for each predicate `p_i ∈ P`. Per the pillow plan A.4a finding, the algorithm is correct under `setundef -anyseq`; what fails today is the *consumer* (explicit-state bit-blast cap), not the discovery. KMTS sidesteps the cap because abstract states are predicate cubes (`2^|P|`), not concrete cross-products of register valuations.

Two-mode operation:

- **Must-mode** (`R_concrete`): `∀ s ⊨ b. ∃ s' ⊨ b'. (s, s') ∈ R_concrete`. Used to populate must-edges. Slower; uses concrete `$mul`/`$div`/`$pow` semantics. Restricted to candidate edges drawn from the may set to bound the work.
- **May-mode** (`R_UF`): `∃ s ⊨ b, s' ⊨ b'. (s, s') ∈ R_UF`. Used to populate may-edges. Default in the lifter; drops queries into EUF + linear-bitvector. See §6.10 for the UF wrapping policy.

Both modes extend the existing z3 BV machinery; the new code lives in the lifter, not in `kripke_smt.rs` (which becomes a strict subset reused for the may-mode constant-image queries).

### §6.7 Predicate seeding

Sources for the initial predicate set P:

- **Property atomic propositions.** Every distinct sub-expression of the form `reg == constant`, `reg < constant`, `reg ∈ {…}`, or a Boolean-valued register reference becomes a predicate.
- **COI register-equality.** For each constant the property's cone references, add `reg == constant` to P. This is what today's `scan_significant_constants` does syntactically; the lifter extends it across the full COI signal set.
- **Typedef-enum membership.** For each enum register in the cone, add one predicate per variant (`is_IDLE`, `is_BUSY`, …). This is what the soon-to-be-deleted [`fsm.rs`](../../crates/mununu-core/src/adapter/systemverilog/fsm.rs) was approximating; the predicate set carries the same information without needing a separate typedef-enum extractor.
- **User-supplied sidecar predicates.** A new `predicates: Vec<MuFormula>` field per module replaces the existing `SignalAbstraction` enum's `BoundedCounter | Enum | BitBlast | Discover` variants. Each entry is a name + a mu-calculus-grammar predicate over module signals.
- **CEGAR refinement** (§6.8). When the evaluator returns `KleeneBot`, refinement adds predicates derived from the spuriousness check's UNSAT core.

### §6.8 CEGAR refinement

On `KleeneBot` verdict, lift the abstract counterexample (lasso for liveness, finite prefix for safety) and discharge it concretely via SBY or direct SMT unrolling. If real, report. If spurious, refine via IC3-IA-style interpolation (Cimatti–Griggio–Mover–Tonetta TACAS 2014). The unsat core of the spuriousness check carries the discriminating fact; the interpolation step produces a new predicate (or strengthens an existing one) that excludes the spurious behaviour. Refinement adds predicates *to the responsible module's local set*, not globally — per-module abstract state spaces stay bounded.

**Bounded refinement.** Default cap: 16 rounds per (property, module). On cap-hit, keep the abstract `KleeneBot` verdict with a soundness-tagged warning. Configurable per-module via sidecar `cegar_max_rounds`.

Full algorithm + heuristic catalog in [`predicate-abstraction-recipe.md`](predicate-abstraction-recipe.md) §4.

### §6.9 Soundness story

- Over-approximation by lifting → adds may-edges, never removes them. `KleeneT` on the abstract transfers to `true` on the concrete.
- Under-approximation by witness → adds must-edges only when a witness has been verified. `KleeneF` on the abstract transfers to `false` on the concrete.
- Refinement only shrinks the may set or grows the must set; the verdict lattice is monotone in the information order. A refined KMTS produces verdicts that are at least as informative as the unrefined one.
- The composed verdict transfers to the concrete iff each module's KMTS is independently a sound abstraction. This is the standard KMTS preservation result; the compositional case is Huth–Jagadeesan–Schmidt TACAS 2001.
- UF-abstracted may-edges (§6.10) remain sound because UF only *adds* may-behaviour beyond the concrete relation; the asymmetry is intentional and is what makes the two-mode predicate-image sound (may-mode under UF, must-mode under concrete operators).
- `// SOUNDNESS:` annotations on every new fallback in the lifter, per the existing project convention.

### §6.10 Operator-level abstraction via uninterpreted functions

**Axis orthogonality.** Predicate abstraction (§6.6) and UF abstraction are *orthogonal* axes of the abstraction design space, not competing techniques. Predicate abstraction abstracts the **state space** — keep concrete operations, abstract which states are distinguishable. UF abstraction abstracts the **operations** — keep concrete or predicate state, replace complex operators with uninterpreted symbols whose only axiom is functional consistency (`f(x) = f(x)`). The plan adopts both.

**Where UF helps the BTOR2 pipeline.** Multipliers, dividers, hashes, CRCs, large adders, custom arithmetic — Yosys emits these as word-level BTOR2 nodes (`$mul`, `$div`, `$pow`, often `$macc` after `opt_share`). Z3's QF_BV solver chokes on them during predicate-image queries. Replacing `$mul(a, b)` with an uninterpreted `f_mul(a, b)` drops queries into the EUF + linear bitvector fragment, dramatically more tractable. Cost: loss of arithmetic identities (`a * 0 = 0` is no longer known). Gain: predicate-image queries that terminate within a sane timeout. This is the Bryant–Burch–Dill provenance (positive equality / EUF for processor verification, late 1990s; Andraus–Sakallah LPAR 2008 for the selective re-interpretation refinement recipe).

**Asymmetry under 3-valued KMTS.** UF abstraction is a **may-side-only over-approximation**. The UF-abstracted transition relation admits more behaviours than the concrete relation (UF satisfies only functional consistency; `$mul` satisfies multiplication). So:

- May-edges computed under UF abstraction: cheap, sound (UF only adds may-behaviour, never claims false-positive verdicts — extra may-edges make more verdicts `KleeneBot`, not `KleeneT` or `KleeneF`).
- Must-edges computed under concrete operators: slower, but UF is **unsound** here — a "for all" witness over UF does not transfer because UF admits behaviours the concrete operator does not.

This is the right asymmetry. KMTS preservation still applies because both relations are independently sound abstractions of their target.

**Default UF wrapping policy.** The lifter wraps `$mul`, `$div`, `$mod`, `$pow` unconditionally; wraps `$add`/`$sub` only when width > 32 bits; never wraps `$and`/`$or`/`$xor`/`$not` (already cheap for SMT). User overrides via sidecar `uf_wrap: Vec<String>` (cell-instance names to force-wrap) or `uf_unwrap` (to force-concretize). The arithmetic-cell lockdown sentence in §3.4.1 covers the Yosys-decomposition risk.

**Cooperation with CEGAR refinement.** When the spuriousness check returns UNSAT on an abstract counterexample, the unsat core distinguishes:

- Bitvector constants in the core → state-distinguishability gap → add predicates.
- UF instance terms (`f_mul(…)`) in the core → operator-behaviour gap → either selectively concretize that UF instance, or add a learned-lemma axiom (`a * 0 = 0`, `a * 1 = a`) without full concretization.

One interpolation query, two refinement decisions, partitioned by symbol kind. The Cimatti–Griggio–Mover–Tonetta IC3-IA recipe extends naturally — interpolants live over a signature including both predicate symbols and UF symbols. v1 heuristic + bounded-refinement cap detailed in [`predicate-abstraction-recipe.md`](predicate-abstraction-recipe.md) §3a/§3b/§4.

## §7 Compositional KMTS: the structural free lunch

The pillow plan §6 catalogued three Paths (α, β, γ) for handling cross-module abstraction soundness. Under KMTS, the ladder collapses: KMTS composition is structural and sound by construction, so the only remaining question is *tightness*.

### §7.1 Why the AGR ladder is no longer needed

The pillow plan's Path catalog was a response to the sidecar-metadata-loss problem: per-source sidecars resolve to abstract state names that mean nothing post-composition, so a property over the composed product cannot reference the abstraction's semantics. Path α (ground-then-compose), β (carry UF on labels), and γ (assume-guarantee discharge) each addressed this from a different angle.

Under KMTS, the problem dissolves. 3-valued AP labels and transition modalities are part of `Clts`, not the sidecar. The composition operator's §6.5 pointwise meet on the capability lattice is sound for *both* may and must (Larsen 1989; Larsen–Larsen–Wąsowski FoSSaCS 2007). The composed verdict transfers to the concrete without an AGR discharge step, *provided* each module's KMTS is independently sound (which it is by construction of the lifter).

### §7.2 What this preserves and what it doesn't — worked counterexample

Preserves:
- Per-module abstraction (the operational win Path α was reaching for).
- Cross-module verdict soundness *under per-module abstractions* (the foundational goal Path γ was reaching for).
- Liveness reasoning under fairness (the gap a single-relation over-approximation cannot close).

Does not preserve:
- *Tightness* — the composed KMTS may have more `KleeneBot` verdicts than a monolithic predicate abstraction of the flattened system.

**Worked counterexample: `multi_producer_consumer_top.sv`.** The fixture composes [`multi_producer.sv`](../../examples/systemverilog/multi_producer.sv) and [`multi_consumer.sv`](../../examples/systemverilog/multi_consumer.sv) via a shared `valid` net plus a 4-bit `data` bus. The property pair: `□ (consumer.received ⇒ producer.sent)` (safety) and `□ ◇ consumer.received` (liveness, under fairness).

Per-module abstraction: each module independently with predicates `{ valid == 0, valid == 1 }` on the shared net and `{ count == 0, count > 0 }` on its local counter. Composition merges modalities pointwise per §6.5.

Failure mode: on the composed KMTS, the safety formula returns `KleeneBot`. Trace: producer's `MayOnly` transition on `(valid=1, data=k)` synchronizes with consumer's `MayOnly` transition on `(valid=1, data=k)` for *any* k — neither side carries a predicate relating `producer.data_out` to `consumer.data_in`, so the composed transition admits a behaviour where consumer receives `data=k'` for `k' ≠ k`. The safety formula's atomic proposition `consumer.received` is observed as `KleeneBot` (some may-successor has it; no must-successor witnesses it).

Monolithic predicate abstraction over the flattened design returns `KleeneT` because the flattened relation directly equates `producer.data_out` with `consumer.data_in` via the structural net assignment; the spurious behaviour does not exist in the concrete.

**The predicate that closes the gap.** Add one port-equality predicate to the multi-module sidecar: `data_eq_on_handshake = (valid ⇒ producer.data_out == consumer.data_in)`. Composition's modality merge now respects this predicate: the spurious cross-data transition becomes a `MayOnly` edge that no must-witness backs, but the safety formula no longer needs the cross-data witness because `data_eq_on_handshake` constrains what consumer can observe. Safety verdict graduates from `KleeneBot` to `KleeneT`. The full sidecar diff and the `verify.toml` for this scenario land in `examples/verify/multi_producer_consumer_kmts/` as part of R.2.

**Why this generalizes.** Heuristic: every shared net in any property's cone gets value-equality predicates on a few canonical constants + an equality predicate between connected ports. The above example instantiates the second half (port-port equality on `data_out ↔ data_in`); the first half (canonical-constant predicates) handles `valid == 0/1` for free. For multi-instance fan-out (one producer driving N consumers), the predicate set grows linearly in N — manageable. For multi-driver buses (rare in well-formed SV), the predicate set encodes the arbitration — this is where the heuristic becomes manual.

Mitigation summary: the port-equality heuristic closes the tightness gap on the fixtures we know about. The lifter auto-emits the canonical port-equality predicates for every connection declared in the multi-module composition. Authors only need to add predicates beyond this when their property crosses an arbitrated bus or a stateful intermediate.

### §7.3 Where assumptions still fit

Even with KMTS, a user may want to declare environment assumptions for tighter abstractions ("this input is always nonzero", "this counter never exceeds N"). Sidecar gains an optional `assumptions: Vec<MuFormula>` field per module; the evaluator treats assumptions as `must`-true on entry. This is **not** circular AGR — it is environment over-approximation, which is sound by construction. Falls back to user-supplied predicates from §6.7 if the sidecar prefers the simpler shape.

### §7.4 Unification with the existing CLTS surface

The KMTS-extended `Clts<S, L>` is the new *unified* shape. Existing CLTS-only adapters (XState, microcode, agentic, hand-written CTXDSL) produce Sharp-everywhere KMTSes vacuously; the BTOR2 lifter produces 3-valued KMTSes; the evaluator handles both via `TruthDomain` monomorphisation. No parallel ecosystem, no `Kmts<S, L>` type — just an additive extension to the existing `Clts`.

## §8 Worked SV ↔ KMTS gallery

Six examples drawn from `examples/systemverilog/`. Each shows the post-sv2v + Yosys-no-flatten + BTOR2-lifter output (the new pipeline), not the hand-rolled-parser output. Examples that depend on R.2/R.3 carry a `// (verifiable post-R.3)` note where the output is sketched against the proposed lifter signature.

### §8.1 Pure FSM with enum state — `traffic_light.sv`

[`examples/systemverilog/traffic_light.sv`](../../examples/systemverilog/traffic_light.sv) is a typedef-enum FSM with `always_ff` + `case`. 4 enum variants (`GREEN`, `YELLOW`, `RED`, `RED_WAIT`), one `tick` input, one timer-driven state machine.

Sidecar (NEW form, post-S.3):

```json
{
  "$schema": "mununu_sv_annotation_v2",
  "module": "traffic_light",
  "predicates": [
    { "name": "is_green",   "formula": "state == GREEN" },
    { "name": "is_yellow",  "formula": "state == YELLOW" },
    { "name": "is_red",     "formula": "state == RED" },
    { "name": "is_red_wait","formula": "state == RED_WAIT" }
  ],
  "controllable": ["light_signal"]
}
```

KMTS shape: 4 states, all Sharp transitions (no abstraction needed — control state is small enough to enumerate exactly). The KMTS lifter recognises the property's atomic propositions reference exactly the 4 predicates above, builds a 4-state abstract automaton where each abstract state corresponds to one predicate cube, and produces transitions identical to the native parser's output. Verdict polarity matches today's output exactly. M.0 baseline candidate.

### §8.2 FSM + bounded counter — `fifo.sv`

[`examples/systemverilog/fifo.sv`](../../examples/systemverilog/fifo.sv) + [`fifo.mununu.json`](../../examples/systemverilog/fifo.mununu.json) — a 4-entry FIFO with fill counter and 4-state control FSM, wide `data_out_r` marked `ignore`.

Sidecar migration: the old `BoundedCounter { bound: 4 }` on `fill` becomes 5 predicates `count_0, count_1, count_2, count_3, count_4`. The old `Enum { variants: [IDLE, WRITING, READING, RDWR] }` on `state` becomes 4 predicates `is_idle, is_writing, is_reading, is_rdwr`. The old `Ignored` on `data_out_r` stays as `signals: [{ name: "data_out_r", preserve: false }]`.

KMTS shape: 4 × 5 = 20 reachable predicate cubes, all Sharp (no abstraction non-determinism). Same verdict as today; demonstrates the migration path. The post-S.3 schema removes the variant-based abstraction types entirely and the new form is shorter and more uniform.

### §8.3 Discover-driven predicate abstraction — `cwe1245_fsm_bug.sv`

[`examples/systemverilog/cwe1245_fsm_bug.sv`](../../examples/systemverilog/cwe1245_fsm_bug.sv) + [`cwe1245_fsm_bug.mununu.json`](../../examples/systemverilog/cwe1245_fsm_bug.mununu.json) — the canonical CWE-1245 FSM bug (uncovered FSM state under undefined initial condition).

Sidecar (NEW): drops the `Discover` abstraction in favour of:

```json
{
  "predicates": [
    { "name": "boot_in_known_state", "formula": "state ∈ {0,1,2,3,4,5,6,7}" },
    { "name": "boot_in_unsafe",       "formula": "state ∈ {5,6,7}" }
  ]
}
```

KMTS shape (post-R.3): the predicate-image step (§6.6) under `setundef -anyseq` discovers `state ∈ {0..7}` as the reachable set, lands two abstract states `{ boot_in_known_state ∧ ¬boot_in_unsafe, boot_in_known_state ∧ boot_in_unsafe }`. The safety formula `□ ¬boot_in_unsafe` returns `KleeneF` on the bug fixture (with a must-witness trace ending in `state == 7`) and `KleeneT` on the `_fixed.sv` companion. This is the verdict-flip the pillow plan A.4a closure could not achieve under the explicit-state bit-blast cap; KMTS sidesteps the cap because the abstract states are predicate cubes, not concrete cross-products.

### §8.4 Multi-module synchronous composition — `multi_producer_consumer_top.sv`

[`examples/systemverilog/multi_producer_consumer_top.sv`](../../examples/systemverilog/multi_producer_consumer_top.sv) + paired sidecar. The tightness counterexample for this fixture lives in §7.2 (worked end-to-end with the port-equality predicate that closes the gap); §8.4 cross-references §7.2 and reports only the verdict diff + automaton stats.

Verdict diff (post-R.3):
- Without `data_eq_on_handshake`: safety = `KleeneBot`, liveness = `KleeneBot`.
- With `data_eq_on_handshake`: safety = `KleeneT`, liveness = `KleeneT` (under fairness).

Automaton stats (composed KMTS):
- Per-module states: producer 12, consumer 8.
- Composed states (Synchronous): ~96 before reachable-state pruning, ~24 after.
- Modality breakdown: 72% Sharp transitions, 28% MayOnly (the spurious cross-data edges).

### §8.5 Symbolic constant via predicate — snippet from `axi4lite_slave.sv`

A 5-line snippet from [`axi4lite_slave.sv`](../../examples/systemverilog/axi4lite_slave.sv) showing address comparisons `awaddr == 32'h1000`. Sidecar declares one predicate:

```json
{
  "predicates": [
    { "name": "is_ctrl_reg", "formula": "awaddr == 32'h1000" }
  ]
}
```

KMTS shape: one Boolean predicate cube partitions the address space into `{is_ctrl_reg, ¬is_ctrl_reg}`; the rest of the address space (which the property does not distinguish) collapses to a single abstract state. This is what `value_map: [{ name: "CTRL_REG", value: 4096 }]` was approximating in the old schema — naming a constant for human-readable state IDs. Under predicates, the human-readable name is the predicate's `name` field; no `value_map` machinery survives.

### §8.6 OOB sink replacement — hand-written 5-line counter

A trivial module:

```systemverilog
module count_overflow(input logic clk);
    logic [2:0] count;
    always_ff @(posedge clk) count <= count + 2;
endmodule
```

Old pipeline with `BoundedCounter { bound: 4 }`: overflow at `count == 4` routes to the `__mununu_oob__` sentinel state; safety verdicts mask OOB out of all formulas (over-approximation); liveness verdicts need care (the sentinel can absorb fairness).

New pipeline with predicates `{ count == 0, count == 1, count == 2, count == 3, count == 4, count_overflow = count > 4 }`: the overflow case is just another abstract state. The predicate-image query for the `(count == 3) → ?` transition returns the SAT result `count == 5`, which falls outside `{0..4}` but matches `count_overflow`, so the must-edge goes `(count == 3) ⊗ count_overflow` cleanly. No sentinel state, no OOB masking, no "OOB-aware" formula semantics — overflow is just data that some predicate captures.

This demonstrates a direct removal: §9 Tier A drops the OOB sentinel machinery without losing soundness, because predicate abstraction makes overflow representable via the user's chosen predicate granularity rather than via a special-cased state.

## §9 Simplification phase: what we remove

Inventory of code that becomes vestigial under the new pipeline. Each Tier has a removal-readiness condition tied to a roadmap phase (§10). Line counts via `wc -l` on the current HEAD; precise code-line counts via `tokei` will be slightly lower (blank/comment subtraction) but order-of-magnitude identical.

### §9.1 Tier A — synthesis-only, removable as soon as R.0 ships

| Target | Lines | Removal condition |
|---|---|---|
| [`emit_controller.rs`](../../crates/mununu-core/src/adapter/systemverilog/emit_controller.rs) | 312 | Confirm no fixture exercises `mununu synthesise --emit-sv`. Synthesised-controller emission moves to a per-pipeline post-process if any user needs it later. |
| `AbstractionType::BitBlast` variant | ~120 (handler in `kripke.rs` + sidecar parsing in `annotation.rs`) | Cap was 4 bits — predicate abstraction subsumes it. Verify no fixture sets `BitBlast` for > 2-bit signals. |
| OOB sentinel routing in [`kripke.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs) (`__mununu_oob__` construction + tied `// SOUNDNESS:` annotations at ~L343, L2067, L2117) | ~180 | Predicate abstraction handles overflow via §8.6 — UNSAT on the predicate-image query for out-of-range successors. The sentinel state, its insertion logic, its formula-evaluator masking, and its `// SOUNDNESS:` annotations all go together. |

### §9.2 Tier B — extraction-only, removable after R.1 + R.2 + fixture-sweep verdict-baseline match

| Target | Lines | Removal condition |
|---|---|---|
| [`fsm.rs`](../../crates/mununu-core/src/adapter/systemverilog/fsm.rs) | 319 | Predicate seeding from BTOR2 IR (§6.7) covers all current uses. Verify by running the KMTS lifter on every fixture that today exercises `fsm::extract_fsm` and confirming verdict polarity matches. |
| `value_map` symbolic-constant binding in `SignalAnnotation` + `build_variant_to_numeric` ([`kripke.rs:L636`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L636)) | ~180 | Existed for human-readable state names in synthesised controllers. Verification doesn't need it; predicate names suffice. |
| `SignalAbstraction::Enum` variant (with `variants` + `value_map`) | ~150 | Replaced by `predicates: Vec<MuFormula>` per §6.7. Migration: each `Enum { variants: [A, B, C] }` becomes three predicates `is_A`, `is_B`, `is_C`. |
| `SignalAbstraction::BoundedCounter { bound }` variant | ~140 | Replaced by predicates `count == 0`, …, `count == bound`. Migration: each `BoundedCounter { bound: N }` becomes `N + 1` predicates. |

### §9.3 Tier C — frontend replacement; the native pipeline is GONE at the end of S.2b

Per the singular-pipeline commitment, Tier C is unconditional: by the end of S.2b the native parser, AST, kripke builder bulk, multi-module entry points, and `mununu_sv_multi_v1` schema are **deleted**. No `--engine native-sv` escape hatch, no parallel maintenance.

Verdict-polarity match against the R.0c baseline is necessary but **not sufficient** for *automatic* drop-in: the native parser handles SVA (`assert property`, `assume property`, `cover property`), `$past`/`$rose`/`$fell`/`$stable`, and immediate `assert` — constructs that sv2v + Yosys may lower to constants because they are synthesis-irrelevant. If a fixture relies on assertion content that the BTOR2 pipeline silently elides, the new pipeline will "match" the native verdict by passing trivially rather than verifying.

**SVA-elision gating condition** (computed by R.0c). For each fixture, run `sv2v --top <top> *.sv > <fixture>.elab.v` and grep the elaborated output for: `assert property`, `assume property`, `cover property`, `\$past`, `\$rose`, `\$fell`, `\$stable`, `s_eventually`, `s_until`, `s_always`, `disable iff`, `##\d`. If any pattern matches, that fixture is SVA-dependent.

**S.2 has two sub-steps that together remove the native pipeline unconditionally:**

- **S.2a — Fixture migration (does not preserve fixtures intact).** Every SVA-dependent fixture takes one of three resolutions before S.2b can ship:
  1. *Rewrite* — port the inline SVA to a `// @mununu` annotation, a CTXDSL formula, or an LTL pattern from [`builtin_templates.json`](../../crates/mununu-core/src/adapter/templates/builtin_templates.json). The RTL stays; only the property encoding changes.
  2. *Reduce scope* — if the SVA expressed an assumption rather than a guarantee, encode as sidecar `assumptions: Vec<MuFormula>` (§7.3); the RTL stays.
  3. *Retire* — remove the fixture with a recorded reason in `examples/systemverilog/RETIRED.md`. Only when the fixture's value is exclusively its SVA content. Prefer rewrite over retire.

  S.2a is bounded by the SVA-elision gate's output set. In practice most existing fixtures (`traffic_light`, `handshake`, `fifo`, `cwe1245/1260/1262`, `multi_*`, `arbiter`, `spi_master`, `axi4lite_slave`) are RTL-only and pass the gate with no migration work.

- **S.2b — Native pipeline deletion.** Two-step: (1) feature-flag the native parser off and confirm `make ci` passes; (2) remove the flag + dead code in one PR.

| Target | Lines | Removal condition |
|---|---|---|
| Native SV [`parser.rs`](../../crates/mununu-core/src/adapter/systemverilog/parser.rs) | 2 539 | (1) sv2v + Yosys covers all parsed SV constructs (§3.1). (2) Verdict polarity matches the R.0c baseline on the SVA-elision-gate pass set. (3) SVA-elision gate empty for every removed-from-native fixture. |
| Native SV [`ast.rs`](../../crates/mununu-core/src/adapter/systemverilog/ast.rs) | 272 | Goes with `parser.rs`. |
| Bulk of [`kripke.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs) — `build_kripke_with_config`, `enumerate_cross_product` consumer, `compute_next_state` | ~1 800 (of 2 705) | The KMTS lifter replaces the cross-product enumeration. Keep `kripke_smt.rs` as the predicate-image SMT helper. |
| Multi-module entry points + `mununu_sv_multi_v1` schema | ~600 (across `mod.rs` and `annotation.rs`) | The new pipeline drives composition from the top-module BTOR2 netlist (§4), not from a separate multi-module sidecar. Migrate fixtures: `mununu_sv_multi_v1` JSONs become `verify.toml` `[[sources]]` entries with `register_map`-style bindings. |

### §9.4 Tier D — sidecar schema migration, gated on Tier B + C

Shrink `SvAnnotation` to: `{ module, source, signals: Vec<{ name, preserve, abstraction: Predicates | Ignored }>, predicates: Vec<MuFormula>, assumptions: Vec<MuFormula>, uf_wrap: Vec<String>, uf_unwrap: Vec<String>, controllable: Vec<String> }`. The old schema continues to load (one-release deprecation window) but emits a warning pointing at an auto-migration tool.

### §9.5 Totals

Total addressable removal: ~7 400 lines across the SV adapter (~67% of the current ~11 009 lines), plus corresponding shrinkage in `domain.rs` (`AbstractionType` variants), `sidecar/mod.rs` (resolver simplifies), and `state_enum.rs` (cross-product enumeration no longer the primary consumer for SV). The new pipeline lands ~1 500–2 000 lines of new code (BTOR2 lifter, `TruthDomain` trait, KleeneDomain evaluator instantiation, modality merge in `composition/mod.rs`), so the net is a **substantial** reduction in SV-adapter LOC while gaining the full KMTS / 3-valued mu-calculus / UF abstraction story.

## §10 Roadmap, validation milestones, blocker protocol

### §10.1 Roadmap

| Phase | Scope | Effort | Done-criteria |
|---|---|---|---|
| **R.0a — sv2v wrapper** | `mununu sv preprocess` CLI + `AdapterOptions::sv_preprocess: Sv2v`; subprocess discovery + version check. | 1 wk | All `examples/systemverilog/*.sv` round-trip through `sv2v` without warnings; coverage table §3.1 graduates 5 rows from ❌ to ✅. |
| **R.0b — Yosys-no-flatten + BTOR2-per-module** | Modify [`yosys/mod.rs::build_script`](../../crates/mununu-core/src/adapter/yosys/mod.rs#L656) to use `submod -name <m>; write_btor <m>.btor` per submodule; multi-output handling in `translate_sv`. | 1 wk | At least 3 multi-module fixtures emit one BTOR2 per submodule. |
| **R.0c — Pipeline-faithfulness regression harness** (RETIRED by S.2b) | New CLI `mununu sv compare-pipelines <fixture>` that runs both native and BTOR2-per-module paths, diffs verdicts + intermediate-shape stats. Includes SVA-elision gate writing `sva_elision_gate.json`. **Retired when S.2b excised the native parser — `compare-pipelines` + `sv_pipeline_compare` + `tests/sv_compare_pipelines.rs` are gone (singular pipeline = nothing to compare against). M.5 confirmed M.0's surviving frontend-reach intact.** | 1 wk | `kmts_pipeline_baseline.json` for every fixture; CI job runs polarity comparisons. Gates S.0–S.2. |
| **R.1 — KMTS data-model extension** | `TransitionModality { Sharp, MayOnly }` on `Transition`; `state_3valued_predicates` on `Clts`; `TruthDomain` trait surfacing both lattices + `BoolDomain` instantiation; composition modality merge per §6.5; **`composition/mod.rs` audit lands in the same PR for both value-equality + modality-meet**. | 2 wk | All existing `cargo test --workspace` passes unchanged; KleeneDomain produces same verdicts as BoolDomain on Sharp-everywhere CLTSes; composition test asserts the modality-meet derivation. |
| **R.2 — BTOR2 → KMTS lifter** | New [`adapter/btor2/kmts_lift.rs`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs); predicate seeding from BTOR2 IR; predicate-image via extended `kripke_smt`. | 3 wk | At least 5 fixtures produce KMTS via the new lifter; verdicts match the native-parser baseline. |
| **R.3 — KleeneDomain evaluator** | `KleeneDomain` instantiation; modal semantics on may/must split; Kleene iteration over the information order. | 3 wk | Soundness regression suite: every fixture's 3-valued verdict captured in `kmts_verdicts.json`. |
| **R.4 — Property-clustered COI** | Extend [`adapter::partition`](../../crates/mununu-core/src/adapter/partition/) to property-clustered module-level cones; Jaccard helper. | 1 wk | Caliptra fixture shows ≥ 10× state reduction beyond the existing 128× when properties cluster cleanly. |
| **R.5 — CEGAR refinement loop** | On `KleeneBot` verdict, lift abstract counterexample, SMT-discharge, IC3-IA-style predicate discovery, retry. Bounded (default 16 rounds). | 4 wk | One end-to-end refinement run closes a `KleeneBot` verdict. |
| **R.5b — UF abstraction in the lifter** | Default UF wrapping per §6.10 policy; sidecar `uf_wrap`/`uf_unwrap`; two-mode predicate-image; unsat-core partitioning in CEGAR. | 1 wk (rides on R.5) | One wide-multiplier fixture closes a verdict that the concrete-operator pipeline times out on. |
| **S.0 — Tier A removal** | Delete `emit_controller.rs`, `BitBlast` variant, OOB sentinel + annotations. | 1 wk | `cargo test --workspace` green; no fixture regressions. |
| **S.1 — Tier B removal** | Delete `fsm.rs`, `value_map`, `Enum`/`BoundedCounter` variants; migrate fixtures via auto-converter. | 2 wk | All `*.mununu.json` migrated; KMTS lifter handles previous functionality. |
| **S.2a — Fixture migration (SVA-dependent)** | Per §9.3 resolutions: rewrite / reduce-scope / retire. | 1–3 wk (bounded by fixture count failing the gate) | Every fixture passes the SVA-elision gate or has an S.2a resolution recorded. |
| **S.2b — Tier C native-pipeline deletion** | Delete native `parser.rs`, `ast.rs`, bulk of `kripke.rs`, multi-module entry points + schema. Feature-flag off → confirm CI → remove flag + dead code. | 1 wk | `grep -r "SystemVerilogAdapter::translate_with_path"` returns zero hits outside deleted-code archives. CI green. |
| **S.3 — Tier D schema migration** | Shrink `SvAnnotation` schema. One-release deprecation window; migration tool ships. | 1 wk | Old schema loads with deprecation warning; auto-migration tool ships. |
| **U.0 — UI parity for the singular SV pipeline** | Remove pipeline-selector UI; replace `BitBlast`/`Enum`/`BoundedCounter` editors with unified `predicates` editor; 3-valued verdict rendering with `KleeneBot` iconography; refinement-trace viewer; `/parity-check` clean. | 2 wk | `/parity-check` passes on SV-extraction surface; UI renders 3-valued verdicts; no UI control references the deleted native pipeline. |
| **D.0 — Theory docs** | `kmts-theory.md` + `predicate-abstraction-recipe.md` + `abstraction-literature.md` AGR/KMTS section. | 2 wk | All anchors resolve; `/docs-traceability` passes. |
| **D.1 — User-facing doc updates** | `docs/abstraction.md` rewrite; `wiki/Verify-Project-Flow.md` + `wiki/Composition.md`; UI docs reflect U.0. | 1 wk | Wiki snippets re-tested against the binary. |

Total effort: ~30 weeks sequential; ~18 weeks with R.0 / R.1 / D.0 / U.0 parallelism. Milestone validation (§10.3) interleaved at ~0.5 wk per milestone on the happy path.

**End-of-roadmap state.** After S.2b + S.3 + U.0 + M.5 ship, mununu has exactly one SV extraction pipeline (the KMTS path), one sidecar schema (the slimmed `SvAnnotation`), one verdict shape (3-valued), one set of UI controls. The singular-pipeline commitment is verifiable by `grep` (no surviving native-parser references), by `make ci` (no compat shims), by `/parity-check` (no orphan controls), and by the milestone sweep (every M.x verdict polarity preserved).

### §10.2 Milestone blocker protocol

**Hard rule: a milestone failure stops the roadmap until the user arbitrates.** Two failure classes, same protocol:

1. **Technical blocker.** The pipeline fails for a *technical* reason within the milestone's scope — an automated stage errors out, returns the wrong verdict polarity against the oracle, fails to terminate within the documented timeout, or produces an unsound modality/3-valued result. Summary names: which stage failed (sv2v / Yosys / BTOR2 lifter / KMTS lifter / KleeneDomain evaluator / CEGAR / UF), which construct or query triggered it, the smallest reproducer (≤ 50 lines of SV), what the oracle expected, and 1–2 candidate fixes ranked by estimated effort. User decides: (a) absorb the fix into the milestone's phase, (b) defer the fix + document the gap in §11, or (c) re-scope the milestone.

2. **Milestone-workload blocker.** The example turns out operationally too expensive for the planned budget — fixture larger than "small", property requires more predicates than expected, oracle (SBY) does not terminate, upstream has changed shape since the milestone was specified. Summary names: realized cost (LOC, predicate count, oracle wall-clock), planned budget, discrepancy, and two alternative fixtures of similar architectural intent at the right scale. User decides: (a) extend budget, (b) swap fixture, (c) re-scope or drop the milestone.

**No silent retries, no hand-written translations, no manual model extractions.** A blocked milestone must not be rescued by hand-writing a CTXDSL or KMTS version of the fixture — that defeats the milestone's purpose, validating the *automated* pipeline. If the only way past the blocker is hand authoring, the milestone has failed and the user must arbitrate.

**No silent skip.** If a fixture is genuinely unavailable (license revoked, repo moved), propose a replacement and the user approves before continuing.

Blocker summaries live at `.claude/plans/milestones/M-<id>-blocker-<date>.md`; the user's decision is appended to the same file.

### §10.3 Industrially realistic validation milestones (M.0–M.6)

Each milestone runs the fully automated SV → CTXDSL/KMTS pipeline end-to-end on a small but industrially realistic RTL fixture — production silicon from an open-source repo, not a teaching example. Properties are authored via sidecar predicates or `// @mununu` annotations; the model is never hand-extracted.

| Id | Position | Primary fixture | Property | Oracle | Pipeline stage tested |
|---|---|---|---|---|---|
| **M.0** | After R.0c | OpenTitan `prim_arbiter_fixed.sv` (~150 LOC). Alt: ibex `ibex_compressed_decoder.sv`. | Auto-emitted via `// @mununu`: "grant is mutually exclusive — at most one `gnt_o[i]` high per cycle." | SBY safety-mode bounded check, k=8. | sv2v → Yosys-no-flatten → BTOR2 emission; pipeline reach + faithfulness baseline. |
| **M.1** | After R.2 | OpenTitan `uart_tx.sv` (~200 LOC, FSM + baud counter). Alt: small ibex pipeline-control FSM. | Sidecar predicates `{ tx_idle, tx_busy, baud_at_zero }`; safety "`tx_busy ⇒ ¬tx_idle`." | SBY safety-mode k=16. | KMTS lifter on real enum + counter; predicate seeding + basic predicate-image. |
| **M.2** | After R.3 | OpenTitan `hmac_core.sv` or smaller SPI FSM (selected at M.2 planning). | Coarse initial predicate set chosen to *intentionally* yield `KleeneBot`; M.2 success = adding 1–2 predicates refines to `KleeneT`/`KleeneF`. | SBY for safety; 3-valued verdict starts at `KleeneBot` and refines toward SBY's. | KleeneDomain evaluator end-to-end; refinement-by-predicate-addition; lattice usage. |
| **M.3** | After R.4 | OpenTitan AES top + key-load submodule pair (or other 2–3 module assembly at planning). | Batch of 3–5 properties spanning modules; auto-cluster by Jaccard. | Per-property SBY verdicts. | Clustered COI on real multi-module. |
| **M.4** | After R.5b | Primary: Caliptra `soc_ifc_boot_fsm.sv` bug/fix pair (verifying via KMTS+UF, not AVR). Alt: small CRC/multiplier-accumulator. | Sidecar declares UF wrapping for wide-arithmetic cells; safety claim requires CEGAR refinement. | Verdict polarity vs bug/fix pair (for Caliptra) or SBY (synthetic). | UF wrapping survives Yosys `opt`; two-mode predicate-image; CEGAR partitioning; refinement termination. |
| **M.5** | After S.2b | No new fixture; regression sweep of M.0–M.4 through singular pipeline. | Same properties. | Verdict polarity preserved. | Singular-pipeline regression — deletions did not break M.0–M.4. |
| **M.6** | After U.0 | One M.x fixture (typically M.2 for `KleeneBot`+refinement demo). | Same property. | UX validation, not verdict — UI loads, renders 3-valued verdict, shows refinement trace, edits a sidecar predicate, re-runs, shows refined verdict. | UI surfacing end-to-end. |

**Fixture-selection discipline.** Primary fixtures are named with upstream URLs (OpenTitan: github.com/lowRISC/opentitan; ibex: github.com/lowRISC/ibex; Caliptra: github.com/chipsalliance/caliptra-rtl). Alternatives require user sign-off (per §10.2 "no silent skip"). All fixtures must be open-source under a permissive license and small enough to verify within the milestone's 0.5-wk budget.

**No vendored fixtures.** The mununu repo does not check in copies of upstream RTL — the milestone harness pulls them at test time via git submodule or shallow clone (same pattern as `examples/verify/sv_yosys_caliptra_rtl_150/`).

## §11 Risks, deferred, out-of-scope

### §11.1 Risks

1. **sv2v / Yosys version drift.** Same problem the pillow plan has with `yosys`/`avr.py`. Mitigation: discovery + version check + actionable error.
2. **Yosys `opt` eliminates predicate-referenced registers.** Per §3.4: insert `(* keep = 1 *)` pre-pass; hard error in the lifter on missing references.
3. **Synchronisation by value-equality on shared labels.** May require extending [`composition/mod.rs`](../../crates/mununu-core/src/composition/mod.rs#L518) from name-equality. Audit before R.1.
4. **CEGAR non-termination.** Lazy refinement can loop on history-dependent predicates. Mitigation: bounded refinement (default 16 rounds); on cap-hit, keep verdict as `KleeneBot` with soundness-tagged warning.
5. **Tightness loss in compositional KMTS.** Per §7.2: port-equality predicate heuristic; lifter auto-emits canonical predicates for declared connections.
6. **Migration drag.** Sidecar schema change (Tier D) breaks every existing `.mununu.json`. Mitigation: 1-release deprecation window + auto-migration tool.

### §11.2 Deferred (with trigger conditions)

- **Memory abstraction** (UF-style on selected addresses, bounded-content havoc on the rest). Trigger: a fixture with `$mem`/`$mem_v2` cells.
- **Multi-clock fairness encoding** for CDC. Trigger: a fixture with explicit `clock_domain` mismatches in the sidecar.
- **Bisimulation validator** between extracted KMTS and Verilator simulation. Trigger: first soundness-bug post-mortem.
- **3-valued controller synthesis.** Trigger: synthesis is re-prioritised. Until then, synthesis runs on the `BoolDomain`-projected KMTS (Sharp-only transitions); the synthesiser hard-errors when fed a KMTS with non-Sharp transitions.
- **Native SV parser revival** is explicitly *not* a deferred item — it is *removed*. Any future SV construct the KMTS pipeline cannot handle becomes a documented gap to close in the lifter, not a reason to resurrect a parallel path.
- **Hand-written CTXDSL / KMTS rescues of milestone fixtures** are *not* allowed (per §10.2). An automated pipeline that cannot extract a milestone fixture has failed the milestone; contributor stops and asks the user.

### §11.3 Out-of-scope

- Anything in the pillow plan B.0 (AVR bridge) — unchanged by this pivot. Native-SV-via-KMTS and Yosys-via-AVR remain complementary: KMTS handles in-process verification with mu-calculus + synthesis affordances; AVR handles cases beyond the explicit-state cap with safety-only verdicts.
- Bundling sv2v / Yosys / SBY / z3 inside mununu.
- Replacing the existing `Clts` for adapters that don't need predicate abstraction (XState, microcode, agentic). They continue to produce Sharp-everywhere KMTSes vacuously.
- Editing [`mununu-private/`](../../../mununu-private/).
