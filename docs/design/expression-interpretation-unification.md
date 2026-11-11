# Expression-interpretation unification

> **Status: planning** — architecture analysis + staged proposal for unifying mununu's BTOR2/SV operator-interpretation layer. Code anchors are current-state references; the trait seam this doc proposes does not exist yet. Graduates to live `Source of truth` anchors as each move lands.

> **Concept:** This doc reasons about *expression interpretation* (BTOR2 operator → some value/term), a layer distinct from *formula evaluation* (mu-calculus formula → verdict). The two are different axes and must not be conflated — the verdict evaluator already has its own tagless-final abstraction (the bulk `EvalDomain` trait, P2.2/P2.3); this doc is about the expression layer that feeds the lift.

## Problem

mununu interprets the same BTOR2 operator set in **five places** that grew independently. Adding a new operator (or a new abstraction like memory) today means touching several of them by hand, and the abstractions (bit-blast / BV / UF / array / memory) are scattered across concrete-eval code, two Z3 walks, a subprocess SMT-LIB renderer, and a concrete-representative substitution hack.

### Current interpreter inventory

| # | Site | Input grammar | Value type | Powers | Ops |
|---|---|---|---|---|---|
| 1 | [`bit_blast.rs::eval_op`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs) (~:3960) | BTOR2 AST | concrete `BvValue` | explicit-state enum **+ predicate-cube sampling** | 43 |
| 2 | [`predicate_image/btor2_encode.rs::encode_op`](../../crates/mununu-core/src/adapter/sidecar/predicate_image/btor2_encode.rs) (~:356) | BTOR2 AST | `z3::ast::BV` | must-edge SMT (`smt_must_edge.rs`) + value enumeration | 38 |
| 3 | [`systemverilog/kripke_smt.rs::expr_to_z3`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs) (~:559) | **SystemVerilog** `Expr` AST | `z3::ast::BV` | RTL value discovery | 18 |
| 4 | ~~`cvc5/mod.rs::build_interpolation_query`~~ — removed 2026-11-11 (mununu#977); the transition-aware query in [`cegar.rs::craig_refinement_over_transition`](../../crates/mununu-core/src/adapter/btor2/cegar.rs) replaced it | predicate cubes only | SMT-LIB text | Craig interpolation | n/a |
| 5 | "UF wrapping" ([`bit_blast.rs` ~:3804](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs)) | — | — | **not an interpreter** — substitutes concrete Zero/Ones into #1 | — |

### Three facts that frame the solution

1. **The AST is already unified and array-capable.** [`Sort::Array`](../../crates/mununu-core/src/adapter/btor2/ast.rs), `Op::Read`, `Op::Write` all exist (ast.rs ~:48, ~:137). The syntax layer needs no change.
2. **#1 and #2 are the same walk over the same AST**, differing only in the `Value` type and the per-op primitive (`wrapping_add` vs `bvadd`; `concat` vs `concat`; `extract` vs `extract`). Every new operator is implemented twice today. The other independent walk (#3) is over a *different* grammar (SV `Expr`, not BTOR2), so it's genuinely separate.
3. **mununu already embraces tagless-final.** [`EvalDomain`](../../crates/mununu-core/src/mu_calculus/evaluator.rs) abstracts the verdict evaluator over a whole-state-set `Valuation` type with `BoolDom` / `KleeneDom` implementors (unified P2.2/P2.3). The expression layer should mirror that idiom.

## Where each concept belongs

```
AST (btor2/ast.rs) ── already unified, array-capable ── UNCHANGED
        │
   trait BvTermBackend { type Value; fn op_add/mul/read/write/… }   ← the seam
   one generic  walk_design::<B: BvTermBackend>()  replaces eval_op + encode_op
        │
   ┌────┴───────────────┬──────────────────────┐
   ▼                    ▼                       ▼
Concrete            Z3Backend               SmtLibBackend
Value = BvValue     Value = Dynamic(BV∪Array) Value = SMT-LIB text
fast; no arrays/UF  sound; UF + Array (QF_AUFBV) portable; cvc5 / bitwuzla / …
        │                    │                     │
        └──── feed ──→ predicate_cube_lift / bit_blast / must-edge / interpolant
                                   │
                                   ▼  CLTS / KMTS
                       mu_calculus evaluator (EvalDomain)  ← SEPARATE axis, untouched
```

- **bit-blast** = the `Concrete` instantiation of the seam. `eval_op` becomes its `impl`; stays where it is.
- **BV** = the shared operator set — the trait's method signatures. One place.
- **UF** = *not a path and not a backend*; a **capability** ("interpret this op as uninterpreted"). The concrete backend approximates it with Zero/Ones representatives (the cheap may-side path — keep it); the symbolic backends provide *real* UF.
- **array / memory** = a **capability** of the symbolic backends (`select` / `store`, logic `QF_AUFBV`). The concrete backend can only bounded-enumerate it (that is what stage 4 bounded-bit-blast is).
- **z3 terms** = the `Z3Backend::Value` type. One place, hosting BV ∪ Array ∪ uninterpreted `FuncDecl`.

The **mu-calculus evaluator is a different axis** — it consumes the lifted KMTS and already has `EvalDomain`. The expression-interpretation unification must not touch it.

## Working with z3 / cvc5

- **z3 is the right home for UF + array.** In-process, already the must-edge workhorse, native Array + uninterpreted-`FuncDecl` + quantifier support, and [`theory.rs`](../../crates/mununu-core/src/adapter/sidecar/predicate_image/theory.rs) already declares `QF_AUFBV` (~:44). Memory abstraction = extend `encode_op` with `Op::Read => select`, `Op::Write => store`. The must-edge SMT path inherits arrays transitively because `smt_must_edge.rs` consumes the encoded transition view.
- **cvc5 stays specialized for interpolation** (its unique capability). Its SMT-LIB renderer is already solver-agnostic standard syntax; only the response parser is cvc5-specific. The latent win: factor an `SmtLibBackend` whose *rendering* both cvc5 and a future subprocess solver share. Lower priority than the z3 array work.
- Do not force z3 + cvc5 into one backend. Let them share the **trait** (query construction) while keeping distinct transports (in-process vs subprocess).

## The three moves (increasing risk)

### Move A — EXPAND z3 to `QF_AUFBV` (this is stage 3.c; smallest, do now)

Extend [`btor2_encode.rs::encode_op`](../../crates/mununu-core/src/adapter/sidecar/predicate_image/btor2_encode.rs) with two arms:

- `Op::Read(arr, idx)` → `z3::ast::Array::select(&arr, &idx)`
- `Op::Write(arr, idx, val)` → `z3::ast::Array::store(&arr, &idx, &val)`

Declare array-sorted state cells as `z3::ast::Array` (index/element widths from `Sort::Array`). The per-cell map widens from `HashMap<Nid, BV>` to a small `enum Z3Term { Bv(BV), Arr(Array) }`. Functional-consistency axioms come free from Z3's Array theory — no manual axiom encoding. `theory.rs` already has the `QF_AUFBV` logic string; flip the selector when any array cell is present.

**This is one encoder file** — not the three sites the earlier `phase10-uf-routing.md` stage-3.c draft named. `kripke_smt.rs` is SV-grammar (wrong tree); `evaluate_pure` is concrete (can't hold a symbolic array). See the stage-3.c correction in `phase10-uf-routing.md`.

### Move B — MERGE #1 + #2 behind `BvTermBackend` (high value, own track)

```rust
// crates/mununu-core/src/adapter/btor2/term_backend.rs  (NEW — proposed)
trait BvTermBackend {
    type Value;
    type Error;
    fn const_bv(&mut self, bits: u128, width: u32) -> Self::Value;
    fn op_add(&mut self, a: &Self::Value, b: &Self::Value) -> Self::Value;
    fn op_mul(&mut self, a: &Self::Value, b: &Self::Value) -> Self::Value;
    fn op_read(&mut self, arr: &Self::Value, idx: &Self::Value) -> Self::Value;   // array capability
    fn op_write(&mut self, arr: &Self::Value, idx: &Self::Value, v: &Self::Value) -> Self::Value;
    fn uf_apply(&mut self, op_nid: Nid, width: u32, args: &[Self::Value]) -> Self::Value; // UF capability
    // … one method per operator family
}
struct Concrete;  // Value = BvValue       (eval_op today; no arrays/UF — bounded only)
struct Z3Backend; // Value = Z3Term        (encode_op today; arrays + real UF)
```

One generic `walk_design::<B: BvTermBackend>(file, &mut backend)` replaces the dual maintenance. Mirror `EvalDomain`. Heavily test-pinned (both paths have extensive suites). ~5–8 sessions. **Own design-doc-gated track — not smuggled into slot 4.**

### Move C — RECLASSIFY UF as a backend capability (closes a soundness gap)

Today the must-edge SMT path "does not currently handle UF" (the sampling-convergence heuristic stands in — see the `smt_must_edge.rs` R.2.5b docstring). Real UF (`z3::FuncDecl`) in `Z3Backend::uf_apply` makes must-edges sound under UF. Keep the concrete Zero/Ones substitution as the cheap may-side approximation. Rides on Move B's seam. ~2–3 sessions after B.

## The composition that makes Move A clean

The predicate-cube lift computes **may-edges via concrete sampling** ([`kmts_lift.rs` ~:1710](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs), which calls `simulate_one_step` → `eval_op`, concrete, **can't do arrays**) and **must-edges via the SMT path** (`btor2_encode` → `smt_must_edge`, **can**). So for a UF memory:

- **may-side = havoc** — reuse the shipped stage-1b machinery (`Op::Read` → fresh nondeterministic input; sound over-approximation).
- **must-side = z3 array theory** — Move A's `select`/`store` encoding; precise.

Havoc-may + array-must is a proper KMTS: loose-but-sound may, tight must. It **composes existing code** (stage-1b havoc + a one-file encoder extension) rather than requiring a symbolic-array *concrete* evaluator. This is materially simpler than a single symbolic path, and it is the recommended stage-3.c shape.

## Staging + risk

| Move | Scope | Risk | When |
|---|---|---|---|
| A (arrays in `btor2_encode`, havoc-may/array-must) | 1 encoder file + lift wiring | medium (soundness-critical encoder) | **now — stage 3.c** |
| B (`BvTermBackend` merge of #1 + #2) | new trait + generic driver + 2 impls | high (load-bearing refactor) | own track, **after slot 4 closes** |
| C (real UF in z3 backend) | `uf_apply` impl + must-edge wiring | medium (rides on B) | after B |

Do **not** start B/C mid-slot-4. Stage 3.b's dispatch (UF → `predicate_cube_lift`) is already correct and unaffected by B/C; only stage 3.c's *internals* are reshaped by Move A.

## Acceptance criteria for "design satisfied"

- [x] Interpreter inventory enumerated with file:line.
- [x] Layer mapping (where bit-blast / BV / UF / array / z3-terms belong) specified.
- [x] z3-vs-cvc5 division-of-labor stated (z3 hosts UF+array; cvc5 stays interpolation; factor SMT-LIB rendering).
- [x] Three moves specified with risk + sequencing.
- [x] The havoc-may/array-must composition documented as the stage-3.c shape.
- [x] `phase10-uf-routing.md` stage-3.c section corrected (separate edit).
- [x] Doc tagged `> Status: planning` + `> Concept:` per CLAUDE.md §Documentation Traceability.

## Cross-references

- §Phase 10 UF routing (stage 3.b dispatch + corrected stage 3.c): [`phase10-uf-routing.md`](phase10-uf-routing.md)
- §Phase 10 fixture selection (Caliptra mbox): `Phase10-fixture-selection-caliptra-mbox-2026-06-12.md` (a planning note, not tracked)
- Verdict-evaluator tagless-final precedent: [`evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs) (the bulk `EvalDomain` trait + `BoolDom` / `KleeneDom`)
- §Phase 11 master roadmap: `you-are-a-formal-vast-lake.md` (a planning note, not tracked)
