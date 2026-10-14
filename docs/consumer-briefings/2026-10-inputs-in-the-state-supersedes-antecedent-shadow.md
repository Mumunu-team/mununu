# Consumer briefing — 2026-10 inputs-in-the-state supersedes the antecedent shadow synthesis (mununu#677)

> **Audience:** every `sv verify-auto` lane that has an SVA `|=>` whose antecedent reads a primary input directly or through combinational logic (`req = ready && …`, `mem_rvalid_mine = mem_rvalid && …`): two monono `dram_preload` properties flip **VIOLATED → HOLDS**; anyone passing `--no-antecedent-shadow` / `no_antecedent_shadow` / `MUNUNU_NO_ANTECEDENT_SHADOW` (now accepted and ignored); anyone reading the `MUNUNU_SHADOW_BTOR2_DUMP` header.
>
> **Related:** [`2026-10-exact-inputs-in-the-state.md`](2026-10-exact-inputs-in-the-state.md) (the lift), the superseded [`docs/design/antecedent-shadow-synthesis.md`](../design/antecedent-shadow-synthesis.md). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Verdict-value change, a defect fix.** The mununu#476 antecedent shadow synthesis is removed from the exact engine. It renamed an input-derived antecedent atom to a register that sampled it and KEPT the `[] C`, so the engine checked the consequent one cycle late — `A@N → C@N+2` where the SVA says `A@N → C@N+1`. Invisible on a sticky consequent (`done_q |=> done_q`), it refuted every one-shot one, and it was masked until inputs-in-the-state (mununu#656) removed the refusal that had routed those properties to the cube engine. The lift gives the same antecedent its same-cycle value as a state predicate — the semantics the shadow approximated — so nothing is lost. No wire shape changed; three switches became no-ops.

## What changed

- `ExactSession::prepare` no longer runs the shadow synthesis; the `|=>` goes to the inputs-in-the-state lift as any other property whose atoms reach an input. `crates/mununu-core/src/adapter/btor2/antecedent_shadow.rs` and the `mu_calculus` helpers that served only it (`detect_pipeimplies_antecedent_atoms`, `substitute_predicates`) are deleted.
- `--no-antecedent-shadow` (CLI), `no_antecedent_shadow` (API) and `MUNUNU_NO_ANTECEDENT_SHADOW` are **accepted and ignored**; the refusal they used to restore is `--no-input-state` / `no_input_state: true` / `MUNUNU_NO_INPUT_STATE`.
- The `MUNUNU_SHADOW_BTOR2_DUMP` header now names the rewrite it omits as the inputs-in-the-state lift (it used to name the shadow synthesis).
- `docs/design/antecedent-shadow-synthesis.md` carries a superseded banner; `docs/verifying-rtl.md` and `docs/api-schemas/verdict.md` point at the lift.

## Why the old verdicts were wrong, concretely

monono `rtl/mem/dram_preload` (public), `--config-value rst_n=1`:

| property | before (`e909bc7`) | after | why |
|---|---|---|---|
| `a_addr_advances_on_a_beat`: `(req && accepted && !phase_q && waddr_q == 0) \|=> (waddr_q == 1)` | VIOLATED (exact) | **HOLDS** | `req` reads input `ready`; the shadow renamed `req` to `req@N-1` and kept the box, so the obligation became `req@N-1 ∧ accepted@N ∧ … → waddr_q@N+1 == 1`, refutable with `ready@N = 0` |
| `a_row_advances_by_the_stride`: `(req && accepted && phase_q && x_q == 3 && row_base_q == 0) \|=> (row_base_q == 8)` | VIOLATED (exact) | **HOLDS** | same mechanism |
| the other five (`done_q \|=> done_q`, `phase_q \|=> phase_q`, …) | HOLDS | HOLDS | sticky consequents are insensitive to the one-cycle shift |

In-repo reproducer (`input_state::tests::x677_a_one_shot_pipe_implication_on_an_input_derived_wire_holds`): `r' = in & acc`, `w = in` as an output wire; `(w && acc) |=> r` HOLDS by construction (the register one cycle after the antecedent is exactly the antecedent's inputs) and was VIOLATED on the parent commit, while the bare-input spelling `(in && acc) |=> r` — never shadowed — HELD on both. The spelling matters: the SVA translator emits bare 1-bit atom names, which is what the shadow resolved.

## Measured

| | result |
|---|---|
| monono `rtl/mem/dram_preload` in `mununu-sva` (slang), `--config-value rst_n=1`, default mode | **7/7 HOLD**, all by `exact-symbolic` — `a_addr_advances_on_a_beat` and `a_row_advances_by_the_stride` VIOLATED → HOLDS; the other five unchanged. The lift now samples **two** inputs for those properties (`accepted`, `ready`) — before, `req` had been shadowed away so only `accepted` was lifted and `ready` stayed a free per-step input, which is the counterexample's `ready = 0` |
| same, `--no-input-state` | 7/7 HOLD (the two `\|=>` by `explicit`, as the issue reported) |
| e2e sweep (`scripts/e2e-sweep.sh`, `mununu-sva`) | **46 passed, 0 failed, 0 crashed** — every real-RTL verdict breakdown unchanged |
| `x677_*`, `x656_*`, `mu_calculus::tests`, the dump-header test | 22/22; `cargo clippy -D warnings` on `mununu-core` (`--features api`) and `mununu-cli` clean; the API-schema drift test passes on the regenerated request schema |

## What to update, per consumer

- **monono:** the `dram_preload` gate that pins the default mode's two VIOLATEDs by name fails on this build, as intended — drop that check; the `--no-input-state` design check keeps passing (both HOLD there too, now by the same engine). Any script passing `--no-antecedent-shadow` keeps working and does nothing.
- **Other lanes:** a `|=>` with a one-shot consequent and an input-reading antecedent that was VIOLATED on an exact decision since 2026-10-10 (mununu#656's merge) should be re-run; a VIOLATED from the cube engine or from a sticky consequent is unaffected.
- **Report parsers:** no shape changes. `skipped` reasons of kind `antecedent-shadow` no longer occur.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | verdict change on `\|=>` with input-reading antecedents | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- x677_
mununu sv verify-auto rtl/mem/dram_preload/dram_preload.sv --source rtl/mem/dram_preload/dram_preload_sva.sv --top dram_preload --config-value rst_n=1   # sva_2 and sva_4 HOLD
```

## Not covered here

- A `|=>` whose antecedent reaches an **anonymous** input (a black-boxed submodule output) is still refused by both mechanisms — unchanged.
- The `--no-antecedent-shadow` flag and the API field are kept as no-ops for one release so argv and request bodies keep parsing; removing them is a later, separately briefed change.
