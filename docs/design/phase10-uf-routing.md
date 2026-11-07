# §Phase 10 UF-mode routing architecture

> **Status: planning** — describes the chosen routing for §Phase 10 stages 3.b + 3.c implementation. Code anchors are pre-implementation references; the `Source of truth` anchors graduate to live citations once stage 3.b ships.

## Problem statement

§Phase 10 §10.2 stage 2 shipped a sidecar schema with four memory-abstraction modes: `havoc`, `uf`, `bit_blast`, `bounded_bit_blast`. Stages 1+1b shipped the recognition pipeline and the `havoc` rewriting. Stage 3.a (commit `46b15a3`) shipped the UF-mode recognition + a UF-specific is_blastable error hint. **Stages 3.b + 3.c remain.**

The naive reading of the §Phase 10 stage-3 plan ("BTOR2 rewriting + Z3 Array theory in `kripke_smt`") suggests UF rides the same `bit_blast::translate` path that `havoc` rides today. **That reading is wrong.** The bit-blaster fundamentally cannot process Read/Write on array-sorted operands — it enumerates concrete state cubes (`combo: usize` indexing each state-cell's value space). An array-sorted state cell has no finite-width enumeration, so any combo-indexed enumeration over an array is undefined. The havoc rewriting sidesteps this by deleting the array entirely (Read → Input, Write/Init/Next dropped). UF mode must preserve the array semantics — havoc-style deletion would defeat the abstraction's purpose.

The right routing is to dispatch UF-declared memories to the **predicate-cube lift** path ([`predicate_cube_lift`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs)), which already operates symbolically (R.5b UF wrapping at [`kmts_lift.rs:646`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L646)). The bit-blaster stays out of the UF-mode loop entirely.

## Routing decision

When `SvAnnotation::memories[].abstraction == Uf` for at least one memory, the BTOR2 adapter routes through `predicate_cube_lift` instead of `bit_blast::translate`. The dispatch decision is made in [`crate::adapter::btor2::Btor2Adapter::translate`](../../crates/mununu-core/src/adapter/btor2/mod.rs) (or its caller in the API / CLI surface).

```text
┌─ BTOR2 input ──────────────────────────────────────┐
│                                                    │
│      sidecar: memories[].abstraction == Uf ?       │
│                                                    │
│                 yes                       no       │
│                  │                         │       │
│                  ▼                         ▼       │
│      predicate_cube_lift             bit_blast::translate
│      (kmts_lift.rs)                  (bit_blast.rs)
│                                                    │
│      ─ accepts UF arrays            ─ array-free
│      ─ R.5b UF wrapping             ─ havoc/no-mem only
│      ─ Z3 predicate-image           ─ explicit-state enum
│      ─ stage 3.c extends            ─ stage 1b shipped
└────────────────────────────────────────────────────┘
```

### Why the predicate-cube path, not an extended bit-blaster

- **R.5b UF wrapping infrastructure is already there.** [`kmts_lift.rs::collect_uf_wrapped_nids`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs) reads the sidecar's `uf_wrap` list and walks them through the predicate-image SMT queries. Extending the same map to carry array sorts (in addition to bit-vector sorts) is the "narrow extension" the §Phase 10 stage-3 plan called for — it matches `evaluate_pure`'s existing UF-substitution shape ([`kmts_lift.rs:1242`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs#L1242)).
- **The predicate-cube state space is symbolic.** Each cube is a conjunction of predicate equalities `{p_1, ¬p_2, p_3, ...}`; there is no per-cell concrete enumeration. Array-typed cells coexist with bit-vector cells naturally; the predicate-image SMT query handles all sorts uniformly.
- **`kripke_smt::discover_significant_values`** ([`systemverilog/kripke_smt.rs:20`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs#L20)) already operates at the SMT layer. Extending it to emit Z3 Array `select`/`store` for memory-typed registers is a localized addition — no bit-blaster surgery required.
- **Future cohesion.** Once stage 3.b + 3.c ship, the routing decision is: small fixtures with no memory → bit-blast (cheap); fixtures with memory → predicate-cube lift. The two paths stay disjoint per the §Phase 10 plan's "no parallel pipelines" discipline.

### Why NOT the obvious alternatives

- **Loosen `is_blastable` for UF Read/Write.** Would let the IR through but the downstream bit-blast enumeration would fail with a less specific error than today's (which explicitly names stages 3+4). Worse user experience; no architectural gain.
- **Extend the bit-blaster to support arrays.** Would require symbolic state representation everywhere a state cell is touched — `CellEnumeration::value_at`, `make_step_env`, `compute_next_state`, `build_state_valuations`, every transition relation construction site. Massive refactor that duplicates work the predicate-cube lift already does.
- **Skip the BTOR2 lifter entirely for UF; route to AVR.** Per the pillow plan's existing architecture, AVR is the external symbolic engine for designs beyond the explicit-state cap; it does NOT consume `SvAnnotation::memories` schema. Routing UF to AVR loses the sidecar-declared abstraction discipline. Mononu's in-process path (predicate-cube lift) preserves it.

## Code-level changes (stage 3.b + 3.c)

### Stage 3.b — routing dispatch (~1 session)

**File:** `crates/mununu-core/src/adapter/btor2/mod.rs` (the entry-point for BTOR2 adapter dispatch). New helper:

```rust
/// §Phase 10 stage 3.b — decide which lift path handles this BTOR2
/// + sidecar combination. UF-declared memories route through
/// `predicate_cube_lift`; everything else (no memories, only havoc,
/// only bit-blastable) routes through `bit_blast::translate`.
fn requires_uf_routing(content: &str, options: &AdapterOptions) -> bool {
    // Parse the BTOR2 to detect any memory cells.
    let file = match parser::parse(content) { Ok(f) => f, Err(_) => return false };
    let memory_cells = bit_blast::detect_btor2_memories(&file);
    if memory_cells.is_empty() {
        return false;
    }
    // Sidecar must declare at least one of them with `abstraction: uf`.
    let uf_nids = bit_blast::sidecar_uf_memory_nids(&memory_cells, options);
    !uf_nids.is_empty()
}

impl FormatAdapter for Btor2Adapter {
    fn translate(content: &str, options: &AdapterOptions)
        -> Result<AdapterOutput, AdapterError>
    {
        if requires_uf_routing(content, options) {
            return uf_lift_to_adapter_output(content, options);
        }
        bit_blast::translate_to_adapter_output(content, options)
    }
}
```

Stage 3.b's commit ships the routing decision + a stub `uf_lift_to_adapter_output` that calls `predicate_cube_lift` with the sidecar's predicate set (empty for now — stage 3.c populates) + returns an `AdapterError` for the array-handling parts. The dispatch is correct; the destination function is partial. Integration tests pin the dispatch.

The existing `sidecar_uf_memory_nids` helper (shipped at `46b15a3`) becomes `pub(crate)` so the routing dispatcher in `mod.rs` can call it. The is_blastable error path stage 3.a installed stays in place as a defensive fallback — if dispatch fails to fire (e.g. malformed sidecar), the bit-blaster still emits the stage-3.a hint.

### Stage 3.c — Z3 Array theory in the BTOR2 SMT encoder (CORRECTED 2026-06-12)

> **Correction.** The original stage-3.c draft (above this line in git history) named `kripke_smt.rs::discover_significant_values` and `kmts_lift.rs::evaluate_pure` as the extension sites. **Both are wrong**, per the architecture map in [`expression-interpretation-unification.md`](expression-interpretation-unification.md):
> - `kripke_smt.rs` walks the **SystemVerilog `Expr` AST**, not BTOR2 — wrong tree entirely.
> - `evaluate_pure` is the **concrete** `BvValue` evaluator — it cannot hold a *symbolic* array term; it can only bounded-enumerate (that is stage 4, not stage 3.c).
>
> The real BTOR2→Z3 encoder is [`predicate_image/btor2_encode.rs::encode_op`](../../crates/mununu-core/src/adapter/sidecar/predicate_image/btor2_encode.rs) (~:356), which `smt_must_edge.rs` consumes for must-edge queries. [`theory.rs`](../../crates/mununu-core/src/adapter/sidecar/predicate_image/theory.rs) already declares the `QF_AUFBV` logic string (~:44). Stage 3.c is **one encoder file**, not three.

**The may/must composition (the key simplification).** The predicate-cube lift computes may-edges via the *concrete sampling* path ([`kmts_lift.rs` ~:1710](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs) → `simulate_one_step` → `eval_op`, which cannot evaluate Read/Write), and must-edges via the *SMT* path (`btor2_encode` → `smt_must_edge`, which can). For a UF memory:

- **may-side = havoc** — reuse the **already-shipped** stage-1b machinery (`Op::Read` → fresh nondeterministic input; `Write`/`Init`/`Next` dropped). Sound over-approximation.
- **must-side = z3 array theory** — the new `select`/`store` encoding in `btor2_encode.rs`. Precise.

Havoc-may + array-must is a proper KMTS — loose-but-sound may, tight must — that **composes existing code** instead of requiring a symbolic-array concrete evaluator.

**Stage 3.c file-level changes:**

1. **`predicate_image/btor2_encode.rs`** — extend `encode_op` (or the array-rejection sites at ~:21/:62/:166/:495) with:
   - `Op::Read(arr, idx)` → `z3::ast::Array::select(&arr, &idx)`
   - `Op::Write(arr, idx, val)` → `z3::ast::Array::store(&arr, &idx, &val)`
   - Declare array-sorted `Node::State` cells as `z3::ast::Array` (index/element widths from `Sort::Array`).
   - Widen the per-cell map from `HashMap<Nid, z3::ast::BV>` to `enum Z3Term { Bv(BV), Arr(Array) }`. (This is a localized `Z3Term` enum scoped to `btor2_encode.rs` — NOT the broad cross-module change the original draft implied.)
   - Functional-consistency axioms come free from Z3 Array theory — no manual axiom encoding.

2. **`predicate_image/theory.rs`** — flip the logic selector to `QF_AUFBV` (already present at ~:44) when any array-sorted cell is encoded.

3. **`kmts_lift.rs` predicate-cube lift** — for UF-declared memories: route the **may-edge** construction through the stage-1b havoc rewrite (reuse `havoc_rewrite_memories`) so the concrete sampling path sees a memory-free design, while the **must-edge** SMT post-pass (`SmtPerTarget` / `SmtPerTargetStandard` / `SmtHyperMust`) consumes the array-aware `encode_design` output. No `evaluate_pure` change — the concrete path never sees an array.

4. **`uf_lift_to_adapter_output`** (the stage-3.b stub in `btor2/mod.rs`) — populate with the `predicate_cube_lift` call, threading the sidecar's UF-memory declarations + forcing the must-edge SMT inference policy on (array-must requires the SMT post-pass).

**Deferred to the unification track (Move B/C), NOT stage 3.c:** the `BvTermBackend` trait that would merge the concrete (#1) and z3 (#2) interpreters, and real UF (`z3::FuncDecl`) replacing the concrete Zero/Ones representative. See [`expression-interpretation-unification.md`](expression-interpretation-unification.md) §"The three moves". Stage 3.c does Move A only.

## Open questions (track during stage 3.b + 3.c implementation)

1. **`Z3Term` enum vs separate Array path.** The bit-vector branch in `btor2_encode.rs::encode_op` is the encoder to extend (corrected — NOT `kripke_smt`). Adding an `Arr(z3::ast::Array)` variant to the per-cell map requires updating the term-handling at each `encode_op` site. The alternative is two parallel code paths (one for bit-vec-only state, one for mixed) — duplication cost vs uniform-term-handling cost. **Mitigation:** prototype the `Z3Term` enum scoped to `btor2_encode.rs` in stage 3.c first; fall back to parallel paths if the match coverage proves unwieldy. The eventual `BvTermBackend` unification (Move B, separate track) supersedes both.
2. **`selected_addresses` interaction.** The sidecar schema lets the user supplement UF mode with a `selected_addresses` list (concrete addresses kept exact). Stage 3.c's UF query needs to consult this list per memory and emit a `(select arr addr) == concrete_value` constraint for each selected address. The constraint encoding shape is straightforward; the open question is whether the constraint goes in the predicate-image query body or in a separate axiom set passed to the SMT solver.
3. **Multi-memory designs.** Caliptra mbox has ONE external SRAM. ibex regfile has ONE register file. Designs with N memory cells (e.g. a CPU with separate I-cache + D-cache) would have N Z3 array variables threaded through every step. Stage 3.c's `HashMap<Nid, Z3Term>` extension scales to this naturally, but the predicate-image SMT-query complexity scales O(N · cube_count) which may need explicit budget management.
4. **Reset semantics for UF arrays.** Today the bit-blaster handles `Init` by pinning state cells to their initial value at cycle 0. For UF arrays, the initial state of every address is undefined unless the sidecar declares an init policy (e.g. "all addresses init to 0"). Stage 3.c's lift either (a) treats every read at cycle 0 as nondeterministic (sound over-approximation), or (b) accepts a sidecar `init_value` for UF memories (more precise; needs schema extension). **Mitigation:** ship (a) in stage 3.c; defer (b) to a follow-up.
5. **State-cell width accounting.** `sum_widths` at [`bit_blast.rs:644`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs#L644) already skips array-sorted state cells, so the bit-blast cap-check ignores UF arrays. The predicate-cube lift's `max_cube_count` cap applies instead. **No accounting change required**; the existing array-skip logic in `sum_widths` is the right call.

## Effort budget (revised from precondition pass)

| Stage | Scope | Effort |
|---|---|---|
| Stage 3.b | Routing dispatch + stub `uf_lift_to_adapter_output` + integration test pinning the dispatch | ~3 days (1 session) |
| Stage 3.c | Z3 Array `select`/`store` in `btor2_encode.rs::encode_op` + `QF_AUFBV` selector in `theory.rs` + havoc-may/array-must wiring in `kmts_lift.rs` + `uf_lift_to_adapter_output` population + 6-8 unit tests | ~1 wk (3-4 sessions; smaller than the original ~1.5 wk estimate now that the encoder location is corrected to one file) |
| Stage 4 | Bounded bit-blast mode (mode-3 reading: enumerate `selected_addresses` as a per-address state-cell ladder) | ~3 days |
| Stage 5 | Caliptra mbox vendoring + sidecar build + SBY oracle + verdict polarity comparison | ~5 days |

**Slot 4 revised total:** ~3.5 wk (was ~2.5 wk in the precondition pass; +1 wk for the more conservative stage 3.b/3.c decomposition).

## Acceptance criteria for "design satisfied" (gate before stage 3.b code)

- [x] Routing decision documented (UF → predicate-cube lift; non-UF → bit-blast).
- [x] Rejected alternatives explicit (loosen is_blastable, extend bit-blaster, route to AVR — each refused with rationale).
- [x] Stage 3.b code-level changes specified at the file + function level.
- [x] Stage 3.c code-level changes specified with the `Z3Term` enum sketch.
- [x] Open questions enumerated with mitigation plans.
- [x] Effort budget revised (~3.5 wk total slot 4).
- [x] Doc tagged `> Status: planning` per CLAUDE.md §Documentation Traceability.

Stage 3.b implementation may begin once the user accepts this design.

## Cross-references

- §Phase 10 §10.1 fixture-selection: `Phase10-fixture-selection-caliptra-mbox-2026-06-12.md` (a planning note, not tracked)
- §Phase 10 §10.2 stage 1 detection: `Phase10-stage1-detection-2026-05-25.md` (a planning note, not tracked)
- §Phase 10 §10.2 stage 2 schema: `Phase10-stage2-schema-2026-05-25.md` (a planning note, not tracked)
- Stage 3.a recognition layer: commit `46b15a3`, CI run 27425450696 green.
- §Phase 11 master roadmap: `you-are-a-formal-vast-lake.md` (a planning note, not tracked) §11.4 slot 4.
