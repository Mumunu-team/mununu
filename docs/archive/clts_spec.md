> Status: historical — written in 2026-05 and deleted in `ce1da0c6` (PR #57, 2026-05-16); restored here on 2026-11-07 from `ce1da0c6^:docs/clts_spec.md` because module docs, READMEs and tests still cite it (roadmap D2). Not maintained since: names, paths and claims below describe the code as it was then. The live design notes are under `docs/design/`.

# CLTS Module Specification

This document specifies the **Compositional Labeled Transition System (CLTS)** module for a verification and synthesis solution. The module provides data structures and operations for representing, composing, and analyzing labeled transition systems.

---

## Rust Implementation Guidelines

The CLTS codebase must follow contemporary Rust best practices:

- Target the latest stable compiler; keep the tree `cargo fmt` and `cargo clippy -- -D warnings` clean.
- Prefer ownership-based APIs, borrowing, and iterators rather than ad-hoc pointer manipulation.
- Use `Result`/`Option` for fallible paths; avoid panics except where explicitly documented.
- Document public items with `///` comments and supply module overviews via `//!`.
- Keep `unsafe` blocks out of scope unless a reviewed justification is recorded in this spec.
- Optimisations must be validated with wall-clock timing and profiler output (e.g., `scripts/profile.sh`) to ensure measurable benefit before being accepted.

These guidelines apply to all modules, tests, and support code that contribute to the CLTS implementation.

---

## 1. State Interface

A `State` is an interface that provides access to a CLTS state instance.

### Operations

- `out_transitions()` → Returns outgoing transitions from this state
- `in_transitions()` → Returns incoming transitions to this state

### Complexity Requirements

- **O(1)** access time for both `out_transitions()` and `in_transitions()`

---

## 2. CLTS (Compositional Labeled Transition System)

A CLTS is a tuple $(S, \Sigma, \Sigma_c, \Sigma_i, T, S_0, V, \nu)$ where:

- $S$ is a finite set of states
- $\Sigma$ is the alphabet (set of atomic alphabet elements)
- $\Sigma_c \subseteq \Sigma$ is the controllable alphabet
- $\Sigma_i \subseteq \Sigma$ is the internal alphabet
- $T \subseteq S \times 2^{\Sigma} \times S$ is the transition relation, where each transition is labeled with a **set** of alphabet elements
- $S_0 \subseteq S$ is the set of initial states
- $V$ is a set of variables
- $\nu: S \to 2^V$ is the valuation function mapping states to variable sets

**Note**: A label $\ell$ on a transition is a subset of the alphabet: $\ell \subseteq \Sigma$. This allows transitions to be labeled with multiple alphabet elements simultaneously.

### Attributes

| Attribute | Type | Description |
|-----------|------|-------------|
| `state_count` | `usize` | Number of states in the CLTS |
| `transitions` | Collection | Set of all transitions |
| `states` | Collection | Set of all states |
| `alphabet` | Set | Complete alphabet $\Sigma$ |
| `controllable_alphabet` | Set | Controllable alphabet $\Sigma_c$ |
| `internal_alphabet` | Set | Internal alphabet $\Sigma_i$ |
| `initial_states` | Set | Set of initial states $S_0$ |
| `valuations` | Map | Valuation function $\nu: S \to 2^V$ |
| `variables` | Set | Set of variables $V$ |
| `controllable_variables` | Set | Subset of controllable variables |
| `internal_variables` | Set | Subset of internal variables |

### Restrictions

1. **Alphabet inclusion**: $\Sigma \subseteq \Sigma_{global}$ where $\Sigma_{global}$ is the global alphabet from the context
2. **Controllable alphabet uniqueness**: For each CLTS in a context, $\Sigma_c$ must be unique (no overlap between different CLTS instances)
3. **Internal alphabet uniqueness**: For each CLTS in a context, $\Sigma_i$ must be unique (no overlap between different CLTS instances)
4. **Initial states**: $S_0 \subseteq S$
5. **Transition states**: For all transitions $(s, \ell, s') \in T$:
   - $s \in S$ (source state)
   - $s' \in S$ (target state)
   - $\ell \subseteq \Sigma$ (label is a set of alphabet elements)
6. **Transition label access**: Accessing labels from transitions must be **O(1)** (or O(1) amortized for disk-backed storage)
7. **Transition insertion**: Adding transitions must be **O(1)** (or O(1) amortized for disk-backed storage)
8. **Valuation mapping**: $\nu$ maps each state to a set of variables: $\nu(s) \subseteq V$ for all $s \in S$

### Per-automaton controllability/internal (DSL)

- Automata may declare `controllable { label ...; }` and `internal { label ...; }` blocks.
- Ownership is exclusive across automata: the same label cannot be declared controllable or internal by more than one automaton.
- When these blocks are present, transitions are controllable if any of their labels is in the automaton’s controllable set; otherwise they are treated as uncontrollable. If blocks are absent, legacy inference applies (epsilon or input-signal labels/guards ⇒ uncontrollable; else controllable).
9. **Variable access**: Retrieving variables from a state must be **O(1)** (or O(1) amortized for disk-backed storage)
10. **Minimization**: The `minimize()` operation should minimize the CLTS (e.g., via bisimulation reduction)
11. **Disk-backed storage**: For large structures, transitions and CLTSs can be stored on disk while maintaining O(1) amortized access through memory-mapped files, lazy loading, and caching strategies
12. **Label handles**: `LabelHandle::Shared(id)` must reference an entry in `labels.payloads`; `LabelHandle::Inline` is only valid for alphabet sizes that fit within `SmallLabel`
13. **Struct-of-arrays consistency**: `transition_from` and `transition_to` caches must stay aligned with `transitions` (same length and ordering)

### Representation Guidelines

- **Labels and variables**: Prefer representation as bitvectors
- **Small label paths**: When labels touch only a handful of alphabet elements, pack them into `SmallLabel` for inline storage; fallback to shared bitvectors for dense labels
- **Deduplication**: Reuse label bitvectors across transitions via `LabelStore` to minimize memory footprint and improve cache locality
- **Fallback**: If bitvectors are not feasible, use arrays of bitvectors or other compact representations
- **Goal**: Optimize for memory efficiency and fast access
- **Identifier widths**: `StateId` and `LabelId` storage types (`u16`, `u32`, `u64`, `usize`, …) are selectable per build profile so deployments can trade identifier range for memory footprint. See [`docs/identifier_width_customization.md`](../identifier_width_customization.md) for detailed guidance on customizing identifier types.
- **Context-aware building**: `CltsBuilder::with_label_store` allows a context to seed builders with a shared `LabelStoreBuilder`, ensuring multiple CLTS instances intern alphabet elements consistently when desired. Standalone builders remain available for focused unit tests or self-contained tools.
- **Immutability**: `Clts` instances are immutable; algorithms that minimise or mutate structures should clone/rehydrate a builder (`CltsBuilder::from_clts` in a future iteration) to regenerate adjacency caches atomically. This keeps `incoming`/`outgoing` tables and name maps consistent without incremental update logic.
- **Canonical label interning**: `LabelStoreBuilder::intern` must treat alphabet sets as order-insensitive and deduplicate equivalent payloads across builders (especially when contexts reuse a store). Tests cover repeated interning to enforce this guarantee.
- **Builder growth & handles**: `CltsBuilder` pre-reserves 256 states and 512 transitions, then grows each collection by ~20 % increments to amortise allocations. Prefer the handle-oriented helpers (`state_with_name`, `state_id_or_insert`, `initial_state_id`, `with_variables_for_state`, `transition_ids`) when constructing large fixtures so repeated hash table lookups and string clones are avoided.
- **Reusable buffers**: `LabelStoreBuilder::intern_in_place` and `VariableStoreBuilder::intern_in_place` accept caller-owned `Vec<String>` buffers, sort/deduplicate them in place, and clear the buffers for reuse. The builders also draw staging buffers from a shared `StringVecPool`, so even the higher-level `intern` helpers reuse previously allocated `Vec<String>` storage. Heavy builders and benchmarks should prefer these APIs to keep allocator churn low.
- **Two builder strategies**: `Clts::builder()` is ideal for standalone fixtures or white-box tests where a self-contained store keeps the setup minimal. When constructing CLTSs that will live inside a context, call `Context::new_clts_builder()` so the shared label store maintains handle alignment.
- **Struct-of-arrays alignment**: `outgoing` and `incoming` caches must remain aligned with the transition list; builders should assert (at least in debug builds) that no transition goes missing during construction.
- **Context registry behaviour**: Contexts manage a shared `LabelStoreBuilder` and ensure each registered CLTS uses a canonical alphabet universe. `ContextBuilder::finish_with_checks` merges every registered CLTS's label payloads back into the shared builder so subsequent `Context::new_clts_builder` calls reuse the same handles. Registration must still detect duplicate names and prepare future enforcement of alphabet uniqueness constraints.
- **Variable store**: State valuations use a canonical `VariableStore` that deduplicates variable sets and exposes per-state lookups. Each state may reference zero or more variables; the store treats valuations as sets (order-insensitive, duplicates removed). Contexts merge each CLTS's variable universe into a global registry during registration.
- **Bitset payloads**: Label and variable stores should expose canonical bitset views (e.g., `bitvec`) so composition/verification can perform inclusion/intersection checks efficiently without re-encoding string payloads.
- **State-set pooling**: `Clts::state_set()` borrows a pooled `StateSet` (backed by `bitvec`) sized to the instance's state space. Dropping the set returns the buffer to an internal arena so μ-calculus fixpoints and reachability sweeps can recycle scratch memory instead of allocating new vectors each iteration.
- **Controllability metadata**: Transitions expose a `TransitionKind` flag (controllable or uncontrollable). CLTS instances group successors by controllability and construct bitset views on demand so large systems avoid dense per-state caches. These APIs should power supervisory control and μ-calculus next-state checks.
- **Guard partitions & memoisation**: `mu_calculus::evaluate_with_options` derives a `GuardPartitions` cache for each distinct modal guard (bitsets for required/forbidden variables on current and successor states) and memoises sub-formula results when no fixpoint bindings are active. Both optimisations are toggled via `EvaluationOptions` for profiling or diagnostic comparisons.

---

## 3. Transition

A transition represents an edge in the CLTS: $(s, \ell, s') \in T$ where $\ell \subseteq \Sigma$ is a set of alphabet elements.

### Attributes

| Attribute | Type | Description |
|-----------|------|-------------|
| `clts` | Reference | Reference to the parent CLTS |
| `from_state` | State | Source state $s$ |
| `to_state` | State | Target state $s'$ |
| `labels` | Set | Set of alphabet elements $\ell \subseteq \Sigma$ labeling this transition |

### Restrictions

**Mutual exclusivity of internal labels**:  
For a transition with label set $\ell \subseteq \Sigma$:
$$\ell \cap \Sigma_i \neq \emptyset \iff \ell \cap (\Sigma \setminus \Sigma_i) = \emptyset$$

In other words, if a label contains any internal alphabet element, it cannot contain any non-internal alphabet element, and vice versa.

---

## 4. Formula

A formula represents a temporal logic property (e.g., μ-calculus) that can be evaluated over CLTS structures.

### Attributes

None specified (formula structure depends on the specific logic being used).

### Restrictions

- Must be a **μ-calculus formula** (or compatible temporal logic)
- Can be applied over:
  - **States**: State-based properties
  - **Labels**: Label-based properties  
  - **Variables**: Variable-based properties
- Modal guards accept label/variable/controllability filters plus an optional bounded-depth clause of the form `steps <= N`. The evaluator honours this limit by exploring the transition system up to `N` edges, which the translation pipeline uses to encode bounded-response SVA patterns (e.g., `req |-> ##[1:3] ack` → `< ( steps <= 3 ) > ack`).

---

## 5. Context

A `Context` manages multiple CLTS instances and provides composition and verification operations.

`ContextBuilder::finish_with_checks` is the production path: it merges every registered CLTS's canonical label payloads back into the shared store and enforces controllable-alphabet uniqueness. `ContextBuilder::finish` exists for lightweight scaffolding but skips these guarantees; use it only in targeted tests. Any overflow or interning failure during the merge surfaces as `ContextError::LabelRegistry`.

Contexts expose a lightweight wrapper, `Context::compose_named(left, right, options)`, for invoking the global `composition::compose` helper on registered systems. The method returns a standalone CLTS that callers may register or analyse further.

### Controller Extraction from Model Checking

- When a μ-calculus (or compatible) formula evaluates to a non-empty satisfying state set over an automaton, the tool must be able to synthesise a controller by restricting the original automaton to only those states.
- Retain the original initial states that satisfy the formula and perform a reachability pass to drop unreachable states and their incident transitions.
- Remove transitions whose source or target lies outside the retained set; preserve variable valuations for surviving states unchanged.
- If the satisfying set is empty (no reachable state satisfies the formula), emit an **empty controller** and inject a comment such as `// controller <name> unrealizable` in the exported artefact.
- Controllers are named artefacts: the workflow must accept a user-provided controller identifier and an optional export path (defaults to stdout). Exported controllers reuse the same context syntax (see `docs/clts_context_syntax_proposal.md`).
- Exporting a controller produces a context document that extends the original automata catalogue with the new controller definition; compositions and formulae may reference the controller in subsequent runs.
- Diagnostic roadmap:
  - Unrealizable results should return a minimal counterstrategy plus at least one canonical counterexample trace per violating initial state. Large losing regions must use heuristics (ranking/pruning) to keep artefacts compact.
  - Realizable controllers should also emit diagnostics metadata (verdict, initial coverage, references to artefacts) and provide sidecar DSL/structured logs/CLI summaries when enabled. The default export remains lean; diagnostics are opt-in.
  - Counterstrategies, traces, and deadlock diagnostics are emitted as sidecar DSL files when enabled; the incremental loader will mark them dirty when dependent automata/formulae change.
  - Deadlock tracing runs only when explicitly requested (default `false`) through API or DSL configuration.
  - Diagnostics options include counterexample/counterstrategy generation and deadlock trace discovery. When disabled they carry zero overhead.
  - Controller synthesis attaches structured diagnostics: success/failure messages, initial coverage, minimal counterexample trace, minimal counterstrategy traces, deadlock traces when requested, and prototype counterstrategies (`ControllerDiagnostics::counterstrategy`) describing the losing-region graph.
  - `ControllerDiagnostics` exposes helper APIs for consumers:
    - `summary()` returns a human-readable overview.
    - `to_json_string_pretty()` / `write_json()` serialize diagnostics as JSON.
    - `write_sidecar_dsl()` exports a DSL sidecar (mirroring `diagnostics { ... }` format) suitable for checked-in artefacts.
  - `ControllerSynthesisOptions::minimize` runs a partition-refinement pass to merge structurally equivalent controller states. When enabled, `ControllerDiagnostics::minimization` reports the number of states/transitions removed and the merged-state list.
  - Unrealizable paths populate `ControllerDiagnostics::proof_obligations`, listing violating initial states (plus optional detail strings); they are serialized in JSON and DSL sidecars to guide remediation.

### Verification / Model Checking

With canonical ordering in place, structural equality and hashing become trivial: contexts can serialise or hash CLTS snapshots by comparing bitsets directly (state adjacency, label/variable stores) without converting back to strings. This enables fast equivalence checks or memoisation keyed by CLTS structure.

A context may orchestrate fixpoint computations across multiple CLTSs: e.g. compose a plant and controller, then run a μ-calculus evaluator against the resulting closed-loop product and a monitoring CLTS. Shared ordering and structural hashing simplify caching/memoisation in such scenarios.

An optional formula optimisation layer may normalise μ-calculus formulas before evaluation (e.g. remove tautologies, merge guards, collapse vacuous fixpoints). Adoption is contingent on profiling evidence showing faster evaluation.

The context DSL ships with an incremental loader (`context_dsl::loader`) that canonicalises parsed documents, fingerprints automata/compositions/formulae/controllers, and propagates change notifications through their dependency graph. Consumers can cache the resulting fingerprints via `ContextDslCache`, diff new documents against the cache, and only rebuild artefacts whose fingerprints changed (with dependency-induced rebuilds for compositions, controllers, and target formulas). The cache exposes `diff`, `update`, and `diff_and_update` helpers so build systems can interleave parsing, planning, and execution while keeping future runs incremental.

`Context::evaluate_mu(name, formula, env, options)` provides the integration point between registry and evaluator. It looks up the registered CLTS (erroring with `ContextError::UnknownClts` when missing), verifies that the supplied μ-calculus `Environment` matches the CLTS state count (`ContextError::EnvironmentMismatch` otherwise), and then delegates to `evaluate_with_options`. Callers can forward bespoke `EvaluationOptions`; omitting them applies the default memoisation + guard partitions configuration. The method returns the raw `EvalResult` bitset so higher-level helpers may post-process satisfying states, derive counterexamples, or feed subsequent analyses. For batch scenarios, `Context::evaluate_mu_many(names, formula, make_env, options)` iterates over multiple registered CLTSs, using the provided closure to build each environment before invoking the same validation/evaluation flow; the helper returns a map of results keyed by CLTS name.

The context DSL ships with an incremental loader (`context_dsl::loader`) that canonicalises parsed documents, fingerprints automata/compositions/formulae/controllers, and propagates change notifications through their dependency graph. Consumers can cache the resulting fingerprints via `ContextDslCache`, diff new documents against the cache, and only rebuild artefacts whose fingerprints changed (with dependency-induced rebuilds for compositions, controllers, and target formulas). The cache exposes `diff`, `update`, and `diff_and_update` helpers so build systems can interleave parsing, planning, and execution while keeping future runs incremental.

`EvaluationOptions` exposes runtime switches (`use_partitions`, `use_memoisation`) so profiling and regression suites can compare the guard-partition/memoised evaluator with the baseline behaviour. CI keeps the optimisations enabled by default; disable them only when hunting regressions or collecting A/B metrics.

## Persistence

- Module: `persistence`
- `save_clts_to_path(clts, path)` serialises a `Clts<DefaultStateIdx, DefaultLabelIdx>` snapshot in the binary `CLTSBIN` format: fixed magic header, little-endian 32-bit fields, a deduplicated string table (state names, labels, variables), followed by state/transition records expressed as indices into that table. From version 2 onwards the snapshot also includes label controllability metadata (controllable/uncontrollable/internal) appended after the transition section so older readers can still locate transition segments.
- `load_clts_from_path(path)` validates the header/version (currently supports `CLTSBIN` versions 1 and 2), rebuilds the string table, then replays state/transition entries through `CltsBuilder`, re-interning labels and variable sets. When loading version 2 snapshots, label controllability is restored before finalising the CLTS, so controllable, uncontrollable, and internal alphabets (and derived caches) round-trip exactly. Malformed inputs surface as `PersistenceError::InvalidSnapshot`.
- `maybe_spill_clts(clts, limit_bytes, path)` serialises to an in-memory buffer first; when the payload exceeds `limit_bytes` the snapshot is written to disk and the byte count returned. `Context::spill_clts_if_exceeds` leverages this to proactively offload large CLTSs.
- `Context::save_clts_to_path` / `Context::load_clts_from_path` wrap the per-system helpers so registered systems (and compositions) can be stored/rehydrated. `ContextBuilder::register_clts_from_path` and `ContextBuilder::register_clts` can be mixed to bootstrap registries from on-disk archives.
- `save_context_to_path(context, path)` / `load_context_from_path(path)` serialise and restore a full `Context` snapshot in the binary `CTXBIN` format: a small header, the number of registered CLTSs, and an embedded `CLTSBIN` payload for each named automaton. `Context::save_to_path` / `Context::load_from_path` provide the high-level API, rebuilding the registry, shared label store, controllable alphabet, and global variables via `ContextBuilder::finish_with_checks`.
- Alignment & IO considerations: contiguous little-endian records keep the formats CPU-friendly, while the shared string table avoids repeating symbol payloads per transition. Larger deployments should still profile spill/load throughput before committing snapshots to long-term storage.

**LTL Support:** The Context DSL now supports LTL (Linear Temporal Logic) formulas alongside μ-calculus. LTL formulas are written using the `ltl` keyword and are automatically translated to μ-calculus during realization. See [LTL Tutorial](ltl_tutorial.md) for syntax and examples. The translation follows standard LTL-to-μ-calculus patterns (e.g., `G φ` → `ν X. (φ ∧ [] X)`, `F φ` → `μ X. (φ ∨ [] X)`) and has been validated for semantic equivalence.

The translation module provides BPM translation pipelines that convert BPMN specifications into CLTS DSL contexts.

### Guard Metadata Contract

Translators may emit structured guard metadata alongside arithmetic/property
sidecars. During realisation, `RealizedContext` stores this information via
`predicate_metadata`, exposing guard expressions, normalised comparisons, and
effect summaries to downstream consumers. Specifications relying on guard
metadata should treat the JSON payload as part of the public contract:

- Each predicate records its guard body (`guard`), originating transition
  (`transition`), and arithmetic analysis (`expr`).
- Optional `effects` arrays capture assignments recovered from BPM branches.
- Tooling (e.g., integration tests, interactive composition CLI, diagnostics
  exports) can query `RealizedContext::predicate_metadata(automaton, predicate)`
  to retrieve the payload.

Auditing guard metadata consumers ensures new features (composition, services)
build on the documented structure without reverse engineering translator output.

For context-built CLTS instances, the module exposes `Clts::structural_eq` / `Clts::structural_hash`, which compare/hashes structure based on canonical label/variable bit ordering. Use these for memoisation, caching, or deduplication; do not rely on the raw `Eq`/`Hash` derived from internal maps.

### Attributes

| Attribute | Type | Description |
|-----------|------|-------------|
| `global_alphabet` | Set | Global alphabet $\Sigma_{global}$ that contains all CLTS alphabets |
| `global_variables` | Set | Global set of variables |
| `cltss` | Collection | Set of CLTS instances managed by this context |
| `formulae` | Collection | Set of formulae to verify/synthesize |
| `alphabet_by_name` | Map | String → alphabet element mapping |
| `alphabet_names` | Map | Alphabet element → string mapping |
| `variables_by_name` | Map | String → variable identifier mapping |
| `variable_names` | Map | Variable identifier → string mapping |
| `clts_by_name` | Map | String → CLTS identifier mapping |
| `clts_names` | Map | CLTS identifier → string mapping |
| `formulae_by_name` | Map | String → formula identifier mapping |
| `formula_names` | Map | Formula identifier → string mapping |
| `parallel_runtime` | Optional | Shared thread-pool configuration for parallel operations |

### Operations

#### `compose_clts(clts₁, clts₂, semantics, destroy)`

Composes two CLTS instances according to specified semantics.

The semantics align with the classical discrete-event handshake product and interleaving schemes used in DES/reactive-system literature. Synchronous composition resembles the shared-event product in Ramadge–Wonham supervisory control (shared labels must fire together), while asynchronous composition mirrors Mazurkiewicz trace interleavings. For example, if automaton A exposes a transition `s₁ ─{req}→ s₁′` and automaton B has `s₂ ─{req, ack}→ s₂′`, the synchronous product emits a single composed transition labelled `{req, ack}` because both contain the shared `req`. Conversely, if A has `s₁ ─{produce}→ s₁′` and B has `s₂ ─{consume}→ s₂′` with no shared alphabet, the asynchronous semantics produce both orderings—A then B and B then A—reflecting independent progress, while the synchronous semantics emit just the joint transition with label `{produce, consume}`.

The algorithm explores the product graph from the paired initial states only, ensuring unreachable `(s₁, s₂)` combinations are never materialised. This keeps the composed CLTS minimal and avoids interning redundant labels or variable valuations.

**Parameters:**
- `clts₁`, `clts₂`: CLTS instances to compose
- `semantics`: Composition semantics (one of):
  - `synchronous`: Synchronous composition
  - `asynchronous`: Asynchronous composition
  - `superset`: Superset composition
- `destroy`: Boolean flag indicating whether to destroy input CLTSs after composition

**Returns:** Composed CLTS

**Parallel execution:** When `parallel_runtime` is present, state-pair exploration and synchronization can be partitioned across threads using work-stealing. Each thread operates on disjoint slices of `transition_segments` to maximize locality.

##### Composition Semantics

- **Shared alphabet alignment**: A composed transition exists only if every alphabet element shared between both automata is present in the respective source transitions. The same requirement applies to variables referenced by the transitions (both synchronous and asynchronous semantics).
- **Shared label union**: When transitions share alphabet elements, emit a single transition in the result whose label set is the union of both source labels (applies to both synchronous and asynchronous semantics).
- **No shared elements — synchronous**: If the two source transitions share no alphabet elements, the synchronous composition still emits a single transition labeled with the union of both label sets.
- **No shared elements — asynchronous**: When the transitions share no alphabet elements, the asynchronous composition emits both permutations (i.e., each source transition interleaved with the other), capturing independent progress of each automaton.
- **Superset semantics**: Apply the same shared-element checks for labels and variables. When transitions share alphabet elements, emit their union once. If no elements overlap, emit the union plus both permutations so the composed system can observe either ordering before or after the unioned step.
- **Reachability only**: Implementations must explore the product graph starting from the Cartesian product of initial states (BFS/DFS) and only materialise reachable composed states/transitions. Unreachable pairs must not appear in the result.
- **Variable valuation**: The variable set of the composed target state is the union of the variables associated with the respective target states of the original transitions.

Formally, let transitions be denoted as

```
(s₁, ℓ₁, s₁′) ∈ T₁
(s₂, ℓ₂, s₂′) ∈ T₂
```

with `Vars(s)` the valuation associated with state `s`. The composed transition set `T⊗` contains `( (s₁, s₂), ℓ, (s₁′, s₂′) )` whenever the following hold:

1. **Shared-label agreement**:
   - If `ℓ₁ ∩ ℓ₂ ≠ ∅`, then `(ℓ₁ ∩ ℓ₂) ⊆ ℓ₁` and `(ℓ₁ ∩ ℓ₂) ⊆ ℓ₂` (always true by construction) and the composed label is `ℓ = ℓ₁ ∪ ℓ₂`.
2. **Variables**: `Vars(s₁′) ∩ Vars(s₂′) ⊆ Vars(s₁′)` and `⊆ Vars(s₂′)` (again, tautological) and the composed valuation is `Vars((s₁′, s₂′)) = Vars(s₁′) ∪ Vars(s₂′)`.
3. **Synchronous case (`⊗syn`)**
   - If `ℓ₁ ∩ ℓ₂ ≠ ∅`, include the single transition with `ℓ = ℓ₁ ∪ ℓ₂`.
   - If `ℓ₁ ∩ ℓ₂ = ∅`, include the single transition with `ℓ = ℓ₁ ∪ ℓ₂`.
4. **Asynchronous case (`⊗async`)**
   - If `ℓ₁ ∩ ℓ₂ ≠ ∅`, include the single transition with `ℓ = ℓ₁ ∪ ℓ₂`.
   - If `ℓ₁ ∩ ℓ₂ = ∅`, include both permutations: `( (s₁, s₂), ℓ₁, (s₁′, s₂) )` and `( (s₁, s₂), ℓ₂, (s₁, s₂′) )`.
5. **Superset case (`⊗sup`)**
   - If `ℓ₁ ∩ ℓ₂ ≠ ∅`, include the single transition with `ℓ = ℓ₁ ∪ ℓ₂`.
   - If `ℓ₁ ∩ ℓ₂ = ∅`, include both permutations: `( (s₁, s₂), ℓ₁, (s₁′, s₂) )` and `( (s₁, s₂), ℓ₂, (s₁, s₂′) )`, as well as the union transition `( (s₁, s₂), ℓ₁ ∪ ℓ₂, (s₁′, s₂′) )`.

#### `verify(formulae, cltss_opt)`

Performs model checking on a set of formulae.

**Parameters:**
- `formulae`: Set of formulae to verify
- `