# Verify Project Flow

> **Source of truth:** [`crates/mununu-core/src/verify/`](https://github.com/Mumunu-team/mununu/tree/main/crates/mununu-core/src/verify/) — surface: CLI+API+UI.

`mununu verify` is the **general N-source verification framework**. It accepts a `verify.toml` manifest listing any combination of supported sources (C firmware, SystemVerilog RTL, hand-authored CTXDSL, XState JSON, CrewAI / LangGraph JSON, microprograms, …), translates each source through its adapter, applies an alphabet-binding strategy, composes the results, and evaluates a list of properties. Returns a structured [`VerifyReport`](#report-shape) usable from the CLI, the HTTP API, and the web UI.

The codesign C+SV flow, the protocol-spec recipe, and the agentic verification examples are all **specialisations** of this same framework.

## Conceptual model

> **Source of truth:** [`verify::config::VerifyConfig`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/config.rs) — surface: CLI+API+UI.

A verification project is the tuple `(Sources, AlphabetBinding, Composition, Properties)`:

- **Sources** — N entries, each `{ id, adapter, files, options }`. The `adapter` field names a [`FormatAdapter`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/mod.rs) implementation. Today's dispatch table: `ctxdsl`, `xstate`, `crewai`, `langgraph`, `sv-yosys` (alias `yosys`), `c-codesign`, `extraction`.

- **AlphabetBinding** — how labels across sources synchronise:
  - **`direct`** (default) — names must already match across sources.
  - **`renamings`** — explicit `{ from = "source_id.local_label", to = "canonical_label" }` map.
  - **`register_map`** — derives renamings from a register-map JSON sidecar via [`coupling::rendezvous_label_name`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/codesign/coupling.rs); SV-rtl sources get an automatic post-process pass via [`verify::register_map_rewriter::derive_sv_renamings_from_register_map`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/register_map_rewriter.rs).

- **Composition** — `{ semantics: synchronous | asynchronous | superset, members: [...], name }`. Drives the existing `composition::compose` primitive.

- **Properties** — `[{ name, (template | formula), args, over }]`. Resolved via the builtin [`TemplateRegistry`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/templates/builtin_templates.json) when a template id is given.

## Pipeline

> **Source of truth:** [`verify::orchestrator::verify_project`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/orchestrator.rs) — surface: CLI+API+UI.

```text
verify.toml --> parse + validate
              \--> for each source: dispatch_adapter
                       \--> SystemVerilog path (post-R.0; singular pipeline post-S.2b):
                                sv2v --top <top> *.sv > elaborated.v
                                yosys read_verilog ... hierarchy -check ... NO flatten
                                       submod -name <m>; write_btor <m>.btor    (per submodule)
                                btor2 → KMTS lifter (kmts_lift.rs)
                                       \--> predicate seeding from property APs + COI + typedef enums
                                       \--> may-mode predicate-image (UF-abstracted operators)
                                       \--> must-mode predicate-image (concrete operators)
                                       \--> emits Clts with TransitionModality { Sharp | MayOnly }
                                            and state_3valued_predicates : Tristate
                       \--> other adapters (XState, microcode, agentic, ctxdsl):
                                produce Sharp-everywhere KMTSes (legacy 2-valued semantics)
                       \--> adapter::partition::classify         (Phase A.3 — auto-COI; orthogonal to KMTS)
                                  \--> Dropped signals → Ignored when no sidecar override
                                  \--> AdapterOutput.partition_summary captures the counts
                       \--> AdapterOutput.ctxdsl (+ state_valuations + state_3valued_predicates side-channels)
                       \--> apply_renamings (binding-driven)
              \--> assemble_unified_ctxdsl (N source bodies into one context)
              \--> parse + realize
                       \--> environment_for(automaton) wires CLTS state_valuations
                                into Environment::abstract_states when numeric (Phase A.3)
              \--> for each property: resolve template, evaluate via mu_calculus
                       \--> EvalDomain choice: BoolDom (default; 2-valued) or KleeneDom (3-valued)
                       \--> on-demand `signal == const` atoms resolve through
                            abstract_states + state-valuation binding (Phase A.3)
                       \--> on KleeneBot verdict: CEGAR refinement loop (post-R.5; bounded at 16 rounds)
              \--> VerifyReport (per-source partition_summary; per-property 3-valued verdict + refinement trace)
```

## KMTS pipeline highlights (post-R.0 / R.1 / R.2 / R.3)

> **Source of truth:** [`docs/design/native-sv-abstraction.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/native-sv-abstraction.md) (architecture), [`docs/design/kmts-theory.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/kmts-theory.md) (theory), [`docs/design/predicate-abstraction-recipe.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/predicate-abstraction-recipe.md) (practical recipe).

The KMTS pivot replaces the SystemVerilog extraction story end-to-end while leaving non-RTL adapters (XState, microcode, agentic, ctxdsl) unchanged. The four moving parts:

- **Frontend: sv2v + Yosys-no-flatten + BTOR2-per-module.** sv2v normalises SV-2017 to a Verilog-2005 subset (preserves hierarchy and signal names); Yosys runs `read_verilog; hierarchy -check; proc; opt -fast -purge -keepdc` **without `flatten`** then emits one BTOR2 per submodule via `submod -name <m>; write_btor`. Top-module netlist drives composition.
- **KMTS data model.** Each `Transition` carries a `TransitionModality` (`Sharp` for must ∧ may; `MayOnly` for over-approximation). Each `Clts` optionally carries `state_3valued_predicates: BTreeMap<(StateId, PredId), Tristate>` where `Tristate ∈ { KleeneT, KleeneF, KleeneBot }`. Legacy adapters produce `Sharp` everywhere with `None` for the 3-valued field — vacuous KMTS, identical to today's 2-valued behaviour.
- **3-valued evaluator.** One generic evaluator body monomorphises over the bulk `EvalDomain` trait (associated `Valuation` = whole-state-set representation). `BoolDom` (BitVec) is the 2-valued cheap path; `KleeneDom` (TritSet) the 3-valued one — the truth-lattice (formula semantics) vs information-lattice (fixpoint convergence) distinction is absorbed into `TritSet`'s must/may pair. Verdicts are `KleeneT` / `KleeneF` / `KleeneBot`; `KleeneBot` triggers CEGAR. (Unified onto the single body in IR-track P2.2/P2.3; the earlier per-element `TruthDomain` trait was retired in P2.4.)
- **CEGAR refinement.** On `KleeneBot`, the lifter lifts the abstract counterexample, SMT-discharges it for spuriousness, and on UNSAT extracts predicate refinements via IC3-IA-style interpolation. Bounded by `cegar_max_rounds` (default 16). Two-axis refinement: predicate addition vs. UF instance concretisation, partitioned by unsat-core symbol kind.

**Singular-pipeline commitment.** As of S.2b the legacy native-SV adapter (the hand-rolled recursive-descent parser + explicit cross-product enumerator) is **deleted**. There is no `--engine native-sv` escape hatch. SV verification has exactly one pipeline — `sv-yosys` (sv2v → Yosys → BTOR2 → bit-blast). Sidecar significant-value discovery moved to `mununu btor2 discover` (which runs over the BTOR2 IR the verify path uses). See [`docs/design/native-sv-abstraction.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/native-sv-abstraction.md) for the architecture.

## Automatic partition (Phase A.3)

> **Source of truth:** [`adapter::partition::classify`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/partition/mod.rs) — surface: CLI+API.

SV (`sv-yosys`) and BTOR2 sources run an **automatic cone-of-influence
pass** during their `dispatch_adapter` translation. The pass walks the
frontend IR's dependency graph from property atoms and marks signals
outside the cone as `AbstractionType::Ignored`. The composition rule
with the sidecar:

- A signal explicitly listed in the `.mununu.json`'s `signals[]` /
  `inputs[]` / `predicates[]` always wins — the auto-partition never
  overrides a user declaration.
- A signal *absent* from the sidecar gets the partition's verdict.
  Most often `Kept` (in-cone); when `Dropped`, the bit-blaster pins
  the signal to a single value and emits an `AdapterWarning`.

The per-source `partition_summary` field on `VerifyReport.sources[*]`
carries the counts (kept / dropped / total) and bit-widths
(`state_bits_before` / `state_bits_after` on BTOR2) so users can read
the COI's reduction effect directly out of the report JSON.

COI is **exact** (bisimilar on the property's atomic-proposition set), not over-approximation — sound and complete for the full mu-calculus. Independent of the KMTS pivot.

See also:
[`docs/abstraction.md` §"Automatic cone-of-influence"](https://github.com/Mumunu-team/mununu/blob/main/docs/abstraction.md#automatic-cone-of-influence-phase-a3-orthogonal-to-kmts)
for the user-facing guidance.

## `verify.toml` schema

> **Source of truth:** [`verify::config::VerifyConfig`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/config.rs) — surface: CLI+API+UI.

```toml
[project]
name = "uart_codesign"
description = "UART firmware + RTL peripheral integrated verification."

[[sources]]
id = "fw"
adapter = "c-codesign"
files = ["firmware/firmware.c"]
options = {
  include_paths = ["firmware/include"],
  defines = ["F_CPU=64000000"],
  cmsis_stubs = true,
  register_map = "register_map.json",
  synthesize_automaton = true,
}

[[sources]]
id = "periph"
adapter = "sv-yosys"
files = ["rtl/uart_peripheral.sv"]

[alphabet]
strategy = "register_map"
register_map = "register_map.json"

[composition]
semantics = "asynchronous"
members = ["fw", "periph"]
name = "System"

[[properties]]
name = "no_double_start"
template = "label_blocked_in_state"
args = { STATE = "fw.Transmitting", LABEL = "wr_ctrl_tx_start" }
over = "System"

[[properties]]
name = "init_reachable"
formula = "mu X. (Init || <> X)"
over = "System"
```

### Safety-cube pass (`[project] safety_cube`)

> **Source of truth:** [`verify::config::ProjectSection::safety_cube`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/config.rs), [`verify::orchestrator::run_safety_cube_pass`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/orchestrator.rs) — surface: CLI+API+UI.

Set `[project] safety_cube = true` to additionally run the KMTS 3-valued safety cube (`AG ¬bad`) on the project's sources, reporting per-source/per-assertion verdicts in `safety_cube_results`:

- **`btor2` sources** — run [`recoverability::verify_safety_scalable`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/recoverability.rs) (enumeration + the **emergent-K** interpolation discovery of constant-bound / register-ordering invariants) on the source's `bad` obligation. These are **cube-only** (the orchestrator has no `btor2`→automaton dispatch): excluded from composition, one result (`property = None`).
- **`sv-yosys` sources** — route through [`slang::verify_auto`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/adapter/slang/verify_auto.rs), whose **slang** front-end parses each `assert property` into a cube obligation *independently of sv2v* (the sv2v flatten silently drops SVA). One result **per SVA assertion** (its name in `property`). These sources still compose normally — the cube is an additional per-assertion check. **Requires slang** (a subprocess tool not on bare hosts; validate under the `mununu-sva` image).

A best-effort pass — an unreadable file, an absent toolchain, or a design with no obligation is silently skipped.

```toml
[project]
name = "csrng_safety"
safety_cube = true

[[sources]]
id = "design"
adapter = "btor2"          # cube-only: carries a `bad` property, not composed
files = ["design.btor2"]

[composition]
semantics = "asynchronous"
members = ["design"]       # resolves to [] in the report — btor2 sources don't compose
name = "System"
```

## Report shape

> **Source of truth:** [`verify::report::VerifyReport`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/verify/report.rs) — surface: CLI+API+UI.

```rust
pub struct VerifyReport {
    pub project: String,
    pub sources: Vec<SourceSummary>,           // id, adapter, resolved automaton
    pub composition: CompositionInfo,          // semantics, name, resolved members
    pub property_verdicts: Vec<PropertyVerdict>,
    pub safety_cube_results: Vec<SafetyCubeResult>, // opt-in `[project] safety_cube`; empty otherwise
}

pub struct SafetyCubeResult {
    pub source_id: String,                     // the source the cube ran on
    pub file: String,
    pub property: Option<String>,              // SVA assertion name (sv-yosys); None for btor2
    pub verdict: String,                       // "holds" | "violated" | "unknown" | "skipped"
}

pub struct PropertyVerdict {
    pub name: String,
    pub formula_source: PropertyFormulaSource, // Inline | Template { id, args }
    pub formula: String,                       // concrete mu-calculus text
    pub over: String,
    pub verdict: KleeneVerdict,                // KleeneT | KleeneF | KleeneBot (post-R.3)
    pub satisfied: bool,                       // true iff KleeneT (preserved for 2-valued clients)
    pub total_states: usize,
    pub satisfying_states: usize,              // for KleeneT/KleeneF; not meaningful for KleeneBot
    pub bot_states: usize,                     // count of KleeneBot states (post-R.3; 0 for BoolDom)
    pub initial_states: Vec<String>,
    pub initial_satisfying: Vec<String>,
    pub refinement_trace: Option<RefinementTrace>, // populated when CEGAR ran (post-R.5)
}

pub enum KleeneVerdict { KleeneT, KleeneF, KleeneBot }
```

For `BoolDom` (legacy adapters, Sharp-everywhere KMTSes), `verdict` is always `KleeneT` or `KleeneF` and `bot_states` is always `0`. For `KleeneDom` (KMTS lifter output with `MayOnly` transitions), `KleeneBot` is possible; when CEGAR refinement closes it, `refinement_trace` is `Some(_)` with per-round predicate / UF additions.

## CLI

> **Source of truth:** [`mununu-cli::handle_verify`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-cli/src/main.rs) — surface: CLI.

```bash
mununu verify <verify.toml>                # human-readable verdict table
mununu verify <verify.toml> --json         # machine-readable VerifyReport
mununu verify <verify.toml> --strict       # exits non-zero on any violated property
```

Relative paths in the manifest resolve against the `verify.toml`'s parent directory.

## HTTP API

> **Source of truth:** [`api::handlers::verify_project_handler`](https://github.com/Mumunu-team/mununu/blob/main/crates/mununu-core/src/api/handlers.rs) — surface: API.

`POST /api/v1/verify`. Body accepts either a pre-parsed `config` JSON object **or** the raw `config_toml` string (exactly one — the handler 400s on both-set or neither-set):

```json
{
  "config_toml": "[project]\nname = \"Demo\"\n...",
  "base_dir": "/abs/path/to/project"
}
```

Returns the structured `VerifyReport`. `config_toml` lets thin clients (like the UI wizard) send the raw manifest without bundling a TOML parser.

## Web UI

> **Source of truth:** [`mununu-ui/src/components/extraction/ExtractionPanel.tsx`](https://github.com/Mumunu-team/mununu-ui/blob/main/src/components/extraction/ExtractionPanel.tsx) — surface: UI.

The Extraction tab's domain selector exposes **Verify Project (verify.toml)** as a workflow. Drop the manifest, type the server-side base directory, click **Run Verify**. The UI POSTs `{ config_toml, base_dir }` to `/api/v1/verify` and renders the response through [`VerdictTable`](https://github.com/Mumunu-team/mununu-ui/blob/main/src/components/extraction/VerdictTable.tsx).

## Examples

> **Source of truth:** [`examples/verify/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/) — surface: CLI.

Each example ships a `verify.toml`, source files, a `validate.sh` script, and a byte-deterministic `transcript.txt`:

| Example | Shape |
|---|---|
| [`xstate_pair/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/xstate_pair/) | Two XState machines composed asynchronously; direct alphabet binding |
| [`microprogram_plus_sv/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/microprogram_plus_sv/) | Microcode listing + SV peripheral via direct binding |
| [`uart_codesign_chaotic/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/uart_codesign_chaotic/) | C firmware + chaotic-stub peripheral via register-map binding |
| [`uart_codesign_protocol_spec/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/uart_codesign_protocol_spec/) | C firmware + hand-authored CTXDSL protocol-spec peripheral |
| [`crewai_handoff/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/crewai_handoff/) | Sequential 2-agent CrewAI crew |
| [`langgraph_workflow/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/langgraph_workflow/) | Conditional-branching LangGraph StateGraph |
| [`v10_mem_fabric_client_mux/`](https://github.com/Mumunu-team/mununu/tree/main/examples/verify/v10_mem_fabric_client_mux/) | Four generated variants of one RTL design decision, with a contrast pair and a mechanism control |

Reproduce any of them:

```bash
bash examples/verify/<example>/validate.sh
```

## See also

- [Agentic Adapters](Agentic-Adapters) — CrewAI + LangGraph adapter details
- [Adapter Formats](Adapter-Formats) — every adapter, its detection rules, and emitted CTXDSL shape
- [Property Templates](Property-Templates) — the template catalog used by the `template = "..."` form
- [`docs/ctxdsl-modelling-guide.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/ctxdsl-modelling-guide.md) — what binds and what silently doesn't when you hand-author a CTXDSL source: the one-comparison guard fragment, pre-state `effects`, and where `var == value` atoms bind (**they do not across a multi-source composition**)
