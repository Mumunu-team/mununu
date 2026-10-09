# Consumer briefing — 2026-10 `verify-recoverability --config-values`: a named config value is reported as asked, one cell included

> **Audience:** anyone passing `--config-values` / `config_values` to `btor2 verify-recoverability` or `sv verify-recoverability` (CLI or `POST /api/v1/{btor2,sv}/verify-recoverability`), and anyone keying assumption discovery off the partition — monono's lanes first.
>
> **Related:** closes [mununu#634](https://github.com/Mumunu-team/mununu/issues/634). Fixture: [`examples/verify/v12_link_ctrl_llm_fsm/`](../../examples/verify/v12_link_ctrl_llm_fsm/). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Refinement-shape change, no wire-format change.** `config_partition` discarded a partition with a single non-empty cell as "config-independent" — right for the bare `--refine` auto reset axis, wrong for an explicit `--config-values rst_n=1`, which is a scoped verdict the user asked for. Run B of the issue (`--config-values rst_n=1` on `link_ctrl.sv`, target `state_q == 0`) returned `refinement: {}`; it now returns `config_partition: { violated: [[["rst_n",1]]], exhaustive: false }`, and `--discover-assumptions` searches under that cell, as it already did for a violated cell of run A. The canonical `verdict` is unchanged (`holds` — reset is a free input).

## What changed

- [`config_partition`](../../crates/mununu-core/src/adapter/recoverability.rs) returns every enumerated cell for an explicit spec. The "≥2 non-empty cells" test is now [`depends_on_config`](../../crates/mununu-core/src/adapter/recoverability.rs), applied only by `auto_config_partition` (bare `--refine`), where the user named nothing and a config-independent partition would only repeat the bare verdict.
- Consequence for a multi-value explicit spec whose cells all agree (`mode=0,1,2,3`, all hold): the partition is now reported (four `holds` rows) instead of omitted. Nothing else in the refinement moves.
- `--help`, the API request doc and [`verify-verbs.md`](../api-schemas/verify-verbs.md) say so.

## Measured — `link_ctrl.sv`, target `state_q == 0` (`e2e_634_an_explicit_single_value_reset_pin_reports_its_violated_cell`, in the sva image)

| run | spec | before | after |
|---|---|---|---|
| A | `rst_n=0,1` | `holds: [[rst_n=0]]`, `violated: [[rst_n=1]]`, `exhaustive: true` | same |
| B | `rst_n=1` | `refinement: {}` | `violated: [[rst_n=1]]`, `holds: []`, `exhaustive: false` |
| B + `--discover-assumptions` | `rst_n=1` | nothing to key off | searches the pinned operational model from its post-reset state (the existing A→B composition) |

The canonical verdict is `holds` in every row: the refinement is diagnostic and never changes it.

## What to update, per consumer

- A lane that passed a single value and parsed an absent `config_partition` as "nothing to report" now gets one cell; read `violated` / `holds` as before.
- A lane that relied on an explicit all-agreeing multi-value spec being omitted (as a "config-independent" signal) should test `holds.len() == rows` instead; `exhaustive` still says whether the rows cover the input's range.
- Report parsers: no new fields.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | the refinement's `config_partition` for explicit specs | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
cargo test -p mununu-core --lib --features api -- x634_ config_partition      # the one-cell explicit spec, the auto axis still collapsing
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --lib --all-features -- --ignored e2e_634_
```

## Not covered here

- A wide / free config (over the 256-valuation cap) still returns no partition; the symbolic `∃config` path is deferred as before.
- Whether `--discover-assumptions` FINDS an assumption under the pinned cell is the design's: `S_FATAL` has no exit but reset, so no constant input hold recovers `link_ctrl`; the search runs, its result is honest.
