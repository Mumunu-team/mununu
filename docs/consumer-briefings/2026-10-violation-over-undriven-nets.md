# Consumer briefing — a VIOLATED that rests on an undriven net is withheld; the census blocks get their missing modules (2026-10)

> Source of truth: [`withhold_violated_over_free_leaves`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`BottomReason::ViolationOverFreeLeaves`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`, every `violated` row

**Audience:** rosf, monono, mununu-ui and anyone whose gate counts `violated` rows of `sv verify-auto`, or pins verdicts of the OpenTitan corpus blocks under `examples/verify/dc_opentitan_*`. PR A of the 2026-10-21 verdict-flipping levers plan (re-scoped after its measurement step; mununu#750).

## TL;DR

- **A `violated` row can become `unknown`.** When a module is missing from the source set, the lift leaves its outputs as undriven nets (anonymous BTOR2 `input`s), free at every cycle; so is any unconnected wire. A `holds` over such a model transfers to the RTL, a `violated` does not: the trace may need values the real module never produces. Now a `violated` row whose cone reads an undriven net becomes **`unknown`** with `bottom_reason.kind = "violation-over-free-leaves"`, the nets named in `detail` (`input_<nid>`), and the trace **kept** in `counterexample` as a may-trace.
- **Unchanged:** `holds`; a refuted `EF p` (the target is unreachable even on the freer model); and a **deliberate** cut — a `--cutpoint` or a `(* blackbox *)` stub (a state with no `next`; mununu#578: a cut you asked for keeps its verdict and its `control-slice` / `blackbox-cut` caveat).
- **Why:** measured on the OpenTitan census. Four rows shipped `violated` over a missing `prim_diff_decode` / `prim_sec_anchor_*` and **hold** on the full closure: `prim_esc_receiver.SigIntCheck1_A`, `prim_esc_sender.StateEscRespHiBackCheck_A` / `StateEscRespLoBackCheck_A`, `prim_alert_sender.AlertTest1_A`.
- **The corpus blocks are completed.** `dc_opentitan_prim_esc_receiver`, `prim_esc_sender`, `prim_alert_sender`, `rom_ctrl_fsm` and `otbn_start_stop_control` now carry their missing submodules, vendored from the same upstream commit (558921c) by `scripts/vendor_opentitan_ip.py` (provenance appended to each `UPSTREAM_COMMIT.txt`). `keymgr_ctrl` is not: its closure does not decide within 30 min.
- **Schema:** `bottom_reason.kind` has one more value (the description table in `sv-verify-auto-response.schema.json`); no field added or removed.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (`--profile industrial`) | a `violated` row over a missing module reads `unknown` (`violation-over-free-leaves`); under `--fail-on unknown` it still fails the gate, under the default it no longer does | rerun; for each such row, vendor the module named in the `blackbox-cut` note |
| **monono** (direct CLI `--json`) | same | rerun the lane at the next pin |
| **mununu-ui** | a new `bottom_reason.kind` value renders like the others | none required (RTL verb) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the post-pass lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not decide verdicts |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | `violated` over an undriven net → `unknown` |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured (mununu-sva)

| run | before | after |
|---|---|---|
| `e2e_opentitan_sva_census`, the four completed blocks | prim_esc_receiver 13 holds / 7 skipped / 1 violated; prim_esc_sender 3 / 11 / 2; prim_alert_sender 2 / 6 / 1; rom_ctrl_fsm 0 / 3 / 0 | prim_esc_receiver **23 holds** / 7 skipped; prim_esc_sender **25 holds**; prim_alert_sender **35 holds**; rom_ctrl_fsm **7 holds** / 7 skipped — the remaining skips are the vendored `prim_count`'s own assertions under the shim lift (`fpv_err_present`), new rows, not regressions; no row of these blocks reads `violated` any more |
| `diff_corpus_monotone_verdict_ledger` | PASS | **PASS** with one ledger row moved False → recorded ⊥: `keymgr_ctrl` `AG EF StCtrlReset` — its VIOLATED read undriven nets from seven missing modules, and the vendored closure does not decide it within 30 min; `otbn_start_stop_control` stays False, now on its full closure (`prim_mubi4_sender` vendored) |
| `e2e_cutpoint_stays_an_over_approximation_no_verdict_flips` | PASS | PASS — requested cuts lift as symbol-less next-less states and are not counted |
| `e2e_opentitan_ip_edn_full` | 56 / 11 / 0 / 0 | PASS, unchanged (edn has no missing module) |

## Verification steps for every consumer

```bash
# a design whose violation runs through a module missing from the source set: expect unknown,
# bottom_reason.kind = violation-over-free-leaves, the trace still in counterexample
mununu sv verify-auto top.sv --top top --json | jq '.properties[] | {name, outcome, k: .bottom_reason.kind}'
```

## Provenance

- Fix commit: pending merge — branch `fix/750-violated-on-free-leaves`.
- Issue: [mununu#750](https://github.com/vscorza/mununu/issues/750).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md); reproducers per [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- `keymgr_ctrl`'s missing modules (its closure is too large to decide in the census).
- A HOLDS-only reading of rows that skip on an undriven net in an atom's own combinational cone (the "havoc posture" of the original plan): measured reach ≤ 4 rows once the blocks are completed, so not built.
