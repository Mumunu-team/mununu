# Consumer briefing — 2026-10 `sv mutate`: all four kinds are documented and spelled one way; `invert-cond` now inverts where the logic reads the signal

> **Audience:** anyone running `sv mutate` (CLI or `POST /api/v1/sv/mutate`) to measure property adequacy — monono's twin lanes first.
>
> **Related:** closes [mununu#635](https://github.com/Mumunu-team/mununu/issues/635). Fixture: [`examples/verify/v12_link_ctrl_llm_fsm/`](../../examples/verify/v12_link_ctrl_llm_fsm/). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **CLI/API documentation + a mutation-application fix; no wire-shape change.** `--mutation` has accepted four kinds since #468 — `stick:`, `drop-reset:`, `off-by-one:`, `invert-cond:` — but `--help` and the API doc listed two, and `--list` spells the kinds as JSON keys with `_` (`off_by_one`), which the selector rejected; both spellings are accepted now and the help names all four. Separately, `invert-cond:<sig>` on a signal that yosys labels through a `uext 0` alias (`violation`, any `assign`ed wire) errored with *"no use sites to invert"* — the logic reads the underlying node, not the alias; it now inverts every reader of the value-identical set. Measured on the demo fixture: both advertised kinds apply; the LLM-written suite **kills the off-by-one** (once #639 lets the `localparam` properties decide) and **does not kill the inverted `violation`** — findings about the properties, recorded as such.

## What changed

- `Mutation::parse` normalises `drop_reset:` / `off_by_one:` / `invert_cond:` to the hyphenated kinds; the hyphenated form is canonical and the error message lists it. `--list`'s JSON keys are unchanged (`stick`, `drop_reset`, `off_by_one`, `invert_cond`) — a consumer parsing them keeps working — and the `--list` help says the keys are the kinds with `_`.
- `sv mutate --mutation` help, its `value_name`, and the API request's `mutation` doc list all four kinds with their syntax (`off-by-one:<reg>[@<const_nid>][:±1]`, `invert-cond:<sig>`).
- `apply_invert_cond` follows the name's `uext`/`sext`-by-0 alias chain to the node the logic reads and flips every reader of that node and of every alias of it (never the signal's own inputs). Before, a target `--list` advertised could fail to apply.

## Measured — `link_ctrl.sv` (eight SVA; `e2e_635_advertised_mutation_kinds_apply_and_the_link_ctrl_suite_kills_the_off_by_one_only`)

| mutation | before | after | verdict flips (on `main` with #639, so the `localparam` properties decide) |
|---|---|---|---|
| `off-by-one:retry_cnt_q` (shift the retry-limit constant) | applied | applied | **2 of 8 — killed**: `a_err_implies_retry_limit` holds → **violated** (`err` rises at the shifted limit, no longer `MAX_RETRIES`); `a_retry_limit` violated → holds (the design's own violation sat at the limit the mutation moved) |
| `invert-cond:violation` (negate `ack & nack`) | *"no use sites to invert"* | applied | **1 of 8, not a kill**: only the already-violated `a_retry_limit` moves to holds (every request now goes to FATAL before a retry); **no holding property is violated** — nothing says fatal is entered *only* on a violation |
| `off_by_one:retry_cnt_q` (the `--list` spelling) | *"unknown mutation"* | same mutation as above | — |

Per claims integrity: a flip measures the **spec's** adequacy, a non-flip is a vacuous property — never a bug in the design. Two lessons the measurement carries: the inverted-`violation` hole is the demo suite's to close (a "fatal only on violation" property changes the row and the test says which); and **a spec's measured adequacy depends on which of its properties decide** — before #639 the same suite flipped 0 and 0, because the four properties that catch the off-by-one were the skipped ones. Re-measure adequacy after any change that moves properties out of `skipped`.

## What to update, per consumer

- Twin lanes that pass `--list` keys straight into `--mutation` work now; nothing to change.
- A lane that asserted `invert-cond` on an `assign`ed wire *errors* (as a known gap) now gets a mutated run and verdicts to compare.
- Report parsers: no change.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | `sv mutate` parsing + `invert-cond` application | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
cargo test -p mununu-core --lib --features api -- x635_          # the parser aliases, the alias-following inversion
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --lib --all-features -- --ignored e2e_635_
```

## Not covered here

- `--list` still names registers through the loose symbol pass (`beat_cnt_d` appears as a register) — that is #633, the same root cause as `check-fsm`'s, fixed there.
- The demo suite's two measured holes are the author's to close (a bidirectional retry-limit property; "fatal only on violation"); this PR records them, it does not write them.
