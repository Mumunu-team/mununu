# Consumer briefing — a black box's outputs are free at every step on every engine (soundness fix, 2026-10)

> Source of truth: [`nextless_states_free`](../../crates/mununu-core/src/adapter/btor2/parser.rs) — surface: (CLI+API)

**Audience:** every client of `sv verify-auto` (and `btor2 cegar`, the recoverability cube ladder) whose design has a black box or any other undriven cell the lift cuts to a free value: rosf, monono, mununu-ui. A verdict-semantics fix; changed verdicts move toward the correct answer.

## TL;DR

- **The defect.** yosys emits a black box's cut outputs (`$anyseq`) as BTOR2 `state` lines with no `next`, which BTOR2 defines as unconstrained at every step. The exact engine read them so. The predicate-cube engines (`explicit`, used by the default portfolio) stepped cube representatives through the simulator's `apply_next`, which keeps an absent `next`'s value, so they treated those outputs as **frozen**.
- **What it produced.** A **false HOLDS**: `assert property (!v |=> $stable(d))` over a black box's `v` and `d` came back HOLDS from the default portfolio (`decided_by: explicit`) while the exact engine skipped it, and every safety engine (exact, native BMC, SPACER, interpolation, btormc, pono, rIC3) finds the data changing between cycles on the very model verify-auto runs. Any property that holds only if a black box's outputs stay put was exposed.
- **The fix.** At the predicate-cube lift's entries, every next-less state gets a `next` from a fresh free input `<name>$next`: exactly the BTOR2 meaning, with any `init` still fixing cycle 0. A `$anyconst`, which keeps a `next` to itself, is untouched. The exact engine, which already read such a state as free, sees the model as before.
- **What the reproducer reports now.** `!v |=> $stable(d)` over a black box comes back ⊥ (undecided) instead of HOLDS: the cube engines no longer prove it, and no engine in the portfolio refutes this shape yet. A ⊥ is an honest "don't know"; the HOLDS was wrong.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a property over a black box's outputs that held only because the cube engines froze them now comes back ⊥ (or VIOLATED, when an engine refutes it) | re-run your lanes on this build; review any HOLDS → VIOLATED/⊥ row as the old verdict being wrong |
| **monono** | the same, on lanes with black-boxed modules | re-run `make formal-docker`; review any flipped row |
| **mununu-ui** | none (verdicts come from the backend) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes | verdict correctness |
| `mununu/docker/Dockerfile.dev` | yes | the UI's dev backend |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | yes | its scripts read verdicts |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction is unchanged |
| rosf `docker/*` | yes, at the next pin | verdict correctness |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes, at the next pin | verdict correctness |

## Verification steps for every consumer

```bash
# bb_top instantiates `(* blackbox *) module bb (output v, output [2:0] d)`; the bound SVA says
# `!v |=> $stable(d)`, which nothing in the design guarantees.
mununu sv verify-auto bb_top.sv --source bb_sva.sv --top bb_top --frontend slang --pin rst_n=1
#   a_stable_unless_valid: UNKNOWN/⊥ (HOLDS, decided_by explicit, before this fix)
```

## Provenance

- Found preparing the EmTech tutorial recordings (segment 3: a vendor receiver as a black box whose datasheet is checked under `--assumes check`; `m_data_changes_on_valid` held where nothing constrains the receiver). Reduced to the two-line black box above.
- Reproducer: `adapter::btor2::cegar::tests::a_state_with_no_next_is_free_in_the_cube_lift` (the cube lift's verdict for "once high, stays high" over a next-less cell: not True; True on the parent). Unit test: `adapter::btor2::parser::tests::a_nextless_state_gets_a_free_next`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md), [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- The simulator's own `apply_next` keeps an absent `next`'s value by design (a deterministic step needs one); every engine that needs BTOR2 semantics now receives a model with no next-less state.
