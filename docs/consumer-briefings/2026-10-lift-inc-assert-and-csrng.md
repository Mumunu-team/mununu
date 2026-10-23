# Consumer briefing — `INC_ASSERT` in the lift shims, lift-only `_lift.sv` variants, csrng vendored, a retry on the BTOR backend's topological loop (2026-10)

> Source of truth: [`scripts/vendor_opentitan_ip.py`](../../scripts/vendor_opentitan_ip.py) (`make_shim`, `strip_inc_assert`) + [`backend_loop_retry_script`](../../crates/mununu-core/src/adapter/yosys/mod.rs) + [`e2e_opentitan_ip_csrng_full`](../../crates/mununu-core/tests/differential_oracle_e2e.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** rosf, monono, mununu-ui and anyone who copies `prim_assert_shim.sv`, lifts OpenTitan RTL with the corpus shims, or pins `sv verify-auto` JSON. PR A0 of the 2026-10-22 fault-injection plan.

## TL;DR

- **A lift that used to die with `Found topological loop while processing cell $N` may now succeed.** yosys's BTOR backend reports that loop on designs `check` finds loop-free (yosys-slang's `$buf` cells chained through wide vectors). On exactly that error the lift reruns its script once with `opt_clean` before `write_btor` and reports it: `diagnostics.lift_retry` (new nullable string) and a `lift-retry` note (Info). A lift that succeeds on the first run is byte-identical to before. Any other yosys error is not retried.
- **The synthesis shims define `INC_ASSERT`.** `prim_assert_shim.sv` (generated) and the corpus `prim_assert.sv` stubs now put OpenTitan's assertion-support logic in the lift — `prim_count`'s `fpv_err_present`, the `ASSERT_FPV_LINEAR_FSM` `<name>_cond` flop — while every assertion statement still expands to nothing. Two upstream files cannot be lifted with that region in: `prim_alert_sender.sv` (`sequence` blocks) and `prim_sparse_fsm_flop.sv` (an enum-method loop neither frontend compiles). The vendor script writes `FILE_lift.sv` variants (`--strip-inc-assert FILE`) and wires `--lift-override FILE=FILE_lift.sv` into `run.sh`; the e2e harnesses pick up any `_lift.sv` beside a source. The sparse flop's `unused_err_o` is therefore still the synthesis constant: the FSM error → alert rows hold vacuously on these models (documented in both IP READMEs; the fault-injection posture will mint the flag).
- **csrng is vendored and pinned** (`examples/verify/ip_opentitan_csrng`, 67 files, `aes_cipher_core` black-boxed): 34 HOLDS / 0 / 0 / 3 unsupported in 110–153 s, pinned by `e2e_opentitan_ip_csrng_full`.
- **Census rows appear:** the corpus blocks' `prim_count` assertions that skipped on `fpv_err_present` decide (prim_esc_receiver +7 HOLDS; rom_ctrl_fsm's six reach the next wall, `rst_ni`). If your gate counts `skipped` rows of these blocks, re-pin.
- **Schema:** `ModelDiagnostics` / API `diagnostics` gain `lift_retry: string | null` (regenerated `sv-verify-auto-response.schema.json`). No field removed, no CLI flag or route change.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (`--profile industrial`, OpenTitan lanes) | a design that failed to lift with the BTOR backend's loop now lifts (a `lift-retry` note says so); if you copy `prim_assert_shim.sv`, the new one defines `INC_ASSERT` and needs the two `_lift.sv` overrides for the alert sender and the sparse flop | rerun; take the shim + variants from `examples/verify/ip_opentitan_edn/source` or regenerate with `--strip-inc-assert` |
| **monono** (direct CLI `--json`) | same; `diagnostics.lift_retry` is a new nullable field | rerun the lane at the next pin |
| **mununu-ui** | a new nullable diagnostics field | none required (RTL verb) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the retry lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not lift RTL |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | lifts that failed may succeed; shim content changed |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured (mununu-sva)

| run | before | after |
|---|---|---|
| `e2e_opentitan_ip_csrng_full` (new) | — (34 / 0 / 0 / 3 measured by hand on 2026-10-18 with `--frontend slang`) | **34 / 0 / 0 / 3**, 110 s, frontend yosys-slang after the sv2v attempt fails, `lift_retry` set |
| `e2e_opentitan_ip_edn_full` | 56 / 11 / 0 / 0 | **56 / 11 / 0 / 0** (866 s; sv2v frontend, no retry) |
| `e2e_opentitan_sva_census` | 164 holds / 18 skipped / 7 violated / 0 ⊥ / 2 unsupported | **171 holds / 11 skipped / 7 violated / 0 ⊥ / 2 unsupported** (1,685 s): the seven `prim_esc_receiver` rows over its `prim_count` instance leave `skipped` (`fpv_err_present` is in the model now) and HOLD; `rom_ctrl_fsm`'s six sibling rows move from that wall to the next one (`rst_ni` — the block pins no reset, so the input is not an atom the exact engine takes); every other row, every `lift=` label and every violated row unchanged |
| `diff_corpus_monotone_verdict_ledger` | PASS | **PASS** (51 s, no flip) |

## Verification steps for every consumer

```bash
# a design whose lift died in the BTOR backend: expect a verdict, `diagnostics.lift_retry` non-null,
# and a `lift-retry` note
mununu sv verify-auto top.sv --top top --json | jq '{retry: .diagnostics.lift_retry, fe: .diagnostics.frontend}'
```

## Provenance

- Fix commit: pending merge — branch `feat/opentitan-lift-inc-assert`.
- Plan: fault-injection posture, linear-FSM properties and the k-fault adversary (2026-10-22), PR A0.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- Minting `unused_err_o` from the enum domain (PR A1), the `ASSERT_FPV_LINEAR_FSM` property itself (PR B).
- An unconditional `opt_clean` before `write_btor` (would change every lift's BTOR2; the retry keeps them byte-identical).
