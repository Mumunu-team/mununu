# Abstraction guidelines for mununu

> Source of truth: [`crates/mununu-core/src/clts/`](../crates/mununu-core/src/clts/) (state-variable + per-state predicate primitives + `TransitionModality` post-R.1), [`crates/mununu-core/src/adapter/btor2/`](../crates/mununu-core/src/adapter/btor2/) (BTOR2 → KMTS lifter, post-R.2), [`crates/mununu-core/src/adapter/systemverilog/`](../crates/mununu-core/src/adapter/systemverilog/) (legacy native-SV adapter; scheduled for removal in S.2b), [`crates/mununu-core/src/mu_calculus/`](../crates/mununu-core/src/mu_calculus/) (`EvalDomain` trait + `BoolDom`/`KleeneDom` instantiations, post-R.3; both evaluator paths unified onto one generic body in P2.2/P2.3) — surface: CLI+API+UI.

## Why this doc exists

Mununu's verdicts are correct **for the model**. Whether they transfer to the real system depends entirely on the abstraction the user (or adapter) picks. The [`Soundness Guarantees`](../CLAUDE.md#soundness-guarantees) section of CLAUDE.md states the soundness directions; this doc gives the **per-subsystem recipe** — *which* mununu primitive to reach for, *when*, and what each primitive preserves.

For how the abstraction fits the wider engine picture — the frontend-agnostic STS-IR seam, the **explicit vs symbolic (BDD, R-F5)** engines, the IR layering, and how over/under/⊥ approximation and may/must edges operate end-to-end — see [`docs/design/post-rf5-architecture.md`](design/post-rf5-architecture.md).

Use this doc when:

- You are authoring an adapter and have to decide how to encode a multi-bit register, a memory region, or a wide-arithmetic operator.
- You are hand-writing a CTXDSL source for a peripheral, microprogram, cache, bus, or protocol spec.
- You are reviewing a `// SOUNDNESS:` annotation and want a checklist.
- You are deciding whether automated extraction is feasible for a new subsystem class or whether the user must hand-author it.

## The canonical recipe — KMTS predicates

> **Status (2026-05-21).** The KMTS pipeline is the **canonical recipe** for SystemVerilog and BTOR2 extraction. Other adapters (XState, microcode, agentic, hand-written CTXDSL) continue to use the legacy primitives (§"Legacy primitives" below) and produce Sharp-everywhere KMTSes vacuously. Architecture: [`docs/design/native-sv-abstraction.md`](design/native-sv-abstraction.md). Theory: [`docs/design/kmts-theory.md`](design/kmts-theory.md). Practical recipe: [`docs/design/predicate-abstraction-recipe.md`](design/predicate-abstraction-recipe.md). Worked example (RTL → BTOR2 → coarse `KleeneBot` → Craig interpolant → refined `KleeneT`): [`docs/design/predicate-abstraction-worked-example.md`](design/predicate-abstraction-worked-example.md).

The canonical recipe has four moving parts: a **predicate set** that partitions the abstract state space, a **modality** that captures the may/must distinction on each transition, a **3-valued evaluator** that returns `KleeneT / KleeneF / KleeneBot`, and a **CEGAR refinement loop** that responds to `KleeneBot` verdicts by adding predicates.

### The four primitives at a glance

| Primitive | What it is | Where it lands | Source of truth (post-R.x) |
|---|---|---|---|
| **Predicate set** `P` | Finite set of Boolean expressions over module signals; each defines one bit of the abstract state | Sidecar `predicates: Vec<MuFormula>` per module; auto-derived from property atoms, COI register-equality, and typedef enums | [`predicate-abstraction-recipe.md`](design/predicate-abstraction-recipe.md) §2 |
| **`TransitionModality`** | `Sharp` (in both `may` and `must`) or `MayOnly` (over-approximation, no must-witness) | [`Transition::modality`](../crates/mununu-core/src/clts/mod.rs) per transition; default `Sharp` for legacy adapters | [`kmts-theory.md`](design/kmts-theory.md) §2.2; post-R.1 |
| **`Tristate`** | `KleeneT` / `KleeneF` / `KleeneBot` per `(state, predicate)` | Optional [`state_3valued_predicates`](../crates/mununu-core/src/clts/mod.rs) on `Clts`; `None` for legacy adapters | [`kmts-theory.md`](design/kmts-theory.md) §2.5; post-R.1 |
| **CEGAR refinement** | On `KleeneBot` verdict, extract the failure subgame, discharge via SMT, find a separating predicate (WP / Craig interpolation via CVC5), add predicates | [`cegar_refine_loop`](../crates/mununu-core/src/adapter/btor2/cegar.rs) (R.5, shipped); bounded by `max_iterations` (default 16) | [`predicate-abstraction-recipe.md`](design/predicate-abstraction-recipe.md) §4 (live) |

### Soundness in one paragraph

KMTS abstraction is **uniformly sound for the full mu-calculus including liveness** (Bruns–Godefroid CONCUR 2000): a `KleeneT` verdict on the abstract transfers to `true` on the concrete; a `KleeneF` verdict transfers to `false`; only `KleeneBot` requires refinement. The asymmetry between `may` (over-approximation, used for `KleeneT` claims on `[a]φ` and `KleeneF` claims on `⟨a⟩φ`) and `must` (under-approximation, used for `KleeneF` claims on `[a]φ` and `KleeneT` claims on `⟨a⟩φ`) is what makes safety, reachability, and liveness all soundly decidable under a single abstract model. The pre-KMTS 2-valued abstraction (the "legacy primitives" section below) is sound only for safety / pure greatest fixpoints under over-approximation, not for the full mu-calculus.

**For the BTOR2 predicate cube specifically**, the transfer above holds for the fragment the cube path actually evaluates — `Control::All`, bare (label-agnostic), unbounded modalities — and rests on two premises now mechanised in CI: the lift is a sound may/must KMTS (`may ⊇ concrete`, `must ⊆ concrete`) and the evaluator computes the §4.3 modal semantics. Out-of-fragment modalities (bounded `steps=k`, label-specific on a non-cube label) are **flagged with a soundness warning, not silently decided**. Controllability (`ctrl=…`) left that set on 2026-06-23 (PO-3 / R.6.8 closed): a *definite* controllability verdict is now computed by the de Alfaro–Godefroid–Jagadeesan per-player rule and is sound (`evaluate_tri` routes `Control::{Controllable, Environment}` through `modal_trit_from_target`). Full write-up: [`predicate-abstraction-recipe.md`](design/predicate-abstraction-recipe.md) §4.9.

### When the canonical recipe applies

- **SystemVerilog** (post-S.2b — the native parser is deleted): the sv2v → Yosys-no-flatten → BTOR2-per-module → KMTS lifter pipeline is the only path. Adopt predicates instead of `BoundedCounter` / `Enum` / `BitBlast` / `Discover` per the migration table below.
- **BTOR2**: the KMTS lifter consumes BTOR2 directly. Same predicate primitives apply.
- **C-codesign**: the lifter's predicate-image is BV-only today; richer theories (arrays for memories) deferred. Use the legacy `c-codesign` adapter for current C extraction work; KMTS for C is gated on a memory-cell abstraction roadmap item.

### When to stay on the legacy primitives

XState, microcode, agentic adapters, and hand-written CTXDSL sources continue to produce **Sharp-everywhere KMTSes** — every transition is `Sharp`, every predicate valuation is two-valued, no `KleeneBot` ever appears in the verdict. These adapters use the legacy `AbstractionType::*` primitives (Boolean / Symbols / Ignored / per-state predicates / multi-label transitions / rich modal guards) without change. The `KleeneDom` evaluator on such a CLTS returns verdicts in `{KleeneT, KleeneF}` only, identical to today's 2-valued `{true, false}` semantics. The `BoolDom` monomorphisation of the evaluator computes the same verdicts more cheaply and is the default for Sharp-only adapters.

### Migration table — legacy `AbstractionType::*` → KMTS predicates

This table is the operational recipe for porting an existing sidecar to the post-S.3 schema. The S.1 / S.3 auto-migration tool applies it mechanically; this table is what the tool implements.

| Legacy variant | KMTS replacement | Rationale |
|---|---|---|
| `Boolean` | One predicate: `{ name: "<reg>_high", formula: "<reg> != 0" }` | A Boolean abstraction is one bit of the predicate cube. |
| `BitBlast { width }` (cap 4) | `width` predicates: `{ name: "<reg>_bit<i>", formula: "<reg>[<i>] == 1" }` for `i in 0..width` | One predicate per bit; the predicate-image computation reconstructs the cube on demand. |
| `BoundedCounter { bound: N }` | `N + 1` predicates: `{ name: "<reg>_eq<i>", formula: "<reg> == <i>" }` for `i in 0..=N` | The bounded counter's domain becomes the predicate set; CEGAR refines via IC3-IA interpolation if `>= N` matters. |
| `Enum { variants: [V_1, …, V_n] }` | `n` predicates: `{ name: "is_<V_i>", formula: "<reg> == <V_i>" }` | Typedef enums seed predicates directly; the lifter pulls variant info from BTOR2 metadata. |
| `Discover` (default for the native SV adapter) | Replaced by predicate seeding from the property's atomic propositions + COI register-equality (auto-derived). User-supplied predicates extend the auto-derived set. | The constants-discovery step (`kripke_smt::discover_significant_values`) becomes the predicate-image computation in `kmts_lift.rs`. |
| `Ignored` | Unchanged: `signals: [{ name: "<reg>", preserve: false }]`. Outside the COI is also dropped automatically by `adapter::partition`. | Cone-of-influence pruning is orthogonal to KMTS; same primitive. |

## Automatic cone-of-influence (Phase A.3 — orthogonal to KMTS)

> Source of truth: [`adapter/partition/mod.rs`](../crates/mununu-core/src/adapter/partition/mod.rs) — surface: CLI+API.

mununu's BTOR2 and SystemVerilog adapters run an **automatic cone-of-influence pass** during translation. The pass walks the frontend IR's signal dependency graph from the seed atoms extracted from the property formulas (intrinsic `bad`/`constraint`/`justice` lines on BTOR2, atoms in `@mununu` property comments on SV), keeps every transitively-reachable state cell and input, and pins everything else to [`AbstractionType::Ignored`](../crates/mununu-core/src/adapter/domain.rs).

COI is **exact** (per [`docs/design/native-sv-abstraction.md`](design/native-sv-abstraction.md) §5): the cone's behaviour projected onto the property's atomic-proposition set is bisimilar to the full system's, so COI is sound *and complete* for the full mu-calculus over Σ_φ — including liveness, including alternating fixpoints. No approximation. R.4 ships property-clustered COI (Jaccard-similarity grouping of property cones) as an extension.

**User wins on collision.** Auto-COI only acts on signals the sidecar does **not** mention. If a signal appears in `.mununu.json`'s `signals[]`, `inputs[]`, or `predicates[]` with any explicit declaration, that declaration wins regardless of the auto-COI verdict. The three layers compose:

1. The sidecar carries user-curated abstractions (`predicates: Vec<MuFormula>` post-S.3; legacy `Boolean`/`Symbols`/`Ignored` pre-S.3).
2. The auto-derived predicate set (property APs + COI register-equality + typedef enums) extends the user-curated set for the canonical KMTS recipe.
3. Auto-COI fills the gap for signals neither layer mentions, dropping them to `Ignored` when they are outside the property's reach.

**Soundness.** COI dropping is exact, not over-approximation. Sound for everything.

**Defensive default for unbindable seeds.** If the adapter cannot extract any property atoms (e.g., a BTOR2 file whose `bad` line traces only through anonymous compiler-synthesised state cells), the partition keeps every signal rather than drop everything silently. This avoids accidental under-approximation when the property structure is opaque to the partition's syntactic scan.

**Observability.** Every adapter populates [`AdapterOutput.partition_summary`](../crates/mununu-core/src/adapter/mod.rs) with the counts and bit-widths. The verify orchestrator threads this through `SourceSummary.partition_summary` in `VerifyReport`; CLI warnings surface each `Dropped` signal (`"auto-partition: state cell 'X' dropped (outside-cone-of-influence); add an explicit sidecar entry to override"`).

**Opt-out.** `PartitionOptions::disabled = true` returns every signal as `Kept` — useful for regression triage but not exposed on the CLI today.

## Uninterpreted-function abstraction for wide arithmetic

> Source of truth: KMTS lifter post-R.5b; recipe in [`predicate-abstraction-recipe.md`](design/predicate-abstraction-recipe.md) §3b + §6.10 of the architecture doc.

Predicate abstraction abstracts the **state space**; UF abstraction abstracts the **operations**. The two axes are orthogonal and the canonical KMTS recipe uses both: wide-arithmetic cells (`$mul` / `$div` / `$mod` / `$pow` unconditionally; `$add` / `$sub` for width > 32 bits) are wrapped as uninterpreted functions with functional consistency as the only axiom. This drops predicate-image SMT queries from QF_BV (slow on multipliers) into EUF + linear bitvector (much faster).

**Soundness asymmetry.** UF abstraction is a **may-side-only over-approximation**. May-mode predicate-image queries (`R_may`) use the UF-abstracted relation — sound because UF only adds may-behaviour. Must-mode queries (`R_must`) use concrete operators — UF is unsound here because a "∀s, ∃s' under UF" witness does not transfer to "∀s, ∃s' under concrete." The lifter enforces this by switching the UF wrapper off for must-mode queries.

**User overrides.** Sidecar `uf_wrap: Vec<String>` forces specific cell instances into UF mode; `uf_unwrap: Vec<String>` forces concretisation. Default policy applies otherwise.

**CEGAR refinement on two axes.** When a `KleeneBot` verdict's spuriousness check returns UNSAT, the unsat core distinguishes:
- Bitvector constants → state-distinguishability gap → add predicates.
- UF instance terms → operator-behaviour gap → either selectively concretise the UF instance or add a learned-lemma axiom (`a * 0 = 0`, etc.).

One interpolation query, two refinement decisions, partitioned by symbol kind.

## Legacy abstraction primitives (still used by non-RTL adapters)

> Source of truth: [`crates/mununu-core/src/adapter/domain.rs`](../crates/mununu-core/src/adapter/domain.rs) — surface: CLI+API+UI.

The pre-KMTS primitives remain canonical for adapters that do not need predicate abstraction (XState, microcode, agentic, hand-written CTXDSL). These adapters produce Sharp-everywhere KMTSes with two-valued AP labellings; the KleeneDom evaluator reduces to 2-valued semantics on them.

| Primitive | What it abstracts | Surface where exposed | Soundness | Status |
|---|---|---|---|---|
| `AbstractionType::Boolean` | Multi-bit signal → `{ low, high }` | CTXDSL `boolean` variables; legacy SV sidecar `domain.abstraction` (pre-S.3) | Sound iff property does not distinguish values within the same equivalence class | Shipped |
| `AbstractionType::Interval { min, max }` | Signal → finite set of integer intervals | Legacy SV sidecar (pre-S.3) | Same as above | Shipped |
| `AbstractionType::Symbols(Vec<String>)` | Signal → named representative values + "other" | CTXDSL enum types; legacy SV sidecar (pre-S.3) | Same as above | Shipped |
| `AbstractionType::Ignored` | Drop signal from state space | Any sidecar; preserved across S.3 | Sound for safety (model permits every concrete value); under-approximates liveness | Shipped |
| Per-state predicate (`state_variable_bitset`) | Lift state name → mu-calculus predicate | [`clts/mod.rs`](../crates/mununu-core/src/clts/mod.rs); CTXDSL `predicates { … }` | Exact (no abstraction loss) | Shipped |
| Per-state structured valuation | Hand-write display metadata on state | CTXDSL `state S { valuations { … } }`; `ContextDoc.state_valuations` | Exact (display-only by default; formal when paired with predicates) | Shipped |
| Multi-label transitions | **Synchronisation vector**: the label SET is ONE compound action | CTXDSL `transition s -> t on label a, label b;`; `SmallVec<[LabelId; 4]>` | Exact for a single automaton. **Under composition it RESTRICTS**: see the warning below | Shipped |
| Rich modal guards | One `[…]` / `<…>` combining labels + current/next predicates + controllability class + step bound | CTXDSL `[(labels = {a}, req_next = {active}, ctrl = controllable)] φ`; `Guard` struct | Exact | Shipped, under-used |
| Chaotic stub on a label set | 1 state + self-loop per label for an unmodelled subsystem | hand-authored CTXDSL; future `codesign emit-chaotic-stub` generator | Sound for safety (over-approximation); **optimistic for liveness** | Pattern shipped; auto-gen pending |
| Hide / reclassify-as-internal | Hide labels from observable alphabet at evaluation time | `mu_calculus::evaluate_with_options` `hide: Vec<String>`; `/context/verify` API `hide` | Preserves safety; **can lose liveness** (hidden label cannot be required) | Shipped |
| Bisimulation minimization | Quotient composed automaton over observable behaviour | `evaluate_with_options` `minimize: true`; API `minimize` | Preserves all CLTS-observable behaviour (full bisimulation equivalence) | Shipped |
| Compositional stubs | Replace one source with `.espec.json` stub at verify time | `/context/verify` API `stubs`; matching CLI flag | Soundness depends on stub posture (chaotic vs. constrained) | Shipped |
| Domain profiles | Bias AST extractor to one of `software` / `rtl` / `agentic` / `synthesis` / `universal` | `mununu-extract --domain`; `extraction/ast_extract/domain.rs` | Profile chooses defaults but does not enforce soundness — must be declared per-extraction | Shipped |
| Mode filtering | One spec → multiple abstraction levels (e.g. `fixed` vs `vulnerable`) | `.espec.json` `mode` field | Per-mode posture declared inline | Shipped |

### ⚠️ Multi-label transitions are synchronisation vectors, not alternation (mununu#570)

> Source of truth: [`compose`](../crates/mununu-core/src/composition/mod.rs) — the pairing rule is a single line: two transitions pair only when their projections onto the **shared alphabet** are EQUAL as sets.

`transition s -> t on label a, label b;` is **one compound action identified by the set `{a, b}`**.
It does **not** fire on `a`, and it does **not** fire on `b`. It synchronises only with a partner
transition whose shared-alphabet projection is exactly `{a, b}`.

**This row's soundness column used to read "Exact" with the description "collapse parallel edges".
Both halves were wrong**, and the difference is large enough to reduce a 7-automaton composition to
one reachable state with a safety property "holding" vacuously.

The worked case. `Multi` is the same in all four; only the partner changes:

```
automaton Multi {
    controllable { label a; label b; }
    states { state S0 initial; state S1; }
    transitions { transition S0 -> S1 on label a, label b; }   // ONE action: {a,b}
}
```

| partner's transitions | shared alphabet | `Multi` projects to | partner projects to | `EF S1` |
|---|---|---|---|---|
| `U0->U0 on a;` **and** `U0->U0 on b;` | `{a,b}` | `{a,b}` | `{a}` / `{b}` | **VIOLATED** — neither matches |
| `U0->U0 on b;` only | `{b}` | `{b}` | `{b}` | HOLDS |
| `U0->U0 on a;` only | `{a}` | `{a}` | `{a}` | HOLDS |
| `U0->U0 on a, label b;` | `{a,b}` | `{a,b}` | `{a,b}` | HOLDS |

**Row 1 is the trap, and it is not monotone.** Its partner permits strictly *more* behaviour than
row 2's, yet the composition permits strictly *less* — it cannot move at all. That is because
adding `a` to the partner does not merely grant a capability, it **enlarges the shared alphabet**,
which retroactively changes `Multi`'s own projection from `{b}` to `{a,b}`. The alphabet is
structural, not a permission set. This is ordinary alphabetised-parallel behaviour in the CSP
family; it is only surprising because the comma reads like "or".

**When you want alternation, write parallel edges.** An "I ignore the other automata's labels"
self-loop is the common case, and the collapsed form is exactly wrong for it:

```
// WRONG — one unsatisfiable compound action; this automaton freezes under composition.
transition Live -> Live on label ld_write, label ld_done, label cpu_write;

// RIGHT — three alternatives, any one of which may fire.
transition Live -> Live on label ld_write;
transition Live -> Live on label ld_done;
transition Live -> Live on label cpu_write;
```

**When you want conjunction, the collapsed form is correct and load-bearing.** A turn-based round
carrying both an environment and a controller action (`emit.rs`'s `{env_*, ctrl_*}` pairs, the
Skolem/Mealy game encoding) and a handshake where two parties must move together
(`examples/verify/v10_mem_fabric_client_mux`, `on label grant, label refuse`) both mean *and*, and
both would be silently wrong as parallel edges.

### Variants scheduled for removal in S.0 / S.1

The KMTS pivot's simplification phase removes the SV-specific synthesis-tuned variants. They are listed here to signal "do not start new use cases" for these primitives; the migration table above maps each to its KMTS replacement.

| Variant | Removal phase | Replacement |
|---|---|---|
| `AbstractionType::BitBlast { width }` | S.0 | Per-bit predicates |
| `SignalAbstraction::Enum { variants, value_map? }` | S.1 | One predicate per variant |
| `SignalAbstraction::BoundedCounter { bound }` | S.1 | One predicate per value in `0..=bound` |
| `SignalAbstraction::Discover` | S.1 | Auto-derived predicates from property APs + COI |

XState, microcode, agentic adapters do not use these SV-specific variants and are unaffected.

## The rule of thumb for automated extraction vs hand-authoring

> **Automated extraction is viable for a subsystem when the abstraction shape is** *uniform across instances* **and the source format carries enough structural information for an adapter to instantiate it mechanically.**

This holds for:

- **SystemVerilog / BTOR2** — the KMTS pipeline. sv2v elaborates SV-2017 to a Verilog-2005 subset; Yosys-no-flatten preserves hierarchy; BTOR2-per-module retains word-level types; the lifter seeds predicates from property APs + COI + typedef enums.
- **Firmware drivers** — `c-codesign` adapter. The register-map sidecar carries the structural information (which signals are MMIO, what their direction is).
- **Restricted microprograms** — shipped via the [`microcode` adapter](../crates/mununu-core/src/adapter/microcode/) (plan Part 5 + Part 5.5). JSON input; `regs` / `mem` / `interrupts` declarations carry the structural information; ops emit canonical rendezvous labels (`wr_mem_<region>`, `rd_mem_<region>`, `fence_<order>`, `irq_ack_<source>`). See `examples/verify/rv5_2core_mesi_microcode_extracted/` (parity) and `examples/verify/dma_engine_microcode/` (industrial DMA demo).
- **Word-level arithmetic inside firmware / microcode** — subsumed by the host adapter's register-abstraction infrastructure; no separate extraction path.
- **Shared memory regions referenced by tracked addresses** — a chaotic-stub generator parameterised by an address list is mechanical.

It does **not** hold for:

- **Pipelines** — abstraction shape is uniform across CPU classes (5-stage in-order, 6-stage with forwarding, …) but no source format carries the structural information. Hand-authored CTXDSL or a **parameterised library template** is the right pattern.
- **Cache controllers** (MESI / MSI / MOSI / dragon, …) — same as pipelines.
- **Coherence buses** — same.
- **Interrupt controllers (PLIC, NVIC, …)** — same.
- **Bespoke critical subsystems** (watchdog, DMA, MMU, debug unit) — narrow utility per template; hand-authored CTXDSL is realistic.

This distinction matters because it tells you where the next investment goes: "build an extractor" versus "ship a parameterised template library". They cost different orders of magnitude — adapters are ~1.5k LOC each plus a per-format learning curve; CTXDSL library templates are ~100-300 LOC plus a small instantiation tool.

## Per-subsystem recipe

For each concrete subsystem class, the **minimum** abstraction that keeps the model tractable plus the primitive that supplies it. Recipes use the canonical KMTS primitives where the adapter is the KMTS lifter; the legacy primitives where the adapter is XState / microcode / agentic / hand-written CTXDSL.

### Word operations (32/64-bit arithmetic, shifts, comparisons)

- **Concrete content.** Arbitrary integer operations on register-resident values.
- **What to abstract.** For RTL: wrap wide-arithmetic operators (`$mul`, `$div`, `$mod`, `$pow` unconditionally; `$add` / `$sub` for width > 32) with UF symbols per §UF above; the predicate-image computation populates may/must relations under the UF abstraction (may) and concrete operators (must). For non-RTL: treat each operand as the abstraction class of its source register; operations have no observable label.
- **Primitive.** **KMTS path:** UF wrapping + predicates on the operand registers. **Legacy path:** `AbstractionType::Symbols` for register values.
- **Automated extraction?** Yes — the KMTS lifter handles arithmetic via UF. Yes via `c-codesign` (LLVM SSA collapses arithmetic). Yes via the [`microcode` adapter](../crates/mununu-core/src/adapter/microcode/).
- **Soundness.** Sound iff property does not distinguish values within the same predicate cube. UF over-approximation may produce more `KleeneBot` verdicts on wide arithmetic; CEGAR refines via UF concretisation or learned-lemma addition.

### Memory (general-address, multi-GB)

- **Concrete content.** Byte-addressable RAM, infinitely many possible values per byte.
- **What to abstract.** Only tracked addresses (declared in microcode `mem { … }` or referenced by the cache's tracked-line set) are modelled. Per-address state is a small symbol set: `{ initial, written_by_<src>, observed_by_<sink> }` for provenance-tracking properties; `{ stale, fresh }` otherwise.
- **Primitive.** **KMTS path:** one predicate per tracked-address value class. Memory cells in BTOR2 (`$mem`/`$mem_v2`) are currently deferred (§11 of the architecture doc); when the lifter learns array theory, predicates over array selects become the primitive. **Legacy path:** `AbstractionType::Symbols` per address; chaotic stub on the untracked-address majority.
- **Automated extraction?** Yes via a chaotic-stub generator parameterised by the tracked-address list — pending implementation. Memory-cell KMTS abstraction deferred.
- **Soundness.** Tracked-address restriction is sound for safety properties referencing only tracked addresses. Chaotic stub over untracked addresses is sound for safety, optimistic for liveness.

See the **[Memory soundness matrix](#memory-soundness-matrix)** below for the per-posture × per-property-class breakdown and the declaration shape (`[sources.memory_abstraction]`) that records the choice in `verify.toml`.

### Pipelines (per CPU core)

- **Concrete content.** N pipeline registers carrying instruction + control fields, hazard logic, forwarding paths.
- **What to abstract.** Per-stage occupancy as one predicate per stage (`stage_i_busy`). Forwarding encoded as multi-label transitions. Branch flush encoded via rich modal guards. No cycle-accurate timing.
- **Primitive.** **KMTS path:** one predicate per stage; multi-label transitions; rich modal guards. **Legacy path:** `AbstractionType::Boolean` for occupancy.
- **Automated extraction?** No — the abstraction is semantic. Library template feasible.
- **Soundness.** Sound iff the property does not require cycle-accurate timing. Document `// SOUNDNESS: pipeline occupancy is Boolean-abstracted; cycle-level hazards are not modelled.` inline.

### Caches (per core, per line)

- **Concrete content.** Cache memory (KB to MB), tag array, coherence state bits per line.
- **What to abstract.** Memory content not tracked. Tracked lines = small hand-picked set (1-4 typically). Per-line state = one predicate per protocol-state variant (e.g. `is_M_lineX`, `is_E_lineX`, `is_S_lineX`, `is_I_lineX` for MESI).
- **Primitive.** **KMTS path:** predicates per (line, state) pair. **Legacy path:** `AbstractionType::Symbols` per line; per-state predicate to lift `M_lineX` into a mu-calculus-usable predicate.
- **Automated extraction?** Partial — library template parameterised by `<N>` cores × `<M>` lines.
- **Soundness.** Sound iff the property references only tracked lines and the protocol's symbol set is exhaustive.

### Coherence buses

- **Concrete content.** Arbitration, snoop requests, invalidations, write-backs.
- **What to abstract.** Bus cycles → discrete events. Arbitration fairness → fairness annotation only if liveness is being verified. At most one outstanding transaction per line in the simplest model.
- **Primitive.** Multi-label transitions on rendezvous events.
- **Automated extraction?** No. Hand-authored CTXDSL with a library-template path for common protocols (MESI bus, AXI ACE) as follow-up.
- **Soundness.** Sound when arbitration determinism is modelled accurately enough for the property; chaotic-stub variant sound for safety only.

### Interrupt controllers (PLIC / NVIC)

- **Concrete content.** Per-source pending + priority + enable; arbiter; per-hart claim/complete.
- **What to abstract.** Pending state Boolean per tracked source (one predicate `is_pending_src_i`). Priority either dropped or symbolic (one predicate per priority class). Claim/complete as discrete events.
- **Primitive.** **KMTS path:** predicates per source. **Legacy path:** `AbstractionType::Boolean` per source; per-state predicates.
- **Automated extraction?** Partial — library template feasible.
- **Soundness.** Same as caches.

### Critical coexisting subsystems (watchdog, DMA, MMU, debug)

- **Concrete content.** Subsystem-specific.
- **What to abstract.** A small CLTS automaton (3-10 states) modelling only the interaction with bus / interrupt interfaces. Internal state not visible at those interfaces is dropped.
- **Primitive.** Hand-authored CTXDSL; KMTS lifter if RTL exists.
- **Automated extraction?** No for the general case via hand-written CTXDSL; yes via the KMTS lifter from RTL.
- **Soundness.** Sound iff the externally-visible abstraction matches the actual interaction protocol.

### Firmware drivers

- **Concrete content.** Real C source.
- **What to abstract.** Memory-mapped accesses → rendezvous labels via the register-map sidecar. Non-MMIO code → internal events. Loops → bounded or under-approximated (with a documented soundness note).
- **Primitive.** `c-codesign` adapter (shipped).
- **Automated extraction?** Yes — most mature path mununu has.
- **Soundness.** Documented inline by the adapter; loop bounding is the main soundness-relevant choice.

## Memory soundness matrix

> Source of truth: [`MemoryAbstractionPosture`](../crates/mununu-core/src/verify/config.rs) (verify-framework declaration), [`docs/design/black-box-modules.md`](design/black-box-modules.md) (chaotic-stub foundations, Doc A §A.4), [`docs/design/hw-sw-codesign-extraction.md`](design/hw-sw-codesign-extraction.md) (HW/SW codesign chaotic-stub formulation, Doc C §C.5) — surface: CLI+API+UI (declared in `verify.toml`, consumed by the orchestrator and surfaced in summaries).

Memory abstraction is the single biggest soundness-relevant choice in any non-trivial verification target. The matrix is unchanged by the KMTS pivot — the four canonical postures interact with the canonical KMTS recipe through the predicate set (per-address predicates of the form `mem.x.fresh = (mem[x] == fresh)`) rather than through legacy `Symbols` abstractions, but the soundness story per posture is identical.

### The four postures

| `kind` | What is modelled | What is dropped | When to reach for it |
|---|---|---|---|
| `chaotic` | Self-loop on every memory label; no per-address state | All address discrimination; all value information; all ordering | Smoke test that the surrounding system terminates / does not deadlock regardless of memory behaviour. Always sound for safety; **never sound for any property that depends on what memory returns**. |
| `tracked_addresses` | Per-address presence/absence in a small declared set; chaotic over the rest | Per-address *values*; ordering across addresses unless fences enforce it | Safety properties that read **which** address was last touched but not **what value** it holds — e.g. "the watchdog kick happens before the deadline register write". |
| `tracked_with_values` | Per-tracked-address state from a declared symbol set (`stale`/`fresh`, `initial`/`written_by_X`/…); chaotic over untracked | Concrete numeric values within a symbol class; behaviour of untracked addresses | The default for memory-correctness properties — "did the microprogram's write to X become observable before the load on the other core?". The symbol set is the verifier's choice of equivalence class. |
| `full_concrete` | Every modelled address carries its concrete integer value | Nothing (within the declared address range) | Only for tiny address ranges (a handful of registers) where the property genuinely depends on numeric values — e.g. a checksum register accumulating a known sequence. State-space cost is exponential in the value width. |

Posture choice is **monotone in expressivity** from `chaotic` to `full_concrete`: each strictly extends what the model can distinguish, at strictly higher state-space cost.

### The three fence semantics

`fence_semantics`, when set on a posture, declares how a microcode `fence` op or an architectural barrier is interpreted in the model. The choice is **independent of `kind`** — any posture can pair with any fence semantics, but some combinations are pointless (chaotic + rvwmo gains nothing).

| `fence_semantics` | What the fence enforces | What it preserves | Soundness caveat |
|---|---|---|---|
| `global_barrier` | All pending writes globally ordered before all subsequent reads, across every source | "After fence, every tracked write is observable to every source" | **Strictly stronger than any real architecture**; sound *as a model assumption* but the user must justify the gap to RVWMO / TSO / SC if claims transfer to real hardware. |
| `release_acquire` | Per-source release-store and acquire-load semantics; happens-before chains via fences | Standard acquire/release reasoning ("if a release-store on core 0 is observed by an acquire-load on core 1, every prior write on core 0 is observable") | Sound for properties expressible in the release/acquire fragment; loses behaviours from non-RA atomics. |
| `rvwmo` | RISC-V Weak Memory Order, modelled per the [public spec](https://github.com/riscv/riscv-isa-manual) | Whatever the embedded RVWMO encoding preserves | **Not implemented in mununu today.** Declaring `rvwmo` is currently an **aspirational** annotation — the orchestrator does not yet enforce RVWMO semantics. The declaration is honoured by `mununu memory check` (when shipped) as a signal that the user understands the gap; it does not change verdicts. |

### Posture × property class — soundness matrix

The **claim transfer direction** is from model verdict → real system. ✓ = transfer is sound (within the modeled scope of declared tracked addresses). ⚠ = transfer is sound only with caveats spelt out below the table. ✗ = transfer is not sound — the verdict is true of the model but says nothing about the real system.

| Property class | `chaotic` | `tracked_addresses` | `tracked_with_values` | `full_concrete` |
|---|---|---|---|---|
| **Safety, no-memory mention** (e.g. "no double-DVFS") | ✓ | ✓ | ✓ | ✓ |
| **Safety, references tracked memory presence** (e.g. "address X written before Y") | ✗ | ✓ | ✓ | ✓ |
| **Safety, references tracked memory values** (e.g. "if X contains `fresh`, then …") | ✗ | ✗ | ✓ | ✓ |
| **Safety, references untracked memory** (any property that reads an address outside `tracked`) | ✗ | ✗ | ✗ | ✗ (use full_concrete only over the *declared* address range) |
| **Liveness, no-memory mention** (e.g. "every request eventually serviced") | ⚠ | ⚠ | ⚠ | ⚠ |
| **Liveness, references tracked memory** (e.g. "every write eventually observable") | ✗ | ⚠ | ⚠ | ✓ |

**Caveats.**

- **All liveness rows carry ⚠** because mununu's composition is asynchronous by default and chaotic / over-approximated memory **admits noop loops**. Without an explicit fairness constraint, the model can satisfy "eventually X" only by demonstrating the existence of a fair execution. Liveness verdicts are honest about the model but pessimistic for the real system: a real bus enforces forward progress that the chaotic-stub model does not. Use `tracked_with_values` + `release_acquire` (or `global_barrier`) + a fairness annotation on the bus source before drawing liveness conclusions.
- **The "references untracked memory" row is uniformly ✗** by construction. Reaching that row means the user named an address that is not in `tracked`. The orchestrator does not detect this today — `mununu memory check` (B2b, pending) will surface the mismatch.
- **`tracked_addresses` + value-mention property** is the most common authoring mistake: the property formula refers to a per-address value class (`mem.x.fresh`) but the posture does not encode values. The verdict on the model is meaningless. `mununu memory check` will flag this.
- **`full_concrete` + large value width** is **not** a soundness problem but a tractability one: the state space grows as `(value-domain-size)^(number-of-tracked-addresses)`. Stay under a few thousand combined states unless the verifier has memory budget for millions.

### Posture × fence-semantics — preservation matrix

For a memory-order property (e.g. "if core 0 stores X then fences, the store is visible to core 1's subsequent load"):

| Posture | `global_barrier` | `release_acquire` | `rvwmo` |
|---|---|---|---|
| `chaotic` | n/a (chaotic posture has no ordering to enforce) | n/a | n/a |
| `tracked_addresses` | Ordering enforced on presence-of-write only | Ordering enforced on per-source release/acquire labels | Aspirational; not enforced |
| `tracked_with_values` | Ordering enforced on value-class transitions across addresses | Per-source acquire/release ordering on value-class transitions | Aspirational; not enforced |
| `full_concrete` | Strongest possible ordering; equivalent to SC | Per-source acquire/release with concrete values | Aspirational; not enforced |

The honest path for v1 multicore verification: pair `tracked_with_values` with `release_acquire`, and **declare the gap to RVWMO in the `notes` field** of `[sources.memory_abstraction]`. The verdict is sound for what the model encodes; transfer to a real RISC-V system requires either a sound abstraction argument (e.g. "this property is in the SC subset that RVWMO preserves") or an external memory-model checker (Herd / RMEM) integrated as a future adapter.

### Declaring the posture in `verify.toml`

> Source of truth: [`MemoryAbstractionPosture`](../crates/mununu-core/src/verify/config.rs) — surface: CLI+API+UI.

```toml
[[sources]]
id = "memory"
adapter = "ctxdsl"
files = ["memory/shared.ctxdsl"]

[sources.memory_abstraction]
kind             = "tracked_with_values"
tracked          = ["X", "Y"]
value_symbol_set = ["stale", "fresh"]
fence_semantics  = "release_acquire"
notes            = "Sound for the SC subset; RVWMO gap analysed in design/rvwmo-gap.md."
```

The block is optional — omitting it is legacy-safe and equivalent to declaring `chaotic` posture. The four validator rules enforced at parse time:

1. `kind` must be one of `chaotic`, `tracked_addresses`, `tracked_with_values`, `full_concrete`.
2. `fence_semantics`, when set, must be one of `global_barrier`, `release_acquire`, `rvwmo`.
3. Non-empty `tracked` requires `kind = tracked_addresses` or `tracked_with_values`.
4. Non-empty `value_symbol_set` requires `kind = tracked_with_values`.

`mununu memory check` (B2b, pending) extends these to property-level cross-checks: every property formula mentioning `<source>.<address>.<value>` must reference a `(<address>, <value>)` pair declared in the source's posture block.

## Soundness summary (one-line reference)

**Canonical KMTS recipe:**

- **KMTS abstraction with 3-valued mu-calculus** — uniformly sound for the full mu-calculus including liveness. `KleeneT` and `KleeneF` verdicts transfer to the concrete; `KleeneBot` requires refinement.
- **Composition** — pointwise meet on the capability lattice (per-axis conjunction); sound for both may and must without an AGR discharge step.
- **UF abstraction on wide arithmetic** — may-side-only over-approximation; must-mode queries use concrete operators.
- **CEGAR refinement** — bounded (default 16 rounds); on cap-hit, `KleeneBot` is retained with a soundness-tagged warning.

**Legacy primitives (Sharp-everywhere KMTSes):**

- **Boolean / interval / symbol-set on a variable** — sound when property doesn't distinguish values within an equivalence class.
- **Ignored variables** — sound for safety; model permits every concrete value.
- **Chaotic stub** — over-approximation; sound for safety, optimistic for liveness (Doc C §C.5 for the codesign formulation).
- **Hidden labels** — preserve safety; can lose liveness (hidden label cannot be required as a transition).
- **Bisimulation minimization** — preserves all CLTS-observable behaviour.
- **Tracked-address restriction on memory** — equivalent to ignoring untracked addresses; sound for properties that reference only tracked addresses.

## Authoring discipline

When introducing an abstraction decision — whether in an adapter, a CTXDSL source, or a sidecar — follow this checklist:

1. **Declare the abstraction posture explicitly.** For the KMTS recipe: list predicates in the sidecar (`predicates: Vec<MuFormula>`) and any UF wrapping overrides (`uf_wrap`, `uf_unwrap`). For legacy adapters: inline (`AbstractionType::Symbols(["zero", "non_zero"])`; `mem { x : shared }` in microcode) or in a comment block at the top of a hand-authored CTXDSL file.
2. **Add a `// SOUNDNESS:` annotation** at every `eval_expr → None` choice and every adapter decision that drops information. State whether it is over-approximation or under-approximation and why it is sound for the relevant property class. KMTS-aware adapters additionally annotate every `KleeneBot` fallback at the spuriousness-check and refinement-cap boundaries. CLAUDE.md § Soundness Guarantees is the enforcement point; `/soundness-check` is the audit skill.
3. **Add a regression test** for the abstraction decision when adding a new adapter or modifying the Kripke builder. The test must exercise both the abstracted case and at least one concrete case that maps into the same abstraction class, asserting the verdict agrees. For KMTS adapters, the test must also cover one fixture where the initial predicate set returns `KleeneBot` and CEGAR refinement demotes it to `KleeneT` / `KleeneF`.
4. **Document the choice in the user-facing wiki page** for the affected adapter or workflow. If the abstraction is non-obvious (e.g. "the chaotic-stub peripheral over-approximates every register access"), state the soundness consequence inline.

## What this doc deliberately does not do

- **Quantify state-space cost** per abstraction choice. That depends on the user's specific model; a follow-up benchmark suite would help, but it does not exist today. The KMTS recipe's `2^|P|` upper bound on abstract states is a worst case; reachable abstract state counts are typically much smaller.
- **Prescribe one abstraction class as "the right one"** for any subsystem. The right class depends on the property — the recipe above gives the *minimum*; safety-only properties often tolerate coarser abstractions than liveness properties.
- **Cover weak-memory-model semantics** (RVWMO, TSO, sequential consistency) **as an enforcement layer**. The [memory soundness matrix](#memory-soundness-matrix) above documents which fence semantics mununu's orchestrator enforces today (`global_barrier`, `release_acquire`) and which are aspirational (`rvwmo`). Verifying memory-order intent at the architectural level still requires either an external checker integrated as an adapter (Herd / RMEM) or a heavy abstraction (TSO / SC) that ignores weak orderings.
- **Cover 3-valued controller synthesis.** Synthesis is de-prioritised under the KMTS pivot. The synthesiser runs on the `BoolDom`-projected KMTS (Sharp-only transitions) and hard-errors when fed a KMTS with `MayOnly` transitions. Revisit if synthesis is re-prioritised; until then, the recipe is "if you want synthesis, stay on Sharp-everywhere adapters (XState, microcode, agentic, hand-written CTXDSL)."

## See also

- [Soundness Guarantees](../CLAUDE.md#soundness-guarantees) — the load-bearing rules.
- [`docs/design/native-sv-abstraction.md`](design/native-sv-abstraction.md) — the KMTS architecture, the simplification phases (§9), the validation milestones (§10).
- [`docs/design/kmts-theory.md`](design/kmts-theory.md) — KMTS definition, 3-valued mu-calculus semantics, preservation theorem.
- [`docs/design/predicate-abstraction-recipe.md`](design/predicate-abstraction-recipe.md) — predicate seeding, may/must image computation, CEGAR refinement, operational debugging.
- [`docs/design/abstraction-literature.md`](design/abstraction-literature.md) — 28-paper catalog (entries 19–24 KMTS, 25–28 AGR).
- [`docs/policies/claims-integrity.md`](policies/claims-integrity.md) — full claims-integrity policy with the abstraction-soundness procedure.
- [`docs/adapters/extraction.md`](adapters/extraction.md) — `.espec.json` extraction adapter, mode filtering, property templates.
- [`docs/synthesis.md`](synthesis.md) — `ControllerMode`, signature-based extraction, Skolem-paradigm rules (operates on Sharp-everywhere KMTSes only post-R.3).
- [`wiki/Verify-Project-Flow.md`](../wiki/Verify-Project-Flow.md) — the verify framework that consumes all of the above.
- [`wiki/Composition.md`](../wiki/Composition.md) — composition semantics including the KMTS modality merge.
