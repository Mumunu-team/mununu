# Consumer briefing — 2026-10 SVA `assume` statements are applied as preconditions by default; `disable iff` voids the whole attempt

> **Audience:** anyone running `sv verify-auto` (CLI or `POST /api/v1/sv/verify-auto`) on RTL that contains SVA `assume` statements, or `disable iff` conditions that are not a plain reset — ROSF's SVA lane, monono's `make formal`, `--expect-*` gates, and anyone validating the response against `docs/api-schemas/`.
>
> **Related:** plan "cover the OpenTitan assertions", Phase 4; [mununu#705](https://github.com/Mumunu-team/mununu/issues/705) (defect). Earlier tranches: [#701](https://github.com/Mumunu-team/mununu/pull/701), [#704](https://github.com/Mumunu-team/mununu/pull/704). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **this changes default behaviour, adds a new flag and request field, and changes the JSON shape.**
>
> 1. **Assumes apply by default.** An SVA `assume` used to be **checked like an assertion**, and it constrained nothing. Now each supported assumption is **applied as a precondition**: the guarantees are checked on the traces along which every applied assumption has held. Assumptions no longer appear in `properties`.
> 2. **Choosing the behaviour.** CLI `--assumes precondition|check|ignore` and API `assumes` select it; `check` restores the old behaviour.
> 3. **New `assumptions` object.** The response reports `{mode, applied, unapplied, unguarded, consistency}`, plus an `assumptions` note.
> 4. **Fixed: mid-attempt disables (#705).** A kept `disable iff (D)` (a compound condition reset gating does not drop) was checked only at the attempt's first cycle, which gave a spurious VIOLATED when `D` rose mid-attempt. It now voids the attempt at any of its cycles.

## What changed

- `adapter/btor2/assume_monitor.rs` (new) — the `mununu_assume_ok` monitor.
  - Each applied assumption becomes a per-cycle `hold`.
  - `ok` is a running conjunction latch.
  - Two shapes are applied: `assume property (B)` and `A |=> B`, over booleans and minted expressions, each with an optional `disable iff`.
  - **Warm-up:** an obligation that reads `$past` is armed only once that history is filled. This weakens the assumption, never strengthens it.
- `adapter/slang/assume_guarantee.rs` (new) — `relativize` guards each boolean block of a guarantee: `!ok ||` where the guarantee is universal, `ok &&` where it is existential.
  - Because `ok` is monotone, this is exact assume-guarantee semantics.
  - It keeps the rescue-reducible shapes. `guard_with_disable` applies the same guarding to a kept `disable iff`.
- `adapter/slang/verify_auto.rs` — `AssumeMode`, `plan_assumptions` and `finish_assumptions`.
  - A consistency check, `AF ¬ok`, decides whether the assumptions are contradictory.
  - If they are, every HOLDS is flagged vacuous.
- **Surfaces.**
  - CLI: `--assumes`.
  - API: `assumes` in the request; `assumptions` in the response (`AssumptionsView`, omitted when the design has no `assume`).
  - Both schemas are regenerated.

## What to expect

On the OpenTitan census, measured against main `42c4292`:

| class | main | this change |
|---|---|---|
| holds | 51 | 53 |
| violated | 15 | 11 |
| properties counted | 136 | 134 |

- **The flips.** Both arbiters' `LockArbDecision_A` go from VIOLATED to HOLDS under `ReqStaysHighUntilGranted0_M`. Both changes are needed: with `--assumes check` it stays VIOLATED, and without the #705 fix it is spuriously VIOLATED.
- **Assumes leave `properties`.** The two assumes no longer appear there; they used to be counted as VIOLATED properties.
- **Nothing else moved.**

**A VIOLATED you saw before may now HOLD,** in two cases:

- the design's assumptions exclude the counterexample;
- the old verdict came from a disable that rose mid-attempt.

**A property that relied on an assumption being checked** must now use `--assumes check`.

## What to update, per consumer

- **ROSF** / **monono**:
  - Re-baseline `--expect-count`, because assumes are no longer properties.
  - Decide per design whether its `assume` statements describe the environment, which is the new default, or are claims to be checked, which needs `--assumes check`.
  - Read `assumptions.unapplied`, because a guarantee checked without an unapplied assumption may be VIOLATED by a trace the RTL's author excluded.
  - Treat `consistency: "contradictory"` as a red flag: every HOLDS is vacuous.
- **Schema validators**: vendor the regenerated `sv-verify-auto-request` and `sv-verify-auto-response` schemas. Both new fields are optional.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | default behaviour, new flag / fields | **Yes** |
| `mununu-dev` | test image; schema drift tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e and the census run here | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

No subprocess-tool version change.

## Test the transition

```bash
cargo test -p mununu-core --lib -- assume_monitor assume_guarantee assumptions_are_split \
  every_translated_formula_of_the_real_fixtures a_kept_disable_guards
docker run --rm -v "$(pwd)":/work -v mununu-target:/cargo-target -w /work mununu-sva \
  cargo test -p mununu-core --lib --all-features -- --ignored --nocapture \
  e2e_assumes_are_applied_as_preconditions e2e_a_disable_rising_mid_attempt_disables_it
```

## Provenance

PR: (filled in once opened). `Repro: in-repo` for #705. The unit reproducer fails on main `42c4292`, which produces the first-cycle-only form; this was checked by hand in a worktree.

## Not covered here

- **Other assume shapes.** Assumptions over bounded sequences, liveness assumptions (`s_eventually`) and `GF` fairness are not applied as preconditions; the `// @mununu_assume GF` annotation path is separate and unchanged. They are reported under `unapplied`.
- **Dead ends.** A dead end, where an assumption-respecting prefix cannot be extended, is not reported separately. Guarantees read it with the finite-prefix semantics.
- **Long attempts.** For an attempt longer than one cycle, a disable at an intermediate cycle with no boolean block of its own is not seen. That is still never weaker than SVA.
