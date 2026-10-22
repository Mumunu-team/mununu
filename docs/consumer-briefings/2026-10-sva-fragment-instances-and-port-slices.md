# Consumer briefing — named sequence instances, `or` antecedents and unpacked-port elements translate; the safety monitor compiles relational leaves (2026-10)

> Source of truth: [`assertion_rewrite`](../../crates/mununu-core/src/adapter/slang/assertion_rewrite.rs) + [`antecedent_implies_any`](../../crates/mununu-core/src/adapter/slang/translate.rs) + [`compile_boolean_subtree`](../../crates/mununu-core/src/adapter/btor2/bad_monitor.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** rosf, monono, mununu-ui and anyone whose gate pins `unsupported` counts or `unknown` rows of `sv verify-auto`. PR C of the 2026-10-21 verdict-flipping levers plan.

## TL;DR

- **Three SVA spellings leave `unsupported`.** All three rewrites are exact:
  - **A named `sequence` / `property` instance** is inlined. slang binds the actual arguments, so `##1 PingSigInt_S |-> …` translates. A declaration with local variables or recursion stays `unsupported`, now with its own reason. OpenTitan's `ASSERT_FPV_LINEAR_FSM` is such a declaration.
  - **A sequence `or` in an antecedent:** `S1 or S2 |-> C` is checked as `S1 |-> C` and `S2 |-> C`.
  - **An element of an unpacked *port*** reads as the slice sv2v flattens it to, with the range's left bound at the top (`logic [3:0] data_i [8]` puts `data_i[0]` at bits 31:28). A constant index is that slice. A variable index is checked per element (`idx != k || cmp[data_i[k]]` for every `k`) when every value of the index names an element. An unpacked array that is not a port is a memory and stays refused (mununu#514).
- **The safety and liveness rescues compile relational leaves.** `lhs OP rhs` over two signals of the same width, such as `data_o == data_i__bits31_28`, now reaches the reach portfolio, through the `bad` monitor and the l2s monitor (`a |-> s_eventually (x == y)`), instead of stopping at `SafetyShapeNotReducible`. Mixed widths are refused at emission, with the reason named.
- No JSON shape change. No CLI flag or route change.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `unsupported` rows on these shapes become verdicts; a row ⊥ with `safety-shape-not-reducible` over a relational leaf may now decide | rerun; re-pin counts |
| **monono** | same | rerun the gate |
| **mununu-ui** | none | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | translator + rescue live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not translate SVA |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | rows leave `unsupported` / `unknown` |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured

| run (`mununu-sva`) | before | after |
|---|---|---|
| `e2e_opentitan_sva_census` | 8 unsupported: `AssertionInstance` ×4 and `Binary` antecedent on prim_alert_sender, the unpacked-array comparison ×2 on prim_arbiter_ppc / prim_arbiter_tree, the `ASSERT_FPV_LINEAR_FSM` instance ×2 (rom_ctrl_fsm, keymgr_ctrl) | **2 unsupported** (the two local-variable properties, now with their own reason); prim_alert_sender `SigIntPing_A`, `SigIntAck_A`, `InBandInitFsm_A`, `InBandInitPing_A` **HOLD**; both arbiters' `DataFlow_A` (`data_o == data_i[idx_o]`, a 256-bit port over the exact cap) **HOLD** through the reach rescue now that the monitor compiles the relational leaf. No previously decided row moved. Totals 164 holds / 18 skipped / 7 violated / 0 ⊥ / 2 unsupported (667 s) |
| `diff_corpus_monotone_verdict_ledger` | PASS | PASS (no flip) |
| `e2e_opentitan_ip_edn_full` | 56 / 11 / 0 / 0 | PASS, unchanged (823 s) |

(The census also shows `otbn_start_stop_control` with two new `OutputsKnown_A` rows and `StartSecureWipeImpliesRunning_A` VIOLATED: those come from vendoring its `prim_mubi4_sender` in mununu#750's PR. On the full closure the row is a definite violation with no undriven net in its cone; like edn's `EdnFatalAlertNoRsp_A` it reads as an environment assumption the bare block does not carry. Not replayed in simulation.)

## Verification steps for every consumer

```bash
# a named sequence in an antecedent: translates (no `unsupported` row) and decides
mununu sv verify-auto prim_alert_sender.sv --top prim_alert_sender --json | jq '.unsupported'
```

## Provenance

- Fix commit: pending merge — branch `feat/sva-fragment-landing`.
- Plan: verdict-flipping levers for the SoC rows (2026-10-21), PR C; the captured slang shapes in `crates/mununu-core/src/adapter/slang/testdata/sva_instance_forms.{sv,ast.json}`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Local-variable sequences / properties (`ASSERT_FPV_LINEAR_FSM`), a repetition of a named sequence that is not a plain boolean, an index that can leave the array, multi-dimensional unpacked ports.
- Addend leaves (`lhs == rhs + k`) in the safety monitor.
