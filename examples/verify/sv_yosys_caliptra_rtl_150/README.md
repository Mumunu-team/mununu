# sv_yosys_caliptra_rtl_150 — Caliptra-RTL boot FSM auto-extraction

> **Two entry points.** [`validate.sh`](validate.sh) is the original
> Phase-A.3 *structural* sanity check (bit-blast reduction; not a
> property claim — see [Status](#status)). [`validate_m4_cegar.sh`](validate_m4_cegar.sh)
> is the **M.4 milestone**: full automated predicate-abstraction CEGAR
> that decides a sound **pre/post-distinguishing** verdict on the CWE-1245
> boot-FSM hazard — pre_fix = definite hazard, post_fix = the *definite*
> hazard removed (KleeneBot, not "proven safe"; see
> [M.4](#m4--predicate-abstraction-cegar) below).

## M.4 — predicate-abstraction CEGAR

`validate_m4_cegar.sh` runs the R.5 predicate-cube CEGAR path
(sv2v → Yosys `setundef -anyconst` → BTOR2 → `mununu btor2 cegar` with
per-target SMT-proved must-edge inference) on both the bug-bearing
`pre_fix` and fixed `post_fix` variants.

Property: `<> (p5 || p6 || p7)` over the cube `{boot_fsm_ns ∈ {5,6,7}}` —
"the next-state register can transition into an unmatched (undefined)
`boot_fsm_state_e` encoding" (the legal encodings are 0..4; the
default-less `unique casez` leaves 5/6/7 unhandled — CWE-1245).

```
pre_fix  (no default arm):       verdict cells T=7 F=1 ⊥=0  → hazard DEFINITE
post_fix (default holds + reset): verdict cells T=4 F=1 ⊥=3  → hazard INDEFINITE (⊥)
```

The undefined-encoding latch is **definitely** present in the bug-bearing
variant (every cell decided, ⊥=0) and **no longer definite** in the fixed
variant (≥1 undefined-encoding cell is KleeneBot) — the sound pre/post
difference is the milestone evidence.

**Soundness (revised 2026-06-22, IR-track P3.4).** The original claim —
post_fix `T=0`, "hazard UNREACHABLE, fix verified" — was **unsound**: it
relied on a Skolem-collapsed `<>`→`[]` diamond (one shared `step` label ⇒
"all step-successors satisfy") that ignored the cube's over-approximating
may-self-loops. Under the corrected EXISTENTIAL `Control::All` diamond,
post_fix is genuinely **KleeneBot**, not definite-safe — and rightly so:
post_fix's `default: boot_fsm_ns = boot_fsm_ps` *holds* the undefined
encoding, escaping only via the reset window (`boot_fsm_ps <= arc_IDLE ?
BOOT_IDLE : boot_fsm_ns`). So the `{p5,p6,p7}` cube **cannot soundly prove
the fixed FSM safe**; it soundly shows the *definite* hazard is gone.
Must-edges are Z3-proved (∀∀, no sampling); `setundef -anyconst`
over-approximates power-up. Proving full post_fix safety would need a finer
abstraction / CEGAR to convergence (out of scope). RTL-level
counterexample replay under a cycle-accurate simulator remains the deferred
empirical step. This reproduces the known caliptra-rtl #150 bug/fix pair
via the automated pipeline (as a sound definite→indefinite distinction).

```bash
cargo build -p mununu-cli
./validate_m4_cegar.sh        # requires yosys + sv2v on PATH
```

## What this exercises

Runs the `sv-yosys → BTOR2 → bit-blast` pipeline on
chipsalliance/caliptra-rtl's
[`soc_ifc_boot_fsm.sv`](https://github.com/chipsalliance/caliptra-rtl/issues/150)
(`pre_fix` variant, the bug-bearing source). The headline measurement:

| Metric | Before Phase A.3 | After Phase A.3 |
|---|---|---|
| Bit-blaster verdict | killed at 22 min / 4.6 GB RSS | completes in seconds |
| Realised state count | n/a (didn't finish) | **4 096** |
| Raw state-cell width | 2^19 = 524 288 | unchanged (abstraction reduces enumeration, not width) |
| Reduction factor | — | **128×** |

The reduction comes from **two composing mechanisms**:

1. **Sidecar-declared abstractions** (`wait_count`, `fw_update_rst_wait_cycles`,
   `BootFSM_BrkPoint`, `BootFSM_Continue`) — user wins on collision per
   the Phase A.3 §3.5 policy.
2. **Auto cone-of-influence** — runs but does not classify any new
   drops on this fixture, because the user-curated sidecar already
   lists every relevant signal. The auto-COI pass is exercised through
   the same code path that drove the headline reduction on smaller
   designs (see
   [`crates/mununu-core/src/adapter/btor2/dep_graph.rs`](../../../crates/mununu-core/src/adapter/btor2/dep_graph.rs))
   and is observable in the bit-blaster's pre-partition state-count
   warning (`2^19 = 524288 explicit states` → post-abstraction 4 096).

## Phase A.4 update (2026-05-20) — verdict-flip blocked by state-bit cap

The Phase A.4 predicate-image discovery algorithm is **complete** and
**correct**, but the Caliptra `no_undef_reachable` verdict-flip
criterion is **not yet achieved**. The trade-off measured:

| Yosys `setundef` pass | State bits | Predicate-image finds `boot_fsm_ns ∈` | Verdict |
|---|---|---|---|
| `-zero` (default) | 19 → 4 096 states | **{0, 1, 2, 3, 4} only** | 1/1 initial satisfies — CWE-1245 silently masked |
| `-anyseq` (opt-in via `MUNUNU_YOSYS_SETUNDEF_ANYSEQ=1`) | **56 — over MAX_STATE_BITS=20 cap** | **{0, 1, 2, 3, 4, 5, 6, 7}** (bug encodings surface) | Cannot translate (bit-blaster refuses) |

So the algorithm works in isolation (Step 4.4's `mununu btor2 discover`
on the `-anyseq`-emitted BTOR2 surfaces the violating encodings), but
the full `sv-yosys → BTOR2 → bit-blast → eval` pipeline cannot close
the verdict because the bug-preserving `-anyseq` synthesis pass
explodes the state space past the explicit-state engine's reach.
This matches the [Phase A.4 plan's anticipated fallback](../../../.claude/plans/phase-a4-predicate-image.md#proof-by-fire-target-validation-contract):
the gap is **measured**, not unknown.

**Next-step options** (deferred to the A.4b follow-up plan):
1. **Lift the bit-blast cap** to absorb the `$anyseq` cells —
   impractical at 2^56 explicit states.
2. **Compose-and-decompose** (Phase 3 of the BTOR2 roadmap) to split
   the design and verify pieces under the cap.
3. **Phase B (IC3-IA)** — implicit predicate abstraction inside an IC3
   prover doesn't materialise the state space, so the bit-blast cap
   doesn't apply.
4. **Sidecar-side workaround**: use the default `-zero` synthesis but
   author a sidecar that explicitly declares the bug-bearing branches
   via `predicates {}` blocks the mu-calculus formula references
   directly. Cheap but model-specific.

The end-to-end "first auto-extracted real-upstream-bug finding" claim
in [proof-by-fire-findings.md](../../../docs/design/proof-by-fire-findings.md)
remains **partially open** — see the Phase A.4 update there for the
full record.

## Status

> **Source of truth:** [`validate.sh`](validate.sh) (asserts state-count
> cap) — surface: CLI-only — this is an example fixture, not a CLI
> capability.
> **Status: structural milestone end-to-end; verdict is not yet a
> finding.** Two layers report cleanly:
>
> 1. **Translation + bit-blast:** completes in seconds. State count
>    4 096 (raw bit width 2^19 = 524 288 → 128× reduction via the
>    sidecar's `wait_count` bounded counter + per-input `Ignored` /
>    `BoundedCounter` declarations).
> 2. **Mu-calculus evaluation:** completes in roughly 2–3 minutes on a
>    debug build (sets the Phase A roadmap's runtime gate at ≈ 5 min
>    wall-clock). All three sidecar properties — `no_undef_reachable`,
>    `boot_idle_reachable`, `safety_all_states_have_successors` —
>    return verdicts.
>
> However, the verdicts are **not a property claim** today. The
> formula atoms (`boot_fsm_ps == 5 || boot_fsm_ps == 6 ||
> boot_fsm_ps == 7`) need to bind to state-cell valuations on the
> bit-blasted CLTS; today they parse as free atoms that map to "false"
> uniformly, which makes the negation vacuously true on every state
> and `no_undef_reachable` returns 4 096 / 4 096 satisfying — a
> **vacuous** verdict, not a discriminating one. The same pre-fix
> source `vs.` the post-fix source would produce identical verdicts,
> so the verdict cannot be cited as evidence the bug was found.
>
> The earlier "CTXDSL re-parse" blocker (`unexpected token
> Symbol(LBrace)` when passing `--sidecar foo.mununu.json` explicitly
> to `mununu context`) was a CLI UX issue, not a pipeline blocker. It
> was fixed in the same Phase A.3 commit that ships this example —
> `--sidecar foo.mununu.json` is now treated as informational (the
> adapter auto-loads it via path adjacency).
>
> Per
> [`docs/policies/claims-integrity.md`](../../../docs/policies/claims-integrity.md):
> this is a **"structural milestone, not yet reproduced"** claim. The
> abstraction landed the design under the bit-blaster's
> `MAX_STATE_BITS = 20` cap *and* the mu-calculus evaluator returns
> finite-time verdicts; the **CWE-1245 UNDEF-state reachability**
> finding referenced in
> [`docs/design/caliptra-abstraction-analysis.md`](../../../docs/design/caliptra-abstraction-analysis.md)
> §2.3 cannot be cited as a verdict until **state-cell-aware
> predicate binding** ships (separate scope from Phase A.3).

## Files

| Path | Purpose |
|---|---|
| `source/soc_ifc_boot_fsm_pre_fix.sv` | Upstream RTL (bug variant) |
| `source/soc_ifc_boot_fsm_post_fix.sv` | Upstream RTL (fix variant) |
| `source/soc_ifc_pkg.sv` | Package referenced by the boot FSM |
| `source/soc_ifc_reg_pkg.sv` | Register package referenced by the boot FSM |
| `source/caliptra_top_reg_defines.svh` | Header referenced by the package |
| `source/caliptra_sva.svh` | SVA macro file referenced by the source |
| `source/soc_ifc_boot_fsm_pre_fix.mununu.json` | Hand-curated sidecar (`mununu_sv_annotation_v1`) |
| `validate.sh` | Structural sanity check runnable from this directory |

## Reproduction

```bash
# From the repo root:
cargo build -p mununu-cli
./examples/verify/sv_yosys_caliptra_rtl_150/validate.sh
```

Requirements:

- `yosys` 0.67 or later (with `read_slang`) on `PATH` for `validate.sh`
- `sv2v` 0.0.13 or later on `PATH` for the hand-yosys flows `validate_m4_cegar.sh` / `validate_m6_ui_e2e.sh` (the `mununu-sva` image provides both)

## Provenance

- Upstream issue: <https://github.com/chipsalliance/caliptra-rtl/issues/150>
- Upstream commit: `b436906f0b16ae` (chipsalliance/caliptra-rtl)
- CWE class referenced by the upstream maintainer: CWE-1245 (missing
  default in state machine).
- mununu phase context:
  [`.claude/plans/phase-a3-adapter-partition.md`](../../../.claude/plans/phase-a3-adapter-partition.md)
  step 3.7.

## See also

- [`docs/design/caliptra-abstraction-analysis.md`](../../../docs/design/caliptra-abstraction-analysis.md)
  — original Phase 1.7 analysis predicting the 4 000× reduction.
- [`docs/design/auto-extraction-architecture.md`](../../../docs/design/auto-extraction-architecture.md)
  §6 — decision-gate thresholds the Caliptra runtime feeds into.
- [`docs/design/proof-by-fire-findings.md`](../../../docs/design/proof-by-fire-findings.md)
  — the predecessor doc that motivated this example.
