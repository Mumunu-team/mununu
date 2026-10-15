# Consumer briefing — 2026-10 signed comparisons no longer decided unsigned; the simulator's shifts follow BTOR2

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) — ROSF's SVA lane, monono's `make formal`, `--expect-*` gates — and anyone relying on witness replay or reset-simulation seeding.
>
> **Related:** [mununu#693](https://github.com/Mumunu-team/mununu/issues/693), [mununu#694](https://github.com/Mumunu-team/mununu/issues/694); plan "cover the OpenTitan assertions" Phase 0. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change (two defect fixes).** (1) An SVA comparison that SystemVerilog evaluates **signed** — `x >= 0` with `logic signed [3:0] x`, a signed `signal OP signal`, `inside` over a signed operand, or a signed equality with a literal outside the signal's signed range — used to be decided **unsigned**, which inverts the answer for negative values (a false HOLDS on `x >= 0` for an always-negative `x`). Those assertions now come back `unsupported` with the reason, until signed comparisons are supported natively. (2) The concrete BTOR2 simulator now shifts by the full amount, as BTOR2 specifies; it used to mask the amount (`1 << 3` on a 5-bit value gave 1).

## What changed

- `adapter/slang/translate.rs`: `signed_comparison_guard` refuses a relational comparison on operands slang typed signed, and a signed equality with an out-of-range literal; `inside` refuses a signed left operand. A mixed signed/unsigned comparison is converted to unsigned by slang and is unaffected; so is every unsigned comparison.
- `adapter/btor2/bit_blast.rs`: the shift amount is the full value saturated at the width (fill with 0, or the sign for `sra`), matching `Bv::shl/shr/sra` and the exact engine's `barrel_shift`.

## What to expect

- A previously decided assertion over a signed signal may now be `unsupported` ("signed relational comparison …"). If it was HOLDS on a design where the signal goes negative, that HOLDS was wrong.
- Witness replays and reset-simulation seeding on designs with shifts by ≥ width or by any amount on a non-power-of-two width now agree with the exact engine; a previously reported counterexample whose replay disagreed may now replay.

## What to update, per consumer

- **ROSF** / **monono**: re-baseline `--expect-count`; re-check any assertion over a `signed` signal — if it moved to `unsupported`, its earlier verdict was not trustworthy.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | translator + simulator change | **Yes** |
| `mununu-dev` | test image | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e runs here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change.

## Test the transition

```bash
cargo test -p mununu-core --lib -- signed_relational_comparison_is_refused simulator_shifts_follow_btor2_semantics
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture e2e_signed_relational_comparison_is_not_decided_unsigned
```

## Provenance

PR: PR_PLACEHOLDER. `Repro: in-repo` — both reproducers fail on main 6732909 (checked in a worktree).

## Not covered here

- Deciding signed comparisons (minted signed BTOR2 comparisons) — Phase 2 of the plan.
