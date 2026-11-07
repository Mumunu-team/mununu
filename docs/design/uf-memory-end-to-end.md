# UF-memory end-to-end: `BvTermBackend` unification with memory as a capability

> **Status: planning** — the decided post-stage-5 path for UF-memory verification. After §Phase 10 stage 5 ships the Caliptra-mbox black-box-interface milestone (the independent near-term deliverable), the internal-memory verification track goes via **Option 4: the full `BvTermBackend` unification** from [`expression-interpretation-unification.md`](expression-interpretation-unification.md) §"Move B", with memory carried as a **backend capability** rather than a special-cased path. This doc is the implementation plan for that work.

> **Concept:** lifting a BTOR2 design with an internal memory cell to a 3-valued KMTS the modal-mu evaluator can verify, by unifying the concrete and SMT operator interpreters behind one trait and expressing memory as a capability each backend provides in its own theory.

## Decision

The §Phase 10 stage-3.c fork (2026-06-12) deferred end-to-end UF-memory verification. Of the four options surveyed (below, §"Why Option 4"), the chosen path is **Option 4 — the `BvTermBackend` unification**, sequenced **after stage 5**:

1. **Stage 5 first (independent, ships the milestone):** Caliptra mbox's external SRAM via the black-box contract path — see `Phase10-stage5-caliptra-mbox-blackbox-2026-06-12.md` (a planning note, not tracked). This is unrelated to the interpreter unification and must not be blocked by it.
2. **Then Option 4 (its own track):** merge the concrete (`eval_op`) and SMT (`encode_op`) interpreters behind `BvTermBackend`; memory becomes a capability each backend implements. The already-shipped Z3 array encoder (`9c5fa09` + `3600cba`) is reframed as the `Z3Backend`'s array methods. UF migrates from the concrete Zero/Ones representative hack to real `z3::FuncDecl`.

The internal-memory fixture for the Option-4 milestone is **ibex `ibex_register_file_ff`** (the natural internal-`$mem` design), not Caliptra mbox (external SRAM, handled by stage 5).

## The problem (grounding)

The predicate-cube lift ([`kmts_lift.rs::predicate_cube_lift`](../../crates/mununu-core/src/adapter/btor2/kmts_lift.rs)) builds a KMTS over `2^|P|` predicate-cube states, computing two relations between cubes:

- **may-edges** (`R_may`) — sound **over-approximation** (some concrete state in `b` reaches some in `b'`). Sound for safety.
- **must-edges** (`R_must ⊆ R_may`) — sound **under-approximation / witness** (the concrete relation forces it). Needed for liveness, alternating fixpoints, and **read-after-write** (`read(addr)` *must* equal the last write).

A 3-valued (KMTS) verdict needs both (Bruns–Godefroid: definite verdicts transfer iff may over-approximates *and* must under-approximates the same concrete relation).

Today the two relations come from **two different mechanisms**, and only one is memory-capable:

| Relation | Mechanism | Memory? |
|---|---|---|
| may | concrete sampling — [`simulate_one_step`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs) → `eval_op` over `BvValue` | **No** — `eval_op` errors on `Op::Read`/`Op::Write` |
| must | SMT — `encode_design_for_lift` → Z3 array theory | **Yes** (since `9c5fa09`) |

The **may side cannot evaluate an array operation** — concrete `BvValue` has no array type. That is the open problem. Today the two relations are written as two hand-maintained interpreters of the same operator set; memory exposes that this duplication has *diverged in capability* (one speaks arrays, the other does not).

## Why Option 4 (over the interim options)

Three narrower options exist; Option 4 subsumes or supersedes each:

- **Option 1 — two-design threading (havoc-may + array-must).** Thread a havoc-rewritten file for may-sampling + the original array file for the must-SMT. *Works, medium risk, but* hard-codes the two-mechanism split it is trying to bridge: two design representations in flight, a consistency invariant to police (havoc must touch only array NIDs), and a may side that stays *havoc-loose* (read-after-write's may admits the violation, so the verdict leans entirely on must-promotion). **Under Option 4 this is expressed without two files:** may = run the `Concrete` backend with the memory capability set to havoc/bounded; must = run the `Z3Backend` with the array capability. Same AST, same driver, different backend — no second design.
- **Option 2 — symbolic SMT may-image.** Replace concrete may-sampling with an existential SMT image (one encoder, exact may). Cleaner than Option 1 *but* rips out the load-bearing R.2.5 sampling mechanism wholesale. **Under Option 4 this becomes a backend choice, not a rewrite:** an `Smt`-flavoured may pass is "run the may query against the `Z3Backend`" — opt-in, side-by-side with the concrete `Concrete`-backend sampling, not a replacement.
- **Option 3 — bounded bit-blast small memories.** Enumerate a tiny array as `N` bit-vector cells (no SMT, no havoc). **This is the planned stage 4 and ships independently** as `MemoryAbstraction::BoundedBitBlast`. Under Option 4 it is simply the `Concrete` backend's array capability for small `N` — the bounded enumeration becomes one of the concrete backend's memory strategies. Stage 4 ships first; Option 4 later absorbs it as a capability.

**Why Option 4 is the right end state:** the may/must tension is *caused* by maintaining two interpreters that have drifted in capability. Unifying them behind one trait + driver means memory is implemented **once per backend, in that backend's natural theory**, and the may/must choice becomes "which backend you run over the one shared AST" — the tension dissolves structurally rather than being worked around per-fixture. It also kills the dual-maintenance burden (every operator is implemented twice today) and gives a clean home for future backends (CVC5 SMT-LIB, abstract-interval).

Cost: it is the largest change — a refactor of two soundness-critical, heavily-tested interpreters. That is exactly why it is sequenced **after** stage 5 (which carries the milestone narrative on its own) and run as its own gated track.

## Option 4 — the plan

### The seam

```rust
// crates/mununu-core/src/adapter/btor2/term_backend.rs  (NEW)
trait BvTermBackend {
    type Value;                  // Concrete: BvValue;  Z3: Z3Term { Bv(BV), Arr(Array) }
    type Error;
    // bit-vector capability (every backend)
    fn const_bv(&mut self, bits: u128, width: u32) -> Self::Value;
    fn op_add(&mut self, a: &Self::Value, b: &Self::Value) -> Result<Self::Value, Self::Error>;
    fn op_mul(&mut self, a: &Self::Value, b: &Self::Value) -> Result<Self::Value, Self::Error>;
    // … one method per operator family (≈ the 40-op set both interpreters already share)
    // memory capability — each backend implements in its own theory
    fn array_decl(&mut self, nid: Nid, idx_w: u32, elem_w: u32) -> Self::Value;
    fn op_read(&mut self, arr: &Self::Value, idx: &Self::Value) -> Result<Self::Value, Self::Error>;
    fn op_write(&mut self, arr: &Self::Value, idx: &Self::Value, v: &Self::Value) -> Result<Self::Value, Self::Error>;
    // UF capability — real uninterpreted application (Z3: FuncDecl; Concrete: representative)
    fn uf_apply(&mut self, op_nid: Nid, width: u32, args: &[Self::Value]) -> Self::Value;
}
```

One generic driver walks the BTOR2 DAG once:

```rust
fn walk_design<B: BvTermBackend>(file: &Btor2File, backend: &mut B) -> Result<Step<B::Value>, B::Error>;
```

This replaces `eval_op` (concrete) and `encode_op` (SMT) — currently ~40 operator arms maintained twice.

### The backends

- **`Concrete` (`Value = BvValue`).** The current `eval_op` becomes its `impl`. Memory capability: `op_read`/`op_write` via **bounded enumeration** (small `N`, the stage-4 mode) or **havoc** (`op_read` → fresh nondeterministic value) for the may-side over-approximation. No symbolic arrays — concrete can't hold them, and doesn't need to.
- **`Z3Backend` (`Value = Z3Term { Bv(BV), Arr(Array) }`).** The current `encode_op` + the **already-shipped** array encoder (`btor2_encode.rs`'s `select`/`store`/array-decl) become its `impl`. Memory capability: Z3 array theory (exact). UF capability: real `z3::FuncDecl` (replacing the concrete Zero/Ones representative — closes the must-edge-under-UF soundness gap noted in `smt_must_edge.rs`).
- **(future) `SmtLibBackend` (`Value = text`).** cvc5 / bitwuzla via SMT-LIB rendering — folds in the existing cvc5 interpolation path as a third backend variant. Out of the initial Option-4 scope.

### How the may/must tension dissolves

`predicate_cube_lift` runs the **same driver** twice over the **same AST**:

- **may-edges:** `walk_design::<Concrete>(file, &mut concrete_backend)` with the concrete backend's memory capability set to havoc (loose, sound) or bounded (exact for small `N`). No havoc-rewritten *file* — the havoc lives in the backend's `op_read`, not in a second IR.
- **must-edges:** `walk_design::<Z3Backend>(file, &mut z3_backend)` with array theory. The must-edge SMT post-passes already route here via `encode_design_for_lift` (shipped `3600cba`) — they become "run the Z3Backend."

Memory is implemented **once per backend**; the lift picks the backend per relation. `R_must ⊆ R_concrete ⊆ R_may` holds by construction: concrete-havoc over-approximates reads (may), Z3-array is exact (must).

### Staging (each step ships green + test-pinned)

1. **Extract `BvTermBackend` + `walk_design`; port `Concrete` first.** Prove identical: the entire existing bit-blast + R.2.5 sampling suite passes against the driver bit-for-bit. ~2–3 sessions. No behavior change.
2. **Port `Z3Backend` (encode_op + the shipped array encoder).** Prove identical: the must-edge + all-SMT suites pass against the driver. The array methods are the `9c5fa09` code, now expressed as trait methods. ~2 sessions.
3. **Wire `predicate_cube_lift` to the backend choice** — may via `Concrete`, must via `Z3Backend`. The two-mechanism split becomes the two-backend choice; the memory tension is resolved. ~1–2 sessions.
4. **Real UF in `Z3Backend::uf_apply`** (`FuncDecl`), retiring the concrete representative as the may-side approximation only. Closes the must-under-UF soundness gap. ~2 sessions.
5. **ibex regfile milestone:** end-to-end read-after-write on `ibex_register_file_ff` (4×4-bit instance), verdict `KleeneT`, SBY-cross-checked. ~1 session + fixture.

Total Option-4 track: ~8–10 sessions. Sequenced after stage 5; own design-doc-gated track.

## Soundness

- **The merge is soundness-neutral** *iff* every operator's concrete and SMT implementations were semantically identical pre-merge — which they are (same AST, arity, width propagation). The merge's risk is *discovering* a latent disagreement, which is a find, not a regression; the staging's "prove identical" gates surface it.
- **Memory capability soundness:** the invariant is `R_must ⊆ R_concrete ⊆ R_may`. The `Concrete` backend's havoc `op_read` is a sound over-approximation (a fresh value admits everything the concrete memory could hold). The `Z3Backend`'s array `op_read`/`op_write` is exact (Z3 extensional array axioms). The bounded-enumeration `Concrete` memory is exact for selected addresses + havoc for the rest (sound).
- **UF capability soundness:** real `FuncDecl` UF over-approximates on the may side (functional consistency only) and is *unsound* as a must witness — so `uf_apply` is may-only; the must side must use concrete/array operators. This is the existing R.5b asymmetry, now enforced at the backend boundary instead of by convention.

## Validation paths

- **Merge fidelity:** the union of the existing concrete (`bit_blast`, R.2.5 sampling) + SMT (`smt_must_edge`, `all_smt`, `btor2_encode`) test suites must pass against the unified driver, each backend producing its pre-merge result bit-for-bit. This is the load-bearing regression gate for steps 1–2.
- **Array correctness (shipped):** the read-after-write UNSAT proof on the `Z3Backend` array methods (`9c5fa09` test `bvufarray_read_after_write_is_forced_by_array_axiom`).
- **Monotonicity:** on a hand-built memory fixture, assert `R_must ⊆ R_may` after step 3 (every must-edge is a may-edge) — the KMTS well-formedness check.
- **End-to-end (the milestone):** read-after-write on ibex `ibex_register_file_ff` (4×4-bit), verdict `KleeneT`, cross-checked against an SBY bounded safety check at the same scale. A divergence from SBY is a soundness bug in the backend.
- **UF asymmetry:** a fixture with a UF-wrapped operator where the may side admits a behavior the must side must not claim — assert the must side does not promote it (the `uf_apply`-is-may-only enforcement).

## Cross-references

- Interpreter-layer map + the three unification moves (Option 4 = Move B): [`expression-interpretation-unification.md`](expression-interpretation-unification.md)
- Routing dispatch + corrected stage-3.c: [`phase10-uf-routing.md`](phase10-uf-routing.md)
- Stage-5 Option-B (Caliptra mbox black-box, ships first): `Phase10-stage5-caliptra-mbox-blackbox-2026-06-12.md` (a planning note, not tracked)
- Array encoder (shipped, becomes `Z3Backend` array methods): commits `9c5fa09`, `3600cba`
- Verdict-evaluator tagless-final precedent (the pattern Option 4 mirrors): [`evaluator.rs`](../../crates/mununu-core/src/mu_calculus/evaluator.rs) (the bulk `EvalDomain` trait + `BoolDom` / `KleeneDom`, unified P2.2/P2.3)
- KMTS soundness foundation: [`kmts-theory.md`](kmts-theory.md)
