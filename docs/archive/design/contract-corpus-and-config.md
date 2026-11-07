> Status: historical — written in 2026-05 and deleted in `ce1da0c6` (PR #57, 2026-05-16); restored here on 2026-11-07 from `ce1da0c6^:docs/design/contract-corpus-and-config.md` because module docs, READMEs and tests still cite it (roadmap D2). Not maintained since: names, paths and claims below describe the code as it was then. The live design notes are under `docs/design/`.

# Contract corpus and unified configuration

> **Status:** Design-stage reasoning. Not shipped architecture.
> **Audience:** Adapter authors and tool integrators who need a single home for "how contracts are written down" and "where mununu's project config lives."
> **Companion documents:** [A — Black-box modules in compositional extraction](black-box-modules.md), [B — RTL frontend unification](rtl-frontend-unification.md), [C — HW/SW codesign extraction](hw-sw-codesign-extraction.md) (design landed; implementation deferred).

## D.1 Why one document covers both

The corpus is what *populates* contracts in the §A2 discovery pipeline. The sidecar is what *references* corpus entries from a user's project. The annotations are what *attach* contract clauses to source files. All three share a schema and would drift if split. Document D is the single source of truth.

It also picks up two tasks the conceptual framework relocated here from Document A: **A6** (corpus-driven discovery phase 2) and **A7** (HITL stage-4 UX). Both depend on the corpus + sidecar deliverables that this document defines, so they belong in the same milestone (M3).

## D.2 The contract corpus

**Goal.** Replace the chaotic stub with a specific, vetted contract whenever one exists for the module / library / IP at hand.

### D.2.1 The query shape

A single canonical query:

```
contract_query(domain, module_name, parameters) -> [candidate_contract, ...]
```

Examples:
- `("rtl_protocol", "axi4_slave", {addr_width: 32, data_width: 64, has_qos: false})`
- `("rtl_memory", "uart_lite", {data_bits: 8, parity: "none", baud_default: 115200})`
- `("software_library", "lodash.debounce", {leading: true})`
- `("software_protocol", "mcp_server", {tools: ["list_directory", "read_file"]})`

The query returns a *ranked* list. The HITL stage 4 from Document A §3 surfaces candidates so the user can pick.

### D.2.2 Deployment tiers

| Phase | Backend | When it's worth graduating |
|---|---|---|
| **Phase 1 — file-backed** | A directory `corpus/` shipped with mununu (or a separate `mununu-corpus` repo). Each contract is a single file indexed by `domain/name@version`. | Always start here. Simple, no service to run. |
| **Phase 2 — local DB** | SQLite (or sled) with full-text search over descriptions + parameter-match index. Still file-shippable. | When the corpus exceeds a few hundred entries and parameter-match lookups get slow on Phase 1's linear scan. |
| **Phase 3 — remote service** | Hosted index at e.g. `contracts.mununu.dev` with HTTPS lookup, content-addressed entries (hash-pinned for reproducibility), and a community contribution path. Optional licensed tier for vendor IP libraries. | When the corpus needs centralised curation, version pinning across many users, or paid vendor contracts. |

The doc explicitly recommends **starting at Phase 1**. The format and query semantics must stabilise before any backend complexity arrives.

### D.2.3 Schema (illustrative)

Each contract entry carries (illustrative — see §D.7 for open questions about the exact field set):

```json
{
  "id": "rtl_protocol/axi4_slave",
  "version": "2.0.1",
  "domain": "rtl_protocol",
  "name": "axi4_slave",
  "parameters_schema": { /* JSON Schema for the parameter object */ },
  "interface": {
    "labels": [...],
    "controllability": {...},
    "automaton": { "states": [...], "transitions": [...] },
    "formulas": [
      { "role": "guarantee", "formula": "...", "soundness": "safety" }
    ]
  },
  "alternatives": [
    { "id": "...", "label": "strict",     "description": "Strict ordering" },
    { "id": "...", "label": "permissive", "description": "Allows out-of-order writes" }
  ],
  "provenance": {
    "origin": "community" | "vendor:<name>" | "mununu-verified",
    "verified_against": "ARM AMBA AXI4 spec rev I",
    "contributors": ["..."],
    "license": "CC-BY-4.0"
  },
  "soundness_flag": "safety" | "safety+liveness" | "unsound-without-fairness"
}
```

### D.2.4 Alternatives are first-class

Real-world IPs have multiple valid contracts depending on configuration and interpretation. A single corpus entry exposes named alternatives ("strict" vs "permissive," "Mealy" vs "Moore," "fairness assumed" vs "fairness required"). The user picks an alternative; that selection is recorded in the project config and is part of the verifier's audit trail.

### D.2.5 Ranking candidates

When multiple entries match a query, the result list is ranked by:

1. Parameter-match exactness (full parameter match wins over partial).
2. User preference (a project-local config can pin `prefer: ["strict", "vendor:arm"]`).
3. Provenance trust tier (`mununu-verified` > `vendor:<known>` > `community`).
4. Recency / version maturity.

Single-candidate auto-pick is allowed in `--non-interactive` mode but always logged.

### D.2.6 Worked example

Vendor "Atomic Synthesis Co." ships an AXI4-slave IP. Their wrapper carries the annotation `@mununu_interface = "contract://rtl_protocol/axi4_slave@2.0.1?alt=strict"` (see §D.5). The discovery pipeline issues:

```
contract_query("rtl_protocol", "axi4_slave", {addr_width: 32, data_width: 64, ...vendor-declared params...})
```

The corpus returns the entry; alternative `strict` is selected per the URI. The chaotic stub from Document A §2 is replaced with the vetted automaton + formulas. The verifier output reports `contract: rtl_protocol/axi4_slave@2.0.1 (strict, provenance: vendor:arm)` so the audit trail shows exactly which contract was discharged against.

### D.2.7 Simplification opportunity

**Phase 1 (file-backed) is enough to demonstrate the entire workflow.** It eliminates the "build a service" question and lets the contract format + query semantics stabilise before any backend complexity arrives. Start there. Phase 2 / 3 only when corpus size or curation needs genuinely demand them.

## D.3 Unified configuration / sidecar story

> **Post-M3 reassessment (2026-05-13).** Most of this section's pitch did not survive contact with reality. Shipping M1 + M2 + M3 produced a different pattern than the unified directory the original D.3 sketch proposed:
>
> - **Co-located sidecars won.** `BlackBoxInterface.json` + `GapMarkerReport.json` are auto-emitted next to the source they describe. `.contract.todo.json` writes next to source. Co-location proved more discoverable than a centralised `.mununu/` tree.
> - **Annotation-driven contract references replaced `contracts/refs.json`.** Each black-box module's `@mununu_interface contract://…` annotation IS the reference. The discovery pipeline reads it directly from `BlackBoxInterface.annotations`. There is no second-level "refs file."
> - **Corpus already lives at repo root**, not under `.mununu/`. `corpus/rtl_protocol/...` and `corpus/rtl_crypto/...` shipped in M3. The flat layout works.
>
> The remaining pieces of D.3 that still hold up:
>
> - The **`coupling/register_maps/`** subdirectory is still the right home for Doc C's register-map sidecars. The capstone needs *some* per-project location for these, and the rest of the tree being descoped does not change that.
> - The principle "files stay small and focused" survives; it's just been realised through per-source-file co-location rather than through a per-concern central directory.
>
> The sections below are kept as the original design record. Treat §D.3.2's full directory tree as **superseded** except for the `coupling/register_maps/` row. Treat §D.3.4's migration story as unnecessary — there is nothing to migrate. The "alphabet soup" criticism in §D.3.1 still describes the legacy formats, but the unification *happened differently* than predicted.

### D.3.1 Current state

Mununu has accumulated several config artefact shapes, each evolved for a specific adapter:

| Artefact | Purpose | Today |
|---|---|---|
| `.espec.json` | Extraction spec (software AST → CTXDSL) | Schema in [`tools/extraction_spec_schema.json`](../../tools/extraction_spec_schema.json) (in the private repo); large file format |
| `.mununu.json` | RTL sidecar (multi-module, controllability hints, discovered values) | Adapter-specific; format slightly different per pipeline |
| `.extract.json` | Tree-sitter extraction config | Software-side only |
| Embedded annotations (`// @mununu ...`, `(* mununu *)`, JSDoc) | In-source metadata | Partial across SV; nothing systematic for C / TS yet |
| Domain profiles | Controllability heuristics | Built into Rust source ([domain.rs](../../crates/mununu-core/src/adapter/extraction/ast_extract/domain.rs)) |
| Property templates | Reusable formulas | Built-in JSON ([builtin_templates.json](../../crates/mununu-core/src/adapter/templates/builtin_templates.json)) |
| Sidecar register maps | Codesign coupling (Document C) | Doesn't exist yet |
| `.contract.todo.json` | Gap markers from §A2.iii | Shipped in M1 |
| `BlackBoxInterface.json` / `GapMarkerReport.json` | Auto-emitted by adapters | Shipped in M2 |
| `blackbox_modules` field in `.mununu.json` | Custom-SV black-box hint | Shipped in M2 |

The user sees an alphabet soup. Each adapter has its own dialect. Cross-adapter projects (RTL + firmware) juggle multiple file formats and tool invocations.

### D.3.2 Proposed unification — `.mununu/` project directory

Replace the alphabet soup with a single project-rooted directory, organised by *concern* rather than by *adapter*:

```
<project-root>/
└── .mununu/
    ├── project.json              # top-level metadata (name, version, domains)
    ├── extraction/
    │   ├── targets.json          # what to extract (SV, C, TS, peripherals)
    │   └── domain_overrides.json # per-target controllability + abstraction
    ├── contracts/
    │   ├── refs.json             # references to corpus entries (id, version, alt, params)
    │   ├── local/                # locally-authored contracts (full schema as in §D.2)
    │   └── todo/                 # gap-marker stubs from §A2.iii, awaiting fill-in
    ├── coupling/
    │   └── register_maps/        # Document C register-map sidecars
    └── properties/
        ├── global.json           # global formulas / templates
        └── per_module/           # module-scoped properties
```

### D.3.3 Why this shape

- **One root, one mental model.** New contributors learn `.mununu/` and explore from there.
- **Each subdir corresponds to a concept the user already has** — extraction, contracts, properties, coupling — not to an adapter.
- **Files stay small and focused.** No monster `.mununu.json` doing six things.
- **Adapter pipelines all read the same tree**, just consume their relevant subdirs.

### D.3.4 Migration story

Backwards compatible at first:

- Existing `.espec.json` / `.mununu.json` / `.extract.json` continue working, treated as legacy single-file configs that map onto the unified shape via a thin adapter.
- A migration command `mununu config migrate` converts a legacy layout to `.mununu/`.
- After two minor versions, legacy formats emit a deprecation warning; eventually they become read-only.

### D.3.5 Where Doc D meets Doc A

The `contracts/` subdir is the meeting point:

- **Corpus references** go into `contracts/refs.json`: `{ id, version, alternative, params }` per black-box module.
- **Locally-authored contracts** go into `contracts/local/` as full JSON files matching the schema in §D.2.3.
- **Auto-emitted gap markers** land in `contracts/todo/` (the `.contract.todo.json` files shipped in M1).

The discovery pipeline reads all three, queries the corpus for any unmatched-but-referenced ID, and produces a unified contract object per black-box.

### D.3.6 Worked example

A small project mixing SV peripheral + TS firmware (an emulator stack) with one closed-IP vendor module:

```
my-uart-stack/
├── rtl/uart_top.sv
├── firmware/uart_driver.ts
└── .mununu/
    ├── project.json
    ├── extraction/targets.json
    ├── contracts/refs.json
    ├── contracts/local/ctrl_arbitration.json
    ├── coupling/register_maps/uart_regs.json
    └── properties/global.json
```

The user runs `mununu verify` from the project root; mununu reads the unified config, runs extraction + composition + verification end-to-end.

### D.3.7 Simplification opportunity

The directory schema can land *before* full migration: ship `.mununu/` as the recommended layout for new projects, document the legacy mapping for old projects, and let migration happen organically. The big win — one mental model for users — is achieved on day one, even if existing projects continue using single-file configs.

## D.5 Source-comment annotation grammar

Owned by D so the corpus, the sidecar, and the in-source annotations share one vocabulary. Referenced from [Document A §2.vi](black-box-modules.md#2vi-source-comment-metadata-conceptual-hook) (conceptual rule) and from Document C §C.4 (register-declaration annotations).

Partially present today (`// @mununu controllable sig` in SV per [adapter/systemverilog/annotation.rs](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs)). This section makes it systematic across languages.

### D.5.1 Canonical tag table

| Tag | Meaning | Cross-doc reference |
|---|---|---|
| `@mununu_blackbox` | Declare module/function/class as black box; suppresses body parsing | A §2.i |
| `@mununu_assume <formula-or-template>` | Environment assumption at this boundary | A §3 stage 4 |
| `@mununu_guarantee <formula-or-template>` | Guarantee the module provides | A §3 stage 5 |
| `@mununu_interface <contract-uri>` | Reference a stored interface (sidecar path or `contract://domain/name@version[?alt=…]`) | §D.2 query |
| `@mununu_controllable <label>` / `@mununu_uncontrollable <label>` | Override default controllability | A §4 |
| `@mununu_register <name> at <addr> <RW|RO|WO>` | Memory-mapped register declaration | C §C.4 |
| `@mununu_behavior <pattern>(<args>)` | Reference a named behavioural pattern | A §2.ii |

### D.5.2 Per-language wrappers

The discovery pipeline strips the wrapper and feeds the inner tag + body to a single parser. Same vocabulary across languages; same diagnostics.

- **SystemVerilog:** Verilog attribute `(* mununu_xxx [...] *)` — survives encryption, sits on the port line — or line comment `// @mununu_xxx ...`.
- **C / C++:** Doxygen-style `/** @mununu_xxx ... */` on the function/struct/typedef, or `//` on declarations. Header-only annotations are the common case — exactly where they need to be for a black-box library.
- **TypeScript / JavaScript:** JSDoc `/** @mununu_xxx ... */` on the class/function/interface.
- **Rust:** Outer doc attribute `#[doc = "@mununu_xxx ..."]` or attribute macro `#[mununu_blackbox]` for crates that adopt a tiny proc-macro shim.
- **Python:** Decorator `@mununu.blackbox` or sentinel comment `# mununu: @mununu_xxx ...`.

### D.5.3 Worked example

Closed-IP DDR controller (the same example threaded through Documents A and B), now with vendor-supplied annotations:

```verilog
// illustrative — canonical form per this section
(* mununu_blackbox *)
(* mununu_guarantee = "G(awvalid → eventually awready)" *)
(* mununu_assume   = "G(aresetn = 0 → all_channels_idle within 8 cycles)" *)
(* mununu_interface = "contract://rtl_memory/axi4_slave@2" *)
module ddr_controller (
    input  wire aresetn, input wire aclk,
    input  wire        awvalid, output wire awready,
    /* @mununu_uncontrollable bresp */ output wire [1:0] bresp,
    ...
);
endmodule
```

The discovery pipeline returns a contract automaton built from the named AXI4-slave interface (looked up in the corpus per §D.2) refined with the two vendor-specific A/G formulas. Gap markers shrink to zero for this module.

### D.5.4 Simplification opportunity

Ship the parser shim for one language first (SystemVerilog — it has the largest existing annotation footprint, the M2 yosys path already detects `(* blackbox *)`), then add C / TS / Rust / Python in subsequent passes. The vocabulary is fixed; the per-language wrapper is a small extractor each time.

## D.6 L\* learning surface (`mununu contract learn`)

> **Post-M3 reassessment (2026-05-13): long-tail follow-up, no implementation queued.** L\* was proposed as the third contract source for cases where neither a corpus entry nor a source-comment annotation is available. After shipping M1 + M3 with corpus + annotations, no user case has surfaced that needed L\* — the two existing sources cover the common pattern. The A7 review surface's `ProposalProvenance` enum at [crates/mununu-core/src/contract/review.rs](../../crates/mununu-core/src/contract/review.rs) is open, so adding L\* later remains a one-place change. **Treat this section as a design placeholder.** No CLI subcommand, HTTP endpoint, or UI panel is committed; the engineering investment (teacher–student loop, counterexample-guided refinement, hooking the verifier as the teacher) does not match a real demand signal today. Revisit if a user case asks for it.

Owned by D because L\* is one of three *contract sources* alongside corpus lookup (§D.2) and source-comment annotations (§D.5). Putting all three in one document lets users reason about contract provenance in one place.

### D.6.1 Conceptual constraints

Also stated in [Document A §3.y](black-box-modules.md#3y-l-learning--conceptual-placement):

1. L\* is **opt-in**, not a required stage.
2. L\* output **never auto-applies** — it flows back into HITL stage 4.
3. Every L\*-learned assumption carries `provenance: l*` in the contract artefact.

### D.6.2 Three-surface parity

Enforced via the existing `parity-check` skill:

- **CLI:** `mununu contract learn --module M --target-property P --max-iters N`. Runs L\* with mununu's verifier as the teacher; emits a candidate assumption that, conjoined with the chaotic stub, suffices to prove P. Implementation route: Cobleigh, Giannakopoulou, Păsăreanu, *Learning Assumptions for Compositional Verification* (TACAS '03).
- **HTTP API:** `POST /contract/learn` with body `{module, target_property, max_iters}` mirroring the CLI semantics.
- **UI:** in the HITL stage-4 contract editor, a "Learn assumption" action button next to each gap marker; same backend.

### D.6.3 Sibling commands

Same parity rule applies to all three:

- `mununu contract query` — corpus lookup (§D.2).
- `mununu contract validate` — discharge check (A §3.x). **Already shipped in M1.**
- `mununu contract learn` — L\* (this section).

These three commands share one `mununu contract` subcommand group, one HTTP namespace, one UI panel. A user learning the contract workflow learns three commands with parallel shapes, not three unrelated tools.

### D.6.4 Simplification opportunity

**Phase 1 can ship `query` + `validate` only.** L\* is the lowest-priority of the three contract sources because corpus + annotations cover most cases. Defer `learn` until a real user case demands it. The CLI / API surface is reserved in the docs so adding it later is a no-breaking-change extension.

## D.7 Open questions

These are flagged as future work, not resolved.

- **Where does the corpus live in source control once it grows?** Sibling repo `mununu-corpus`, or stay in `mununu` until size demands a split?
- **How are contracts versioned for stability** — semver, content-hash, or both? Recommend content-hash for reproducibility + semver for human-readable references.
- **IP-XACT (IEEE 1685) and SystemRDL:** should corpus entries import-from / export-to these existing register-description formats? The doc recommends evaluation but no hard dependency.
- **Should the corpus accept user-uploaded contracts directly,** or gate community contributions behind review (mununu-verified flag)? Probably gate by default; community tier explicit.
- **Licensing of vendor contracts** — if Atomic Synthesis Co. contributes their IP contract, what license terms apply? Names the question; legal answer out of scope here.
- **CTXDSL grammar extension for inline `contract { ... }` blocks (stage-3 integration)** — Document A §2.vi notes that today contracts are JSON-only. Inline grammar is post-M3 work; this open question places the decision marker.

## D.8 Implementation plan

Concrete work items grouped by stream. **Before any task in §D.8.1+ starts, the §D.8.0 scoping pass must be completed.**

### D.8.0 Scoping pass — required gate before implementation

Same shape as [Document A §8.0](black-box-modules.md#80-scoping-pass--required-gate-before-implementation) and [Document B §B.7.0](rtl-frontend-unification.md#b70-scoping-pass--required-gate-before-implementation):

**Inputs:** this document; current state of `crates/mununu-core/src/contract/`, `crates/mununu-core/src/adapter/templates/`, the SV / yosys / extraction adapters as of M2 merge; `CLAUDE.md`. Document A and B are now on `main` — verify cross-doc references resolve.

**Checklist:** re-read §D.1–§D.7; re-verify every code reference; re-verify cross-doc dependencies (especially Document A's source-comment grammar reference and Document B's `BlackboxModuleEntry`); re-verify sequencing; re-verify estimates; record scoping log entry at `.claude/plans/scoping-logs/contract-corpus-and-config-implementation.md`; record GREEN / YELLOW / RED verdict.

### D.8.1 Task D1 — File-backed corpus + Phase 1 schema
**Touches:** new `crates/mununu-core/src/corpus/` module, optional new `corpus/` directory at workspace root (or sibling `mununu-corpus`).
**Scope:** define the contract entry schema (§D.2.3) as a Rust struct; implement file-backed lookup with parameter-match ranking; query API `query(domain, name, params) -> Vec<ContractEntry>`. No SQLite, no HTTP yet.
**Validation:** unit tests against three hand-authored corpus entries (matching, partial-match, no-match cases).

### D.8.2 Task D2 — `mununu contract query` CLI + HTTP
**Touches:** [crates/mununu-cli/src/main.rs](../../crates/mununu-cli/src/main.rs), [crates/mununu-core/src/api/server.rs](../../crates/mununu-core/src/api/server.rs).
**Scope:** new `mununu contract query <DOMAIN/NAME>` subcommand; `POST /api/v1/contract/query` HTTP endpoint. Both call into D1's library.
**Validation:** end-to-end test against the §D.9 industrial example.

### D.8.3 Task D3 — `.mununu/` directory layout
**Touches:** new `crates/mununu-core/src/project_config/` module.
**Scope:** the directory schema (§D.3.2). Load + validate routine; a small `migrate` CLI subcommand for legacy single-file configs. New projects opt in; old projects keep working.
**Validation:** load three real existing projects' legacy configs, run them against the migrate routine on paper, confirm semantic equivalence.

### D.8.4 Task D4 — Source-comment grammar parser (SystemVerilog first)
**Touches:** new `crates/mununu-core/src/annotations/` module, extension to [crates/mununu-core/src/adapter/systemverilog/annotation.rs](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs).
**Scope:** §D.5's tag table parsed in SV (Verilog attributes + line comments). Extracted annotations attach to the parsed Module AST and flow into the auto-emitted `BlackBoxInterface` (Document A §2.vi conceptual rule, finally a concrete producer).
**Validation:** unit tests against vendor wrapper patterns from §D.5.3.

### D.8.5 Task A6 (relocated from Doc A §8.6) — Discovery pipeline phase 2
**Touches:** [crates/mununu-core/src/contract/discover.rs](../../crates/mununu-core/src/contract/discover.rs).
**Scope:** extends phase 1 (shipped in M1) with the source-comment metadata reader (D4) and the corpus lookup client (D1 / D2). Each discovered automaton fragment / formula clause is tagged with provenance (`source_comment`, `corpus:<id>`, …).
**Validation:** golden tests against a small set of vendor-supplied annotation patterns covering each `@mununu_*` tag.

### D.8.6 Task A7 (relocated from Doc A §8.7) — HITL stage-4 UX
**Touches:** [crates/mununu-cli/src/main.rs](../../crates/mununu-cli/src/main.rs) (`mununu contract review` subcommand), [mununu-ui/](../../mununu-ui/) (review panel).
**Scope:** the HITL UX from Document A §3 stage 4 — surfaces proposed contracts + soundness flags + "what changes if you accept" preview. Wires together the proposal sources from A6, D §D.2 (corpus query), and D §D.6 (L\* learning) into one approve/edit/reject flow.
**Validation:** scripted end-to-end run on the §D.9 example: extract → review → discharge → verify.

### D.8.7 Task D5 — `mununu contract learn` (L\*)
**Touches:** new `crates/mununu-core/src/contract/learn.rs`.
**Scope:** §D.6's CLI / HTTP / UI surface, but **deferred** per §D.6.4. The CLI / API stubs are reserved so the parity rule holds; the implementation lands when a real user case demands it.

### D.8.8 Sequencing summary

Recommended landing order: **D1 → D2 → D3 → D4 → A6 → A7 → D5**. Each row delivers value standalone:

| Task | Standalone value |
|---|---|
| D1 | Library-side corpus lookup works against hand-authored entries |
| D2 | Users can query the corpus from CLI / API |
| D3 | `.mununu/` directory works for new projects; legacy configs still load |
| D4 | SV annotations are picked up by the auto-emission shipped in M2 |
| A6 | Discovery pipeline phase 2 — source-comment + corpus produce real contracts, not chaotic stubs |
| A7 | HITL workflow end-to-end: extract → review → discharge → verify |
| D5 | L\* learning — opt-in, the last contract source |

**Minimum viable slice: D1 + D2 + D4 + A6.** Ship those and the loop closes (extraction now uses real contracts, not just chaotic stubs). D3 (unified config) is high-value polish; A7 + D5 are the next iterations.

## D.9 Industrial example — TLS handshake with closed-IP crypto and corpus contracts

> **What shipped (M3.c, 2026-05-13):** [`examples/industrial/tls_handshake/`](../../examples/industrial/tls_handshake/) demonstrates the **corpus + annotations + discovery** slice end-to-end. The shipped slice covers two of the three contract sources (corpus hit + corpus miss with reference URI); HMAC's source-comment-only path, the `.mununu/` directory layout (§D.3), and the `mununu contract review` HITL UX (Document A §A7) are explicitly deferred to follow-up work. The shipped `validate.sh` reproduces a byte-deterministic `transcript.txt` against the pinned commit.

The example exercises the corpus + annotations + discharge end-to-end against a recognisable security-critical use case: a **TLS handshake state machine** that drives a closed-IP AES core and a closed-IP RNG.

### D.9.1 Why this example

- **Realistic.** TLS handshake state machines are everywhere — every HTTPS-capable device has one.
- **Critical.** Bypassed or downgraded handshakes have led to real CVEs (Heartbleed-adjacent, ROBOT, various downgrade attacks).
- **Three contract sources at once.** AES gets a corpus contract (mature, well-vetted). RNG gets a locally-authored contract (no general RNG contract exists). One submodule has only source-comment annotations. The example demonstrates all three working together.

### D.9.2 Components

```
┌──────────────────────────────────────────┐
│ TLS handshake FSM (open, verifiable)     │
│  ├─ ClientHello / ServerHello state      │
│  ├─ Key exchange driver                  │
│  └─ Finished message sequencer           │
└──────────────────────────────────────────┘
       │              │              │
       ▼              ▼              ▼
┌───────────┐   ┌───────────┐   ┌──────────┐
│ AES-CTR   │   │ RNG       │   │ HMAC     │
│ (corpus)  │   │ (local)   │   │ (annotated) │
└───────────┘   └───────────┘   └──────────┘
```

### D.9.3 What the example demonstrates

| Concept | How |
|---|---|
| §D.2 corpus query | `mununu contract query rtl_crypto/aes_ctr@1` returns the AES contract; discovery pipeline picks it up automatically. |
| §D.3 `.mununu/` directory | The project is laid out with `contracts/refs.json` (corpus refs) + `contracts/local/rng_v1.json` (locally-authored) + `contracts/todo/` (any remaining gaps). |
| §D.5 source-comment annotations | The HMAC wrapper has `@mununu_guarantee` / `@mununu_interface` annotations; the parser (D4) picks them up. |
| §A6 phase-2 discovery | Replaces chaotic stubs with real contracts for all three closed-IP components. |
| §A7 HITL review | `mununu contract review` surfaces the contract set; the user approves. |
| §A3.x discharge | The corpus + local + annotation set produces an acyclic discharge graph for the TLS-handshake-completes property. |

### D.9.4 Properties of interest

1. **Safety: no application data flows before the handshake completes.**
2. **Confidentiality: the master secret never appears on the wire.**
3. **Liveness (contingent): a valid handshake eventually completes** — contingent on the AES contract's latency clause + the RNG's "eventually produces" assumption.

### D.9.5 Concrete validation script

Same shape as M1/M2: `validate.sh` reproduces a byte-deterministic transcript. Specifically, it:

1. Loads the `.mununu/` project directory.
2. Runs discovery — pulls AES contract from corpus, local contract from `contracts/local/`, annotation-based contract from HMAC wrapper.
3. Runs the discharge check.
4. Runs `mununu context eval` for each of the three properties.

The cross-check this time is **provenance verification**: every clause in the discharge output carries its source (`corpus:rtl_crypto/aes_ctr@1`, `local:rng_v1`, `source_comment:hmac.sv:8`). The transcript shows the audit trail.

### D.9.6 What this example does NOT claim

Per the [CLAUDE.md claims-integrity rules](../../CLAUDE.md):

- It does **not** claim mununu found a bug in any commercial TLS implementation.
- The corpus AES contract is illustrative, **not** derived from any specific vendor's AES core. The provenance tag would be `mununu-verified (illustrative)`.
- The "TLS handshake state machine" is a stylised abstraction — real TLS state machines have substantially more states and edge cases.
- The proof is conditional on the contracts; the contracts are conditional on the vendors honouring their datasheets.

## D.10 Publication plan

Two derivative artefacts publish the result after the §D.9 transcript is reproducible.

### D.10.1 Substack — "A contract corpus for hardware verification — adopting OVL's idea, extending it to automata"

**Audience:** formal-methods practitioners, hardware verification engineers, security researchers, ecosystem maintainers thinking about how to share verification artefacts.

**Structure:**
1. The problem — every adapter today reinvents what "this module is a closed IP" means.
2. The three sources of contracts: corpus, source-comment annotations, L\*. One vocabulary across all three (§D.5 tag table).
3. The corpus design — file-backed Phase 1, alternatives as first-class, ranked candidates.
4. The unified `.mununu/` directory — replacing the alphabet soup.
5. Walking the TLS handshake example end-to-end with the actual transcript.
6. Provenance: every clause has a source. Audit trail.
7. Honest caveats from §D.9.6.
8. Pointer to Document C as the capstone.

**Length target:** 2500–3500 words. One transcript block, two diagrams, no marketing language.

### D.10.2 LinkedIn — "Mununu's contract corpus: borrowing OVL's idea, extending it to behavioural automata"

**Audience:** semiconductor / formal verification leadership, technical decision makers, OSS ecosystem watchers.

**Structure:**
- Two-sentence problem statement.
- One-sentence what-we-built summary (corpus + unified config + annotations + provenance).
- Two-sentence summary of the TLS handshake example.
- Link to the Substack deep dive and the example directory.

**Length target:** 150–200 words.

### D.10.3 Validation gate (per §A10.3 / §B.9.3)

Before either draft posts publicly, all four checks must pass:

1. `./examples/industrial/tls_handshake/validate.sh` exits 0 against the pinned commit.
2. Every transcript verdict line cited in the Substack matches the script's output byte-for-byte.
3. Claims-integrity checklist signed off — no claims about real TLS bugs, illustrative provenance on the AES contract explicitly stated.
4. Second reviewer confirms §D.9.6 caveats are not buried.

Drafts are not written before §D.9 is reproducible.

## D.11 What comes next

When this document is marked **implemented** (tasks D1–D5 + A6 + A7 landed), **validated** (the §D.9 transcript is reproducible), and **published** (the §D.10 posts are live), the next document to tackle is:

→ **[Document C — HW/SW codesign extraction](hw-sw-codesign-extraction.md)** (design landed; implementation deferred).

Document C is the **capstone** for the four-document arc. It composes Document A's controllability rule, Document B's dual-frontend unification, and Document D's corpus + sidecar layout into one industrial use case: peripheral RTL + firmware C, coupled via a register-map sidecar (the format sketched here in §D.3.2 under `coupling/register_maps/` and developed in Document C §C.3.2).

The full roadmap order: **A → B → D → C → governance update**. See the planning file at `.claude/plans/i-want-you-to-distributed-orbit.md` for the milestone breakdown.

---

## References

**Corpus-like precedents in hardware verification.**
- Accellera Open Verification Library (OVL) — [accellera.org/activities/working-groups/ovl](https://www.accellera.org/activities/working-groups/ovl). Parameterised assertion checkers; the closest mature precedent for a contract corpus. Mununu extends OVL's "parameterised predicate" pattern to "parameterised (automaton + formulas)".
- SystemVerilog Assertions (SVA) — IEEE 1800-2023; the vocabulary mununu's source-comment annotations target.
- Property Specification Language (PSL) — IEEE 1850-2010.

**Software contract languages.**
- ACSL (Frama-C) — [frama-c.com/html/acsl.html](https://frama-c.com/html/acsl.html). `requires` / `assigns` / `ensures` vocabulary; mununu's `@mununu_assume` / `@mununu_guarantee` map directly onto this.
- JML — Java Modeling Language. Earlier ACSL precedent.
- SPARK Ada — `Pre`, `Post`, `Contract_Cases`. Same vocabulary.
- Naks, T. *Program Verification in SPARK and ACSL: A Comparative Case Study* — [Springer](https://link.springer.com/chapter/10.1007/978-3-642-13550-7_7).

**Public verification artefact corpora (the model for §D.2.2 Phase 3).**
- SV-COMP benchmarks — [github.com/sosy-lab/sv-benchmarks](https://github.com/sosy-lab/sv-benchmarks). 30,000+ C verification tasks, public Git repo, structured categorisation, community contribution.
- HWMCC benchmarks — [hwmcc.github.io](https://hwmcc.github.io/2024/). BTOR2 format, RISC-V cores, sequential equivalence checking.

**Register description formats relevant to §D.7's IP-XACT / SystemRDL question.**
- IP-XACT (IEEE 1685-2022) — [Accellera User Guide](https://accellera.org/images/downloads/standards/ip-xact/IPXACT-2022_user_guide.pdf).
- SystemRDL — Accellera, focused on register descriptions.
- CMSIS-SVD — ARM. Influenced by IP-XACT, simpler scope.

**Learning-based assumption generation (§D.6).**
- J. M. Cobleigh, D. Giannakopoulou, C. S. Păsăreanu, *Learning Assumptions for Compositional Verification* — TACAS '03, LNCS 2619. [Springer](https://link.springer.com/chapter/10.1007/3-540-36577-X_24).
