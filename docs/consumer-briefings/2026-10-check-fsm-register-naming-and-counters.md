# Consumer briefing — 2026-10 `check-fsm`: registers are named by a value-identical alias, and a counter is set aside with no verdict

> **Audience:** anyone running `btor2 check-fsm` / `sv check-fsm` (CLI or `POST /api/v1/{btor2,sv}/check-fsm`) as a CI gate, and anyone feeding `sv mutate --list` keys into `--mutation` — monono's lanes first.
>
> **Related:** closes [mununu#633](https://github.com/Mumunu-team/mununu/issues/633). Fixture: [`examples/verify/v12_link_ctrl_llm_fsm/`](../../examples/verify/v12_link_ctrl_llm_fsm/). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Verdict-semantics change + one additive response field.** On the demo fixture `check-fsm` reported the state register under the name of a combinational wire (`beat_cnt_d`, legal `{0..5}`, holds) and both counters as FSMs with legal `{0, 3}` → **two false `violated`**, exit 2. Now every register is named by its own symbol or a **value-identical** alias (`state_q`), never a next-value wire; a register its own logic steps by ±1 is listed as `"kind": "counter"` with no legal set and `"skipped"`. Same fixture after: `state_q` fsm `{0..5}` **holds**, `retry_cnt_q` / `beat_cnt_q` counter skipped, `fsm_registers_checked: 1`, `illegal_encodings_found: 0`, exit 0. `sv mutate --list` names registers the same way (`beat_cnt_d` no longer listed as a register; `stick:state_q` applies).

## What changed

- **Naming** — [`parser::canonical_register_names`](../../crates/mununu-core/src/adapter/btor2/parser.rs): the `state` line's own symbol; else the first value-identical `Op` alias (`uext … 0 NAME`), then output port; else the loose cone-tracing name only for a cell nothing names exactly. `collect_symbols` (the loose pass the sidecar and lint resolvers use) is unchanged. The loose pass attached the FIRST symbol-bearing op whose cone reaches a cell, and `beat_cnt_d` — computed under `case (state_q)` — reaches the state cell before its own; a value-identical alias cannot name the wrong cell. Used by `fsm_encoding_scan`, `mutate::list_targets` and `--mutation`'s register resolution (which also accepts any value-identical spelling, e.g. the port that mirrors a register).
- **Counters** — [`fsm_scan::is_counter`](../../crates/mununu-core/src/adapter/fsm_scan.rs): a register whose `next` cone (value positions: `ite` branches, width adjusts) contains `inc`/`dec` of the cell, or `add`/`sub` of the cell and `1` / `2^w − 1`, recognised through `uext`/`slice` (the `int`-context `cnt_q + 1`). Its values are a range, not an enumeration, so the compared-constants "legal set" `{0, limit}` was a wrong premise and every in-between value a false finding. **Arithmetic by any other step (`st + 3`) is still an FSM**: a computed out-of-enum value is the bug class the scan exists for (`illegal_fsm_reaches_the_illegal_encoding` keeps that).
- **Response shape (additive):** each `registers[]` entry carries `"kind": "fsm" | "counter"`; a counter has `"legal_encodings": []`, `"verdict": "skipped"`, `"illegal_encoding_reachable": false`. `fsm_registers_checked` counts `"fsm"` entries only (`registers.len()` may exceed it). Schema: [`btor2-check-fsm-response.schema.json`](../api-schemas/btor2-check-fsm-response.schema.json); doc: [`verify-verbs.md`](../api-schemas/verify-verbs.md).
- **The issue's second guess** (the state register "not scanned at all" because its `localparam` literals were not seen): measured otherwise — it *was* scanned, with the right legal set, under the wrong name. yosys folds `localparam`s before BTOR2; the slang-side fold (#639) is for SVA atoms, not for this verb.

## Measured — `link_ctrl.sv` (`e2e_633_check_fsm_scans_the_state_register_and_sets_the_counters_aside`, in the sva image)

| register | before | after |
|---|---|---|
| `state_q` (3-bit, `localparam` 0..5) | reported as **`beat_cnt_d`**, legal `[0..5]`, holds | `state_q`, `kind: fsm`, legal `[0,1,2,3,4,5]`, **holds** |
| `retry_cnt_q` (2-bit, `+ 2'd1`, limit 3) | legal `[0,3]`, **violated** (1 and 2 reachable) | `kind: counter`, `skipped` |
| `beat_cnt_q` (3-bit, `+ 3'd1`, limit `BEATS-1`) | legal `[0,3]`, **violated** | `kind: counter`, `skipped` |
| summary | `fsm_registers_checked: 3`, `illegal_encodings_found: 2`, exit 2 | `fsm_registers_checked: 1`, `illegal_encodings_found: 0`, exit 0 |

## What to update, per consumer

- **CI lanes gating on `check-fsm`:** a lane that was red on a counter's in-between values goes green; a lane that parsed `registers[].register` and matched `_d` names finds them gone. Read `kind` before treating an entry as a verdict; `fsm_registers_checked` no longer equals `registers.len()` when counters are present.
- **Report parsers:** `kind` is new and always present; nothing removed.
- **`sv mutate` lanes:** `--list` keys are the canonical names now; a `--mutation stick:<port>` spelling that mirrors a register resolves (it used to error).

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | `check-fsm` naming + counter classification; `sv mutate` naming | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
cargo test -p mununu-core --lib --features api -- x633_                  # naming tiers, counter rule, list_targets
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --lib --all-features -- --ignored e2e_633_
```

## Not covered here

- A counter stepped by a constant other than ±1 (`cnt + 2`) is still classified as an FSM and may be reported on its skipped values; say so on the issue if a real design has one.
- A register with no exact name (a cell whose only symbol sits on a derived op) keeps the loose cone name, as before.
