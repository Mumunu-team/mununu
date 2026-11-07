> Status: historical — written in 2026-05 and deleted in `ce1da0c6` (PR #57, 2026-05-16); restored here on 2026-11-07 from `ce1da0c6^:docs/design/black-box-modules.md` because module docs, READMEs and tests still cite it (roadmap D2). Not maintained since: names, paths and claims below describe the code as it was then. The live design notes are under `docs/design/`.

# Black-box modules in compositional extraction

> **Status:** Design-stage reasoning. Not shipped architecture.
> **Audience:** Reviewers thinking about how mununu should model components it cannot see into.
> **Companion documents:** [B — RTL frontend unification](rtl-frontend-unification.md), [C — HW/SW codesign extraction](hw-sw-codesign-extraction.md), [D — Contract corpus and config](contract-corpus-and-config.md).

## 1. The black-box question

Mununu extracts a CLTS from a real system — RTL design, software service, multi-module project — and verifies properties over the result. The current pipelines assume **full source visibility** of every component in the composition. That assumption breaks the moment we touch:

- Hierarchical RTL where one submodule is a closed IP block (encrypted SystemVerilog, vendor NDA, pre-synthesized macro, `(* blackbox *)` attribute).
- Software systems that call into third-party libraries, opaque microservices, or runtimes we cannot statically analyse.
- Mixed designs that have *partial* visibility — we see the interface (port list, function signatures) but not the body.

This is precisely the situation that motivated the half-century arc of compositional verification: from Misra & Chandy's networks of processes (1981), through Pnueli's "modular temporal reasoning" (1985) and Jones's rely/guarantee, to the COMPOS '97 manifesto that compositionality is *the significant difference* between toy verifiers and ones that scale. Mununu inherits the same problem and the same vocabulary.

Three concrete sub-cases recur:

1. **Pure interface, no contract.** Only the port list / function signature is known. The vendor or library author supplied nothing else.
2. **Interface + assumption set.** The vendor states e.g. "input `a` is held stable for two cycles before `b` toggles." We can encode these as constraints on the environment.
3. **Interface + assumption + guarantee.** Full assume-guarantee contract: under stated input assumptions, the component delivers stated output guarantees.

This document reasons about how to handle all three without abandoning the soundness guarantees mununu already makes. The goal is a shared mental model that future adapter work (compositional SV, multi-service software extraction, MCP server federation) can refer to.

## 2. Extracting the interface — unified discovery pipeline

The two intuitive encodings — *chaotic stub* (one self-looping state, every output free) and *contract-shaped automaton* (assumptions and guarantees enforced) — look like two modes but are really two ends of one spectrum. The pipeline this section proposes always emits a **contract-shaped artefact**; the chaotic case is the degenerate one where the contract is empty.

This lets the rest of mununu treat all black boxes uniformly (always a contract object), while preserving the soundness asymmetry that motivated the distinction in the first place.

### 2.i The pipeline

For each black-box module `m`, the discovery pipeline produces a contract object with five fields:

1. **Interface alphabet** — labels derived from the port list / function signature.
2. **Controllability classification per label** — automatic from port direction (per §4 below).
3. **Discovered automaton fragments** — sequencing patterns mined from any source we *do* have access to:
   - Source-comment metadata (`@mununu_blackbox`, `@mununu_assume`, `@mununu_guarantee`, `@mununu_interface`; see §2.vi).
   - Contract corpus lookups by `(domain, module_name, parameters)` (see Document D §D.2).
   - Property-template references on existing sidecars (`.mununu.json`, `.espec.json`).
   - Verilog `assert property` / `assume property` blocks that survive encryption.
   - For software: doctests, JSDoc `@throws`, MyPy stubs, MCP `tools/list` schema.
4. **Discovered formula assumptions / guarantees** — invariant-style constraints; see §2.ii for the rule that decides automaton-vs-formula.
5. **Gap markers** — explicit `unknown { ... }` regions where nothing was found and the user must decide whether to accept chaotic semantics or hand-author a contract.

The output is always a contract; an empty contract is exactly the chaotic stub. There is no "switch mode" decision for the user to make — the pipeline always produces the most specific contract it can derive, and the gap markers tell the user where they would gain by adding more.

### 2.ii Automaton vs formula

A constraint at a black-box boundary can be encoded as either a labelled automaton or a temporal formula. The choice matters because automata blow up the composition product, while formulas are evaluated against it. The decision rule:

| Constraint shape | Encoded as | Rationale |
|---|---|---|
| Temporal sequencing of interface events ("`push` must precede `pop`", "after `reset` deasserted, `ready` rises within 2 cycles") | **Interface automaton** (states + labelled transitions) | Sequencing requires memory; this is exactly what Interface Automata (de Alfaro & Henzinger 2001) are for. |
| State predicate on a snapshot of interface labels ("`full` and `empty` are never both high") | **Formula assumption / guarantee** (mu-calculus / LTL invariant) | No memory needed; a global formula evaluates faster than an automaton state-product. |
| Causal / response pattern ("if `req` rises then `ack` eventually rises") | **Formula** (mu-calculus AF / LTL response) | Mununu already has templates for these; the formula is more readable than an equivalent Büchi-style encoding. |
| Quantitative bound ("between any two `tx` events there are at least 10 cycles") | **Automaton** *or* a parameterised template formula with bounded modal `max_steps` ([mu_calculus/mod.rs:323](../../crates/mununu-core/src/mu_calculus/mod.rs#L323)) | Counter automata blow up the product; bounded-modal formulas are tighter. |

Rule of thumb: **sequencing → automaton, predicate → formula, response → formula, counter → formula-with-bound.** This partition matches the Interface Automata split between the I/O-automaton skeleton and the trace-property side of a component contract.

### 2.iii Default behaviour, reported loudly

The recommendation is straightforward: chaotic stub as the *default*, hand-authored or corpus-supplied contract as opt-in. The asymmetry of costs is what makes this the right call:

- A chaotic stub being too permissive yields conservative verdicts. The verifier may report "safety holds" under stronger assumptions than the real component needs, or report a false unrealizability. The user can tighten the contract and recheck.
- A hand-authored contract being a *lie* yields false safety claims. The verifier reports "safety holds" against a fictional component, and the real system has the bug the contract said couldn't happen. There is no recovery — the user has shipped on an unsound proof.

Over-approximation + safety is sound (per the project's soundness rules — see [CLAUDE.md](../../CLAUDE.md)); under-approximation + safety is unsound. The default must be conservative.

But the user must always know which mode they are in. Whenever the discovery pipeline emits a contract that is not fully specified — i.e. contains any `unknown { ... }` gap markers — mununu must:

1. **Log a structured diagnostic** at extraction time, naming each gap (module, port, gap kind) and the soundness consequence. Example:
   ```
   WARN: blackbox FifoIp has unknown output sequencing on {full, empty};
         safety verdicts hold; liveness verdicts using these labels are unsound
         (no progress assumption discharged).
   ```
2. **Emit a `.contract.todo.json` skeleton** next to the input, pre-filled with the discovered fields and explicit empty slots for the gaps.
3. **Tag downstream verdicts** with a contract-completeness annotation, so the API response / CLI output can show "verified under chaotic FifoIp stub" rather than just "verified."
4. **Refuse silently passing** under a `--strict-contracts` flag. Off by default; CI for safety-critical projects flips this on, and any gap marker becomes a hard error.

The principle: the default is sound, but the user should always know when they are coasting on the default vs. relying on an authored contract. The current behaviour — mununu silently ignoring unparsed instantiations — violates this principle and is the bug this pipeline fixes.

### 2.iv Worked example — AXI master against closed-IP DDR controller

The surrounding system is an AXI master writing to a closed-IP DDR controller. The vendor delivers an encrypted Verilog file plus a small unencrypted wrapper.

- **Available**: AXI port list (channel signals AR/AW/R/W/B), reset, clock.
- **Discovered from the wrapper's header `assert property` blocks**: `AWVALID → AWREADY eventually` (formula), `BRESP only after WVALID handshakes` (automaton fragment).
- **Gap**: data-payload semantics, response latency upper bound.
- **Output**: contract with the response-pattern formula, the partial handshake automaton, and `unknown { latency_bound }`. Verifying "master never gets stuck" produces "holds under chaotic DDR latency; recommend authoring `latency_bound` for tighter liveness."

The user can then either accept that they cannot prove liveness against a chaotic latency, or hand-author the bound (or pull it from the corpus, per Document D §D.2).

### 2.v Simplification opportunity

The pipeline above can be implemented incrementally. Phase 1 ships just steps 1, 2, and 5 (alphabet + controllability + gap marker). Phase 2 adds discovered automaton fragments. Phase 3 adds discovered formulas. Each phase is independently useful.

The user-facing reporting from §2.iii is **independent of any phase** and should land first. It is a near-free change that immediately makes the existing "silently ignore unparsed modules" behaviour visible. This reporting layer is the highest-leverage cheapest piece of the entire proposal; everything else can follow.

### 2.vi Source-comment metadata (conceptual hook)

The discovery pipeline's step 3 ("discovered automaton fragments") and step 4 ("discovered formulas") both consume source-embedded annotations: `@mununu_blackbox`, `@mununu_assume`, `@mununu_guarantee`, `@mununu_interface`, `@mununu_controllable`, `@mununu_uncontrollable`, `@mununu_register`, `@mununu_behavior`.

The conceptual point lives here: **the same vocabulary applies across SystemVerilog, C, TypeScript, Rust, and Python**, parsed once after stripping the language-specific comment / decorator / attribute wrapper. Vendors annotate their source once, mununu picks it up regardless of language. This is one of the largest user-facing simplifications in this design.

The canonical tag table, per-language syntax wrappers, and worked example live in [Document D §D.5](contract-corpus-and-config.md#d5-source-comment-annotation-grammar). Keeping the representation there avoids duplicating the schema in two docs and lets the conceptual rule (this section) and the canonical form evolve independently.

## 3. Assume / guarantee structure — HITL pipeline

Two layers of contracts:

- **Global properties** (current state). One set of formulas applied to the composed system. Mununu's IR already carries `PropertyRole::{Assumption, Guarantee, Invariant, Standalone}` ([adapter/ir.rs:211](../../crates/mununu-core/src/adapter/ir.rs#L211)). This layer exists and is wired through. The contract IR treats `Invariant` as a guarantee with universal temporal scope (it is wrapped in `G` by the emitter).
- **Per-component contracts** (new). `(A_m, G_m)` per module `m`. When `m` is black-box, `A_m` is *required* (without it the chaotic stub is the only option) and `G_m` is enforced *by axiom* on the stub automaton.

The discharge rule, following Pnueli (1985) and Abadi & Lamport (1995): a global guarantee `G_top` is provable from `(⋀_m A_m → G_m) ∧ (⋀ environment-of-top assumptions)`, with the side condition that every `A_m` consumed by a sibling must be guaranteed by either another module's `G_k` or by the top-level environment.

The well-known soundness pitfall — circular A/G can be unsound without an inductive condition — is handled by McMillan-style circular reasoning when needed (K. L. McMillan, "Circular Compositional Reasoning about Liveness," CHARME '99). This document names the pitfall and proposes a detection check (§3.x) but does not commit to an implementation route for the McMillan-style discharge itself.

### 3.i The six-stage pipeline

```
Extract → Propose → Decompose → Review (HITL) → Discharge → Verify
   1         2          3            4              5         6
```

| Stage | Automated? | What happens |
|---|---|---|
| **1. Extract** | full | Per-module skeleton from §2 pipeline: alphabet, controllability, discovered automaton fragments, discovered formulas, gap markers. |
| **2. Propose** | best-effort | For each gap, propose candidate `A_m`/`G_m` from (a) property templates matching the module's domain profile, (b) L*-learned assumptions from counterexample traces (Cobleigh-Giannakopoulou-Păsăreanu, TACAS '03), (c) common patterns in the templates registry. |
| **3. Decompose** | partial | When a global `G_top` cannot be discharged from the current contracts, propose a *decomposition* — split `G_top` into per-module sub-obligations that, conjoined, imply `G_top`. Mechanical for conjunctive properties; for liveness, requires McMillan-style fairness placeholders the user must approve. |
| **4. Review (HITL)** | human required | The user reviews proposed contracts, decomposition, and external-contract references (vendor-supplied A/G). Mununu surfaces: each proposed clause, its soundness flag (sound / unsound / contingent), and a one-screen "what changes if you accept" preview that runs the verifier under the proposed contract and shows the new verdict. |
| **5. Discharge** | full | After the user accepts a contract set, mechanically check the discharge graph (§3.x): every consumed `A_m` is guaranteed by some other module's `G_k` or by the top-level environment. Report unmet obligations as a *discharge failure* (not a verification failure — distinct UX). |
| **6. Verify** | full | Run the standard mu-calculus / synthesis pipeline against the composition + contracts. |

### 3.ii Why human-in-the-loop stays in stage 4

Three reasons HITL is not just convenient, it is required:

1. **External contracts are out-of-band trust assertions.** When a vendor says "this IP guarantees X under assumption Y," accepting it into the proof system is a *trust decision*. Mununu must not silently accept; the user must sign off.
2. **Decomposition is rarely canonical.** A global property usually admits multiple decompositions, each with different ergonomics and proof strengths. Heuristics propose; humans pick.
3. **L\* and template proposals are speculative.** They are sound if accepted — they only constrain the environment — but they may be far weaker or stronger than the user intends. Without review, accepted assumptions silently change the verdict.

### 3.iii Worked example — bus arbiter with closed-IP master and slave

Bus arbiter, closed-IP AXI master, and closed-IP AXI slave, all on a shared fabric. Global property: "no bus deadlock."

- **Stage 1** extracts each component's interface contract from the §2 pipeline; both vendor IPs have gap markers on liveness sequencing.
- **Stage 2** proposes — from the `protocol_implementation` domain templates — `A_master = G(req → eventually grant)` and `G_master = G(grant → release within K cycles)`, plus the symmetric pair for the slave.
- **Stage 3** decomposes "no bus deadlock" into three per-pair obligations: master/arbiter, arbiter/slave, and a top-level fairness assumption.
- **Stage 4**: the user accepts the master's contract (matches vendor datasheet), rejects the slave's response-time clause (vendor only commits to a weaker bound), and edits the top-level fairness clause.
- **Stage 5**: the discharge check passes — every assumption has a guarantor.
- **Stage 6**: verification succeeds. The output report lists the contract set used, with provenance per clause: `user-supplied / template-derived / discovered-from-source / corpus:rtl_protocol/axi4@2.0.1`.

### 3.iv Simplification opportunity — a minimum viable version

The full six-stage pipeline is rich. A minimum-viable version drops stages 2 and 3 initially and ships 1 + 4 + 5 + 6:

- Without Propose, the HITL UX in stage 4 degrades to "fill in this `.contract.todo.json` and re-run." Not glamorous, but functional.
- Without Decompose, the user takes the global property as given and edits per-module contracts directly. They can decompose by hand.
- **Discharge (stage 5) is the highest-value automation** because it catches the most common A/G mistake — circular reasoning where a module's assumption is only ever guaranteed by itself. This is what mununu cannot detect at all today.
- L\* in stage 2 is a long-tail enhancement, not a phase-1 requirement.

The recommended ordering for implementation effort: **5 → 4 → 1 → 2/3 → 6 wiring**. Discharge-first because it is fully mechanical and catches the most embarrassing soundness gaps; HITL UX next because that is what makes the discipline usable; then extraction quality and learning-based proposals.

### 3.x Circular-reasoning detection

Whether a contract set requires circular A/G reasoning (à la McMillan 1999) is **deterministically decidable from the discharge graph**.

Build the dependency graph:
- **Nodes**: each clause in the contract set — every `A_m` and every `G_m` for every module `m`.
- **Edges**: a directed edge from `G_k` to `A_m` whenever `G_k` is the claimed discharger of `A_m`.

Run Tarjan's or Kosaraju's SCC on this graph:

- **All SCCs are singletons** → the discharge order is a linear topological sort → the standard non-circular Pnueli (1985) rule suffices. Report: `discharge: acyclic`.
- **At least one non-trivial SCC** → at least one cycle exists in the A/G dependencies → circular reasoning is required. Report: `discharge: circular reasoning required at {clauses}`. Mununu cannot prove this with the Pnueli rule alone. The user has two routes: (a) break the cycle by rewriting a clause to be unconditional, or (b) accept circular discharge, which requires McMillan-style induction over the alternation depth of the cyclic obligations. Today mununu does not implement (b); this document names it as future work but explicitly **refuses to silently accept** circular discharge.

**Approximation when the graph is incomplete.** Some clauses come from the corpus and may not be loaded at check time, or reference external contracts. Conservatively treat unknown clauses as participating in every cycle that touches their module. Report this as `discharge: potentially circular (clauses {X, Y} unresolved)` so the user knows whether the warning would clear once the corpus is refreshed.

The check is mechanical, runs in linear time over the contract set, and has very high value: today mununu has no notion of A/G discharge at all, so circular reasoning slips through completely undetected. Shipping just the SCC analysis with a clear diagnostic is one of the highest-leverage moves in this entire plan.

### 3.y L\* learning — conceptual placement

The Cobleigh, Giannakopoulou & Păsăreanu (2003) L\*-based assumption learning loop is the obvious automated route for stage 2 (Propose) when no source-comment metadata and no corpus entry are available. Three conceptual constraints belong here:

1. **L\* is opt-in**, not a required stage. The default contract for an un-annotated black box stays chaotic, with gap markers visible to the user.
2. **L\* output never auto-applies.** It always flows back into the HITL stage 4. This preserves the §2.iii principle (no silent change to verdicts) and matches the original L\* paper's intended use as a teacher-student loop with the user in the loop at the end.
3. **Every L\*-learned assumption is tagged with `provenance: l*`** in its contract artefact so future readers can see it was machine-proposed and treat it with appropriate skepticism.

The exact CLI / API / UI surface for `mununu contract learn` lives in [Document D §D.6](contract-corpus-and-config.md#d6-l-learning-surface-mununu-contract-learn), which owns three-surface parity for the `mununu contract {query, validate, learn}` command group. Keeping the surface in D means there is a single home for "how the user invokes L\*" alongside "how the user invokes corpus lookup" — they are sibling contract-source operations.

## 4. Controllability for black boxes

The key insight: **controllability is a property of a label relative to the surrounding scope, not an intrinsic property of the signal.** This is the same observation underlying the controlled-variable / external-variable distinction in Reactive Modules (Alur & Henzinger 1999).

The rule restated:

- A black-box's **output port** is uncontrollable from the perspective of the rest of the design — the surrounding controller cannot choose its value; the black-box's internals do. Labels driven by black-box outputs map to `Uncontrollable`.
- A black-box's **input port** is controllable from the perspective of the surrounding logic that drives it. Those labels stay whatever the driver was (typically `Controllable`).
- **Internal signals of the black-box never appear in the label alphabet.** They are not observable from the outside. A property that reads such a signal is ill-typed against the model.

This dissolves the question "what about controllability for black boxes": it is the existing rule (port direction → controllability class) applied at a new boundary. The principle:

> The controllability of a transition label is the controllability of whichever side of the boundary drives it.

This is enforced by the `LabelControllability` enum at [crates/mununu-core/src/clts/mod.rs:248](../../crates/mununu-core/src/clts/mod.rs#L248), which already covers `Controllable`, `Uncontrollable`, and `Internal`. No new enum variant is needed; the rule operates over the existing three.

### 4.i Worked example — top module with closed-IP CRC engine

A top module instantiates a closed-IP CRC engine.

- The CRC engine's `data_in` port is driven by top → label is `Controllable` (the surrounding logic chooses when to push data).
- The CRC engine's `crc_out` port drives a top-level register → label is `Uncontrollable` (we cannot predict its bit pattern without the body).
- The CRC engine's internal `state[31:0]` shift register: not in the label alphabet at all. Properties cannot mention it.
- Result: the surrounding logic can be verified to never *miss* a CRC result, but cannot be verified to compute *the right* CRC unless the contract specifies the function.

### 4.ii Simplification opportunity — one rule across three pipelines

Mununu today has three independent controllability heuristics:

- Custom SV uses port direction at [adapter/systemverilog/kripke.rs:1021-1044](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L1021-L1044).
- BTOR2 defaults to all inputs `Uncontrollable` with a CLI override list at [adapter/btor2/bit_blast.rs:280-305](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs#L280-L305).
- The software extraction adapter uses method-name globs in domain profiles at [adapter/extraction/ast_extract/domain.rs:38-116](../../crates/mununu-core/src/adapter/extraction/ast_extract/domain.rs#L38-L116).

The unification is a one-rule replacement: classify labels by the direction of the driving port at the current scope's boundary. The CLI override flags can stay as escape hatches for unusual cases (a designer who wants to treat a normally-output signal as adversarial for a particular property), but they should not be the primary mechanism. Cheap, high-clarity fix; details in [Document B](rtl-frontend-unification.md).

## 5. Consistency between yosys and custom-SV pipelines

The two RTL paths disagree about what compositional extraction means:

- **Custom SV** preserves module boundaries via sidecar JSON. It can express black-box-as-component naturally.
- **Yosys frontend** runs `flatten` unconditionally at [adapter/yosys/mod.rs:242](../../crates/mununu-core/src/adapter/yosys/mod.rs#L242). Boundaries are gone before BTOR2 sees the design. A `(* blackbox *)` module either fails (body is empty, producing no transitions for that module) or folds into spurious behaviour after `chformal -lower`.

The recommendation, summarised here and elaborated in [Document B](rtl-frontend-unification.md): yosys-side, detect `(* blackbox *)` / `keep_hierarchy` / per-module `BLACKBOX` attributes, **emit those modules separately as IR components with a chaotic-stub automaton**, and only flatten the visible interior. The composition step that already exists in custom SV (`ConnectionSpec`, port-binding to shared labels at [adapter/systemverilog/annotation.rs:1060-1122](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs#L1060-L1122)) becomes the shared backend both frontends target.

This aligns the two paths on the Interface Automata view: yosys produces a flattened body plus a list of interface-only neighbours, which is exactly the partition Interface Automata (de Alfaro & Henzinger 2001) operates over.

### 5.i Worked example — SoC fabric with open-source CPU and closed-IP DDR controller

A user wants to verify an SoC fabric consisting of an open-source CPU core and a closed-IP DDR controller. Using yosys today, `flatten` collapses the DDR controller body (currently empty under `(* blackbox *)`) and the verifier emits a degenerate Kripke structure. The user's properties about "CPU never deadlocks on outstanding DDR transactions" cannot be discharged.

With the proposed change, yosys emits two IR components — the CPU/fabric flattened body and a chaotic DDR stub — plus a composition spec wiring the AXI labels between them. The user can now discharge the deadlock property using a hand-authored or corpus-supplied DDR contract sitting next to the SoC source.

### 5.ii Simplification opportunity — the yosys script already has the hierarchy info

The yosys script already knows the design hierarchy before it runs `flatten`. The change is mostly bookkeeping: snapshot port directions per module pre-flatten, mark `(* blackbox *)` modules for separate emission, only then call `flatten` on the visible interior. No new IR shape is needed — `ConnectionSpec` covers it. This is the lowest-effort, highest-impact RTL change in this proposal.

## 6. The software analogue

The same machinery applies in software extraction, with three boundary types:

- **Library call without source.** Partially handled today via `CallSummary` with `CallEffect::Unknown` defaulting to havoc ([call_summary.rs:35](../../crates/mununu-core/src/adapter/extraction/ast_extract/call_summary.rs#L35)). Reframed: a per-callee `CallSummary` is exactly a rely/guarantee contract on a method-shaped black box (Jones 1983). Generalising it from per-method to per-component (per-class, per-service, per-MCP-server) is the software analogue of the RTL black-box question, and is the thread-modular view of Flanagan & Qadeer (2003) applied at the component grain.
- **Opaque microservice or MCP server.** The interface is its tool list (for MCP) or its API schema (for HTTP). Today: hand-author an `.espec.json` stub (`js_map_stub.espec.json` is the worked example). Future: auto-derive a chaotic stub from an MCP server's `tools/list` response, then let the user attach a contract on top — the same default / opt-in split as §2.
- **Async runtime or I/O.** Truly out-of-scope state. Today: domain profiles wave it away by marking certain method names uncontrollable. This is an *implicit* chaotic-environment stub. The recommendation is to make it explicit, so the user can see (and challenge) the abstraction.

### 6.i Worked example — TypeScript microservice with mixed library access

A TypeScript microservice calls `lodash.debounce` and `axios.post`.

- `lodash.debounce`: well-understood library, has a built-in `CallSummary` with `CallEffect::ReadOnly` semantics for the wrapping behaviour. Full contract, no stub needed.
- `axios.post`: HTTP egress to a third-party API. `CallSummary` resolves to `CallEffect::Unknown` → chaotic stub today (silent). With the §2.iii reporting requirement, the extractor emits a `.contract.todo.json` skeleton flagging the call site, with controllability classification (the response status code is `Uncontrollable` — environment chooses), so the user can add an assumption like "200 OK arrives within 5 s" if their property needs it.

### 6.ii Simplification opportunity — the existing havoc path needs user-visible reporting

The `CallEffect::Unknown` path already exists. The gap is *user-visible reporting* of which calls fell through to it. Adding a single structured warning when an `Unknown` summary is consumed gives the user the same diagnostic surface §2.iii proposes for RTL. Zero new analysis is required — just thread the existing classification through to the output. The same shape as the §2.v early-phase recommendation.

## 7. Resolved questions

Questions listed as open in earlier drafts have been resolved. The resolutions are recorded here so future readers see what was considered and what was decided.

**Q1 — Where do contracts live in CTXDSL?**
**Resolved.** Contracts attach at the same scope as their natural CTXDSL parent: to an `automaton { ... }` block when the contract is about a single component, and to a `composition { ... }` block when the contract is about an aggregate. This mirrors Interface Automata's per-component A/G triples (de Alfaro & Henzinger 2001) and Reactive Modules' hierarchical module declarations (Alur & Henzinger 1999). Both anchor points are needed: per-automaton for primitive black boxes, per-composition for compound systems where the contract is about the whole rather than the parts. The exact grammar — whether contracts are nested inside the parent block or live as siblings with a reference — is a syntactic question owned by [Document D](contract-corpus-and-config.md).

**Q2 — Do contracts compose automatically?**
**Resolved with a two-tier check.** The §3 pipeline always **proposes** a discharge for the user (stages 2 and 3); the user **approves, edits, or rejects** in HITL stage 4. Before reaching the user, mununu attempts to discharge the proposal mechanically:

1. **Acyclic discharge graph** (Pnueli 1985 rule applies): mununu proposes the linear topological order; HITL typically rubber-stamps it.
2. **Circular discharge graph — lightweight McMillan check.** When the §3.x SCC analysis finds a cycle, mununu first tries a *lightweight* McMillan-style discharge: it checks whether the cycle corresponds to a **strict mu-calculus rank ordering** that decreases around the cycle. This piggybacks on the existing fixpoint engine and does not require step-indexed reasoning. If the check succeeds, mununu accepts the cycle with provenance tag `mununu-verified circular discharge (mu-rank)`. The user is informed but not blocked.
3. **Circular discharge graph — fallback.** If the lightweight check cannot find a witness, mununu surfaces the cycle to HITL as in the original answer. Approval is **axiomatic acceptance** — provenance tag `user-approved circular discharge (no mu-rank witness)`. The user takes responsibility for the soundness of the cycle, the same way they take responsibility for vendor-supplied contracts.

The full McMillan check with step-indexed witnesses (CHARME '99 in its general form) is deferred future work — it requires adding step counters to the model and substantial proof machinery. The lightweight tier covers most well-formed cycles in practice (e.g. arbiter↔master fairness cycles encoded in nested fixpoints) and falls back honestly when it cannot. This staging matches the rest of the document's "ship the cheap mechanical check first; HITL covers the residual" principle.

**Q3 — How does this interact with the controller-mode strategy extraction (projection / functional / permissive)?**
**Resolved.** Synthesis is unchanged. The strategy extraction operates on whatever model it is handed; if a chaotic stub produces an over-cautious controller, the remedy is to **tighten the black-box contract**, not to change synthesis. Synthesis stays model-agnostic; abstraction quality is a model question, not a synthesis question. A consequence worth stating explicitly: **good controllers against black boxes require authored contracts.** If the user wants a permissive controller, they must give mununu enough information to know which environment behaviours are admissible.

**Q4 — Should mununu learn assumptions when none are supplied?**
**Resolved.** L\* learning is opt-in only and surfaces through the HITL pipeline. The user can add assumptions manually, refine an L\*-proposed assumption, or reject L\* from the start. There is no automatic invocation. Details in §3.y and [Document D §D.6](contract-corpus-and-config.md#d6-l-learning-surface-mununu-contract-learn).

### 7.1 If new questions emerge

The four questions above are closed. New questions that surface during the §8 implementation work — for example, around the exact discharge-graph rendering in the HITL UX, or around how `mununu contract validate` should report SCCs that touch unresolved corpus entries — will be added here as they appear, then resolved through the same milestone review process.

## 8. Implementation plan

The §3.iv sequencing — **5 → 4 → 1 → 2/3 → 6 wiring** — translates into the following concrete work items. Each item names the files that change, the scope, and the validation criterion. The order is deliberately the one that lets each item deliver value standalone, so the work can be paused after any task.

**Before any task in §8.1–§8.8 starts, the §8.0 scoping pass must be completed.** It is a required gate, not optional.

### 8.0 Scoping pass — required gate before implementation

Time elapses between writing this document and starting the work. The codebase moves, related documents land, new context appears. The scoping pass is a short structured re-read whose purpose is to catch drift before it becomes a bug in the implementation plan. It runs once, immediately before §8.1.

**Inputs to the scoping pass:**
- This document, read end-to-end with fresh eyes.
- The current state of `crates/mununu-core/`, `crates/mununu-cli/`, `crates/mununu-extract/`.
- Whichever of Documents B, D, C have landed (they may have shifted assumptions this document relies on).
- The project's [CLAUDE.md](../../CLAUDE.md) — soundness rules may have been refined.

**Checklist (each item produces a written note in the scoping log):**

1. **Re-read the reasoning sections (§1–§7).** Confirm the conceptual framework still holds given anything that has changed. If a paper, experiment, or related project change has invalidated an argument, **revise the document first** — do not implement against stale reasoning.
2. **Re-verify every code reference.** Each `crates/...#Lnnn` link in this document must still resolve to the symbol it claims. Run `grep` on the symbol name; if line numbers have drifted, update the document. If the symbol no longer exists, escalate — this is a load-bearing assumption.
3. **Re-verify task scope per task.** For each of A1–A8, confirm the file paths in the *Touches* line still exist and that the *Scope* description still matches the current codebase shape. A recent refactor may have folded one of the target files into another.
4. **Re-verify cross-document dependencies.** Document A's tasks cross-reference Document D's corpus schema (§D.2) and source-comment grammar (§D.5). If Document D has not landed, A6 is blocked; if D has landed with a different schema than this document assumes, update the A6 task description.
5. **Re-verify sequencing.** The ordering A1, A2, A3, A4, A8, A5, A6, A7 reflected the world at the time of writing. Has a downstream consumer (e.g. the §9 industrial example, or Document C's codesign pipeline) created a new precedence constraint? If so, adjust the order *in the document*.
6. **Re-verify estimates.** Original cost estimates (e.g. "~200–400 lines for A8") were rough. Sanity-check them against the actual file sizes and complexity of the relevant modules today. If an estimate is off by more than 3×, update it.
7. **Re-verify the minimum-viable slice.** The document claims A1–A3 are the minimum viable slice. Is that still true? Has any prerequisite shifted into A2 or A3, or has any A3 piece become irrelevant?
8. **Scoping log entry.** Write a short note — three to five paragraphs — recording what was found, what was updated, and what assumptions were re-confirmed. The log lives at `.claude/plans/scoping-logs/black-box-modules-implementation.md`. This is the audit trail; future readers see what the implementer knew when they started.
9. **Explicit go / no-go decision.** End the scoping pass with one of three verdicts, recorded in the scoping log:
   - **GREEN — proceed to §8.1.** Reasoning holds, references resolve, sequencing is current. Begin implementation.
   - **YELLOW — proceed with named adjustments.** Most checks passed but one or two items required document edits. The edits are listed in the scoping log; implementation begins against the updated document.
   - **RED — revise document first; do not implement yet.** A load-bearing assumption is invalidated. The reasoning sections need revision; once that lands the scoping pass repeats. This protects against implementing against stale plans.

**The scoping pass is cheap** — typically 1–2 hours of focused re-reading and grep. The cost of skipping it (implementing against a stale document, then discovering the drift halfway through) is much higher. The pass is mandatory; the verdict is not predetermined.

**Repetition.** If implementation pauses for more than two weeks at any point during §8.1–§8.8, the scoping pass is re-run before resuming. The audit log accumulates one entry per re-run.

### 8.1 Task A1 — `Contract` IR type and discharge graph machinery
**Touches:** [crates/mununu-core/src/adapter/ir.rs](../../crates/mununu-core/src/adapter/ir.rs), new module `crates/mununu-core/src/contract/`.
**Scope:** introduce a `Contract` struct that bundles `{alphabet, controllability_map, automaton: Option<Clts>, formulas: Vec<(PropertyRole, Formula)>, gap_markers: Vec<GapMarker>, provenance}`. Wire it into `AdapterIR` so each component can carry an optional contract. No verification semantics change yet — this is plumbing.
**Validation:** existing tests pass; new round-trip test that a component with an empty contract produces the same CTXDSL as today.

### 8.2 Task A2 — SCC-based discharge check (the highest-leverage piece)
**Touches:** new module `crates/mununu-core/src/contract/discharge.rs`, CLI wiring in [crates/mununu-cli/src/main.rs](../../crates/mununu-cli/src/main.rs).
**Scope:** build the discharge graph (nodes = clauses, edges from `G_k` to `A_m`); run Tarjan SCC; report acyclic / circular / potentially-circular per §3.x. Surface as both a library call and a `mununu contract validate` CLI subcommand.
**Validation:** three unit tests — one acyclic linear chain (reports acyclic), one self-loop (reports circular), one with an unresolved external clause (reports potentially-circular). Integration test against a hand-authored contract set.

### 8.3 Task A3 — gap-marker diagnostics and `--strict-contracts` flag
**Touches:** [crates/mununu-core/src/adapter/](../../crates/mununu-core/src/adapter/), wherever black-box modules are encountered today. Surface a structured `tracing::warn!` per gap marker plus an emitter for `.contract.todo.json` skeletons. Add `--strict-contracts` to the CLI; in strict mode any gap marker becomes a hard error.
**Scope:** **only** add reporting and the flag. Do not change verdict semantics. Existing behaviour with the flag off must be unchanged.
**Validation:** end-to-end test that today's silent-ignore on a closed-IP module now emits a warning, and that `--strict-contracts` returns a non-zero exit on the same input.

### 8.4 Task A4 — controllability rule unification (port direction at scope boundary)
**Touches:** [adapter/systemverilog/kripke.rs:1021-1044](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L1021-L1044), [adapter/btor2/bit_blast.rs:280-305](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs#L280-L305), [adapter/extraction/ast_extract/domain.rs:38-116](../../crates/mununu-core/src/adapter/extraction/ast_extract/domain.rs#L38-L116).
**Scope:** replace the three separate heuristics with a single shared classifier that operates on the driving port direction at the *current scope's boundary*. CLI override flags remain as escape hatches but are no longer the primary mechanism. Detailed in [Document B](rtl-frontend-unification.md).
**Validation:** regression suite for all three adapters; assertion that a black-box module's outputs are uniformly `Uncontrollable` and inputs are `Controllable` across all three paths.

### 8.5 Task A5 — discovery pipeline phase 1 (alphabet + controllability + gap marker)
**Touches:** new module `crates/mununu-core/src/contract/discover.rs`.
**Scope:** the §2.i pipeline restricted to fields 1, 2, and 5 of the contract object. For each detected black-box module, produce a contract object with the interface alphabet, controllability classification, and a gap marker explaining what is unknown. No automaton fragments, no formulas yet.
**Validation:** test against three input shapes — SV with `(* blackbox *)`, BTOR2 with declared external inputs, and TS with an opaque microservice call (`CallEffect::Unknown`).

### 8.6 Task A6 — (moved to Document D)
**Originally:** discovery pipeline phase 2 — automaton fragments from source-comments + corpus.
**Now owned by:** [Document D §D.8](contract-corpus-and-config.md#d8-implementation-plan), milestone M3.
**Why moved:** A6's two inputs — source-comment grammar and contract corpus — are both Document D deliverables (§D.5 and §D.2). Implementing A6 before D would force speculative API choices; implementing it alongside D keeps the contracts honest. Until M3 lands, mununu emits chaotic stubs (A5 phase 1) with full diagnostic visibility (A3), which is the right safety posture.

### 8.7 Task A7 — (moved to Document D)
**Originally:** HITL stage-4 UX — `mununu contract review` CLI + UI surface.
**Now owned by:** [Document D §D.8](contract-corpus-and-config.md#d8-implementation-plan), milestone M3.
**Why moved:** A7 surfaces proposed contracts to the user for approval / edit / rejection. Useful proposals come from the corpus (D §D.2), source-comment discovery (A6, now also M3), and L\* learning (D §D.6). Shipping A7 before those are wired produces an empty review screen. The full HITL flow lands with D as one coherent UX.

### 8.8 Task A8 — lightweight McMillan-style circular discharge
**Touches:** `crates/mununu-core/src/contract/discharge.rs` (extending A2), [crates/mununu-core/src/mu_calculus/](../../crates/mununu-core/src/mu_calculus/).
**Scope:** for each non-trivial SCC found by A2, attempt the lightweight mu-rank check per §7 Q2: does the cycle correspond to a strict mu-calculus rank ordering that decreases around it? Implement by walking the SCC, computing the alternation depth of each clause's fixpoint variable, and checking that the cycle has a strict monotonic decrease in this rank. If yes, mark the discharge `mununu-verified circular discharge (mu-rank)` and accept. If no, fall back to A2's "user-approved circular discharge (no mu-rank witness)" path.
**Validation:** three unit tests — (a) an arbiter↔master fairness cycle with a clean mu-rank witness (lightweight check passes), (b) a genuinely unsound cycle with no witness (lightweight check fails, falls back to HITL), (c) a sound-but-step-indexed cycle that the lightweight check cannot find a mu-rank witness for (falls back to HITL with a hint that step-indexed reasoning is needed).
**Out of scope:** the full McMillan check with step counters and explicit ranking functions. That remains deferred future work.

### 8.9 Sequencing summary

The recommended landing order for tasks owned by Document A is **A1, A2, A3, A4, A8, A5**. A6 and A7 have moved to Document D's M3 implementation plan — they depend on D's deliverables, and shipping them earlier would force speculative API choices.

| Task | Owner | Standalone value |
|---|---|---|
| A1 | M1 | Foundational plumbing; no user-visible change |
| A2 | M1 | Discharge check usable as `mununu contract validate` against any hand-authored contract set |
| A3 | M1 | Existing "silently ignore unparsed modules" bug fixed |
| A4 | M1 | Three controllability heuristics collapse to one rule |
| A8 | M1 | Lightweight circular-discharge check; auto-discharges most well-formed cycles |
| A5 | M1 | First version of the discovery pipeline; phase 1 only |
| A6 | **M3 (Doc D)** | Source-comment + corpus integration |
| A7 | **M3 (Doc D)** | End-to-end HITL workflow |

Tasks A1–A3 are the minimum viable slice; if work pauses after A3, mununu has gained the highest-leverage pieces (foundational types, circular-reasoning detection, gap visibility) without taking on the deeper extraction work. A8 slots in after A4 because it benefits from the unified controllability rule when building the discharge graph for cross-frontend cases. A5 phase 1 closes M1; phase 2 work (A6) and the HITL UX (A7) wait for D.

## 9. Industrial example — secure boot ROM with closed-IP crypto

The example exercises every concept in this document end-to-end against a realistic critical use case: a **secure boot ROM** that verifies a firmware signature before allowing execution. The cryptographic primitives are vendor closed-IP black boxes; the surrounding controller logic is open and verifiable.

### 9.1 Why this example

- **Realistic.** This is the architecture used in commercial secure-boot ROMs across the industry — Apple Secure Enclave, ARM TrustZone, Google Titan M, the OpenTitan reference design. The pattern is mainstream, not academic.
- **Critical.** Getting this wrong has consequences: a bypassed signature check means arbitrary firmware boots on the device. Real-world incidents (checkm8 on iPhones, various IoT boot vulnerabilities) make the verification stakes concrete.
- **Black-box-essential.** The crypto primitives (RSA signature verify, SHA-256 hash) are almost always sourced as closed IP from a vendor. The user *cannot* see inside them. The verification must work without that visibility — exactly the question this document answers.
- **Properties cross the boundary.** "No firmware executes without valid signature" and "key material never appears on the host bus during verification" both refer to interactions between the open controller and the closed crypto blocks. They cannot be verified inside either side alone.

### 9.2 Components

```
┌─────────────────────────────────────────┐
│ Secure Boot ROM (open, verifiable)      │
│  ├─ Boot controller FSM                 │
│  ├─ Flash interface                     │
│  └─ Host bus arbiter                    │
└─────────────────────────────────────────┘
       │                  │
       ▼                  ▼
┌─────────────┐    ┌─────────────┐
│ SHA-256 IP  │    │ RSA verify  │
│ (vendor BB) │    │ IP (vendor BB)│
└─────────────┘    └─────────────┘
```

- **SHA-256 IP** — closed-IP from Vendor V1. The vendor datasheet states: "after `start` rises with data presented on `din`, the output `hash_valid` rises within 64 cycles; `hash_out` reflects the SHA-256 of the input."
- **RSA verify IP** — closed-IP from Vendor V2. The vendor datasheet states: "after `verify_start` rises with `signature`, `hash`, and `pubkey` presented, the output `verify_done` rises within 1024 cycles; `verify_ok` indicates whether the signature is valid."
- **Boot controller** — open, hand-authored as CTXDSL. Drives flash reads, feeds the SHA-256 IP, then the RSA verify IP, then gates the host bus.

### 9.3 Properties of interest

1. **Safety: no firmware executes without valid signature.**
   `AG (host_bus_unlock → previously(verify_done ∧ verify_ok))`
2. **Confidentiality: key material never appears on the host bus during verification.**
   `AG ((sha_busy ∨ rsa_busy) → host_bus_quiesced)`
3. **Liveness (contingent): every valid firmware eventually boots.**
   `AG (signature_valid → AF host_bus_unlock)`

Properties 1 and 2 are safety; property 3 is liveness and depends on progress assumptions about the crypto IPs.

### 9.4 Walking the document concepts through the example

| Section | What it demonstrates on this example |
|---|---|
| §2.i discovery pipeline | The user runs `mununu contract discover --target secure_boot.ctxdsl`. Output: two contract objects for the SHA and RSA IPs, each with interface alphabets, controllability classifications, and gap markers on liveness. |
| §2.iii reporting | The output names the gaps explicitly: `WARN: blackbox SHA256_V1 has unknown output sequencing on {hash_out}; safety verdicts hold; liveness verdicts depending on hash_valid timing are unsound.` |
| §2.vi source-comment metadata | The vendor's `(* mununu_guarantee = "AG (sha_start → AF (hash_valid)) within 64" *)` attribute on the wrapper module is picked up by the discovery pipeline and replaces the liveness gap. |
| §3.x discharge check | The discharge graph shows that the RSA IP's assumption (`pubkey is loaded`) is guaranteed by the boot controller's `pubkey_load` state; the SHA IP's assumption (`data presented before start`) is guaranteed by the bus arbiter. Discharge: acyclic. |
| §3.x circular case | A deliberately broken variant where the RSA IP's assumption depends on the SHA's guarantee, *and* the SHA's assumption depends on the RSA's guarantee. The SCC check reports: `discharge: circular reasoning required at {A_rsa, G_sha, A_sha, G_rsa}`. The user must break the cycle by anchoring one assumption on the top-level reset. |
| §4 controllability | The SHA IP's `din` (input) is `Controllable` (the boot controller drives it); `hash_out` (output) is `Uncontrollable` (the vendor IP drives it). The boot controller cannot assume any specific value of `hash_out` for an arbitrary input. |
| §6 software analogue | Not exercised in this RTL example. The TypeScript microservice example in §6.i covers the software side. |

### 9.5 Concrete validation script

The example ships as a directory `examples/industrial/secure_boot_rom/` with:

```
examples/industrial/secure_boot_rom/
├── README.md                      # narrative of this section
├── secure_boot.ctxdsl             # open-source boot controller
├── sha256_v1.ctxdsl               # vendor wrapper with @mununu_guarantee
├── rsa_verify_v2.ctxdsl           # vendor wrapper with @mununu_guarantee
├── properties.ctxdsl              # the three properties of §9.3
├── broken_circular.ctxdsl         # the deliberately-circular variant for §3.x
└── validate.sh                    # runs all checks; produces transcript
```

The validation script runs:
```bash
# 1. Discovery on the un-annotated baseline
mununu contract discover --target sha256_v1_blank.ctxdsl
# expect: gap markers on liveness

# 2. Discovery with vendor annotations
mununu contract discover --target sha256_v1.ctxdsl
# expect: no gap markers on the annotated clauses

# 3. Discharge check on the proper composition
mununu contract validate --target secure_boot.ctxdsl
# expect: discharge: acyclic

# 4. Discharge check on the broken variant
mununu contract validate --target broken_circular.ctxdsl
# expect: discharge: circular reasoning required at {...}

# 5. Verification of the three properties
mununu context eval secure_boot.ctxdsl --formula no_execution_without_signature
# expect: SAT (safety holds under chaotic crypto)
mununu context eval secure_boot.ctxdsl --formula keys_not_on_bus
# expect: SAT (safety holds)
mununu context eval secure_boot.ctxdsl --formula valid_firmware_boots
# expect: UNSAT under chaotic crypto; SAT after vendor liveness annotation
```

The transcript is the evidence. Per the [CLAUDE.md](../../CLAUDE.md) claims integrity rules, this transcript must be reproducible by anyone running `validate.sh` against the pinned commit; no hand-typed expected outputs.

### 9.6 What the example does *not* claim

- It does **not** claim mununu found a vulnerability in any commercial secure-boot ROM. The vendor-IP black boxes are stylised; the boot controller is hand-authored for the demonstration.
- It does **not** claim the closed-IP contracts are accurate to real-world SHA-256 or RSA implementations. They are illustrative of the *contract shape*, not derived from any specific vendor's datasheet.
- It does **not** prove that any real device using this architecture is secure. The proof is conditional on the contracts; the contracts are conditional on the vendor honouring its datasheet.

These honesty caveats are baked into the example's README per the claims-integrity rules.

## 10. Publication plan

After §9 is implemented and validated (i.e. the transcript reproduces cleanly), two derivative artefacts publish the result. **The Substack and LinkedIn drafts are not written until the example transcript is reproducible.**

### 10.1 Substack post — technical deep dive

**Working title:** "How a model checker handles closed-IP modules: a secure boot walkthrough"

**Audience:** formal-methods practitioners, hardware verification engineers, security researchers.

**Structure:**
1. The problem — black-box modules in real designs.
2. Two encodings, one pipeline — the §2 discovery view.
3. Walking through the secure-boot example end-to-end, with the actual transcript embedded.
4. The discharge check — what circular reasoning looks like and why mununu refuses to silently accept it.
5. Honest caveats — what the example does and does not claim (§9.6).
6. Where this lives in the literature — Pnueli, Interface Automata, OVL, JasperGold cutpoints.
7. What's next — pointer to the next document.

**Length target:** 2500–3500 words. One transcript block, two diagrams, no marketing language.

### 10.2 LinkedIn post — executive summary

**Working title:** "Verifying secure boot when you can't see inside the crypto blocks"

**Audience:** hiring managers, semiconductor / formal verification leadership, technical decision makers.

**Structure:**
- Two-sentence problem statement.
- Three-sentence what-we-did summary, citing the actual property verified.
- One-sentence callout to the honest caveat (chaotic-stub vs. vendor-contract distinction).
- Link to the Substack deep dive.
- Link to the public example directory.

**Length target:** 150–200 words. No images beyond a single diagram of the boot architecture.

### 10.3 Validation gate before publication

Before the Substack or LinkedIn posts go live, the following must be true:

1. `examples/industrial/secure_boot_rom/validate.sh` exits 0 against the pinned commit.
2. The transcript referenced in the post matches the transcript the script produces (byte-for-byte for verdict lines).
3. The claims integrity checklist from [CLAUDE.md](../../CLAUDE.md) is signed off by the author: no claims that the example finds bugs in real systems; no severity inflation; all abstractions documented.
4. A second reviewer (human or `review-orchestrator` agent) has read the post and confirmed the §9.6 caveats are not buried.

The publication only proceeds after all four gates pass.

## 11. What comes next

When this document is marked **implemented** (tasks A1–A5 + A8 landed; A6 and A7 are now owned by Document D's M3), **validated** (the §9 example transcript is reproducible), and **published** (the §10 Substack + LinkedIn posts are live), the next document to tackle is:

→ **[Document B — RTL frontend unification](rtl-frontend-unification.md)** and its accompanying implementation plan.

Document B is independent of Document A's industrial example but anchors on Document A's controllability rule (§4) and chaotic-stub recommendation (§2). Tackling B next gives mununu's RTL pipelines a consistent contract story before the codesign use case (Document C) tries to compose across them.

The full roadmap order: **A → B → D → C → governance update**. Document D's M3 picks up the deferred A6 (corpus-driven discovery phase 2) and A7 (HITL stage-4 UX) alongside D's own corpus + sidecar work. See the planning file at `.claude/plans/i-want-you-to-distributed-orbit.md` for the milestone breakdown.

---

## References

**Compositional proof systems and the framing.**
- W.-P. de Roever, H. Langmaack, A. Pnueli (eds.), *Compositionality: The Significant Difference*, COMPOS '97, LNCS 1536. [Springer](https://link.springer.com/book/10.1007/3-540-49213-5).
- W.-P. de Roever et al., *Concurrency Verification: Introduction to Compositional and Noncompositional Methods*, Cambridge University Press, 2001.

**Assume-guarantee origins.**
- J. Misra, K. M. Chandy, "Proofs of Networks of Processes," IEEE TSE 7(4), 1981.
- C. B. Jones, "Specification and Design of (Parallel) Programs," IFIP 1983.
- A. Pnueli, "In Transition from Global to Modular Temporal Reasoning about Programs," NATO ASI Series 13, Springer 1985. [Springer](https://link.springer.com/chapter/10.1007/978-3-642-82453-1_5).
- M. Abadi, L. Lamport, "Conjoining Specifications," ACM TOPLAS 17(3), 1995.
- K. L. McMillan, "Circular Compositional Reasoning about Liveness," CHARME '99.

**Open-systems / module checking.**
- O. Kupferman, M. Y. Vardi, P. Wolper, "Module Checking," CAV '96; journal version J. ACM 47(2), 2000. [J. ACM](https://dl.acm.org/doi/10.1145/333979.333987).
- O. Kupferman, M. Y. Vardi, "Verification of Open Systems," LNCS 1346, 1997. [Springer](https://link.springer.com/chapter/10.1007/BFb0058035).

**Interface theories.**
- L. de Alfaro, T. A. Henzinger, "Interface Automata," ESEC/FSE 2001. [ACM](https://dl.acm.org/doi/10.1145/503271.503226).
- T. A. Henzinger, S. Qadeer, S. K. Rajamani, "You Assume, We Guarantee: Methodology and Case Studies," CAV '98.

**Reactive Modules and the controllability rule.**
- R. Alur, T. A. Henzinger, "Reactive Modules," Formal Methods in System Design 15(1), 1999. [Springer](https://link.springer.com/article/10.1023/A:1008739929481).

**Learning-based assumption inference.**
- J. M. Cobleigh, D. Giannakopoulou, C. S. Păsăreanu, "Learning Assumptions for Compositional Verification," TACAS '03, LNCS 2619. [Springer](https://link.springer.com/chapter/10.1007/3-540-36577-X_24).

**Software thread-modular verification.**
- C. Flanagan, S. Qadeer, "Thread-Modular Model Checking," SPIN '03.
- S. Chaki, E. Clarke, A. Groce, S. Jha, H. Veith, "Modular Verification of Software Components in C," IEEE TSE 30(6), 2004.

**Industrial precedent.**
- Cadence JasperGold `cutpoint -blackbox` / `set_blackbox` — covered in [LUBIS-EDA: Formal Abstraction Methodologies](https://lubis-eda.com/formal-verification-abstraction-methodologies/).
- yosys `(* blackbox *)` attribute and `hierarchy` command — [yosys docs](https://yosyshq.readthedocs.io/projects/yosys/en/stable/cmd/index_passes_hierarchy.html).
- SymbiYosys (sby) formal flows — [github.com/YosysHQ/sby](https://github.com/YosysHQ/sby).
- OpenTitan formal verification — [github.com/lowRISC/opentitan/hw/formal](https://github.com/lowRISC/opentitan/blob/master/hw/formal/README.md).
