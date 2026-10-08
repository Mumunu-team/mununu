# Consumer briefing — 2026-10 SVA atoms that compare against a `parameter` / `localparam` now fold to the elaborated constant instead of skipping

> **Audience:** anyone running `sv verify-auto` on hand-written or LLM-written RTL whose SVA compare registers against named constants (`state_q == S_IDLE`, `cnt == MAX_RETRIES`) — monono and ROSF lanes included.
>
> **Related:** closes [mununu#632](https://github.com/Mumunu-team/mununu/issues/632). Same mechanism as the XL.6b enum-member fold. Fixture: [`examples/verify/v12_link_ctrl_llm_fsm/`](../../examples/verify/v12_link_ctrl_llm_fsm/). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **verdict-semantics change — properties that were `skipped` now decide.** The slang translator kept a `parameter` / `localparam` reference as a name inside the atom (`retry_cnt_q == MAX_RETRIES`), and the engine skipped the property as *"unknown register/signal `MAX_RETRIES`"*. It now folds the value slang already elaborated (`3`, `3'b10`, an overridden `--param` value), exactly as an enum member has folded since XL.6b. On the design that found it, four of eight properties moved skipped → decided and **one of them is VIOLATED**. A parameter whose value is not an integer literal (a string, a type) still does not fold and still skips by name.

## What changed

`adapter/slang/translate.rs` — `collect_parameter_values` reads every `Parameter` node's elaborated `value` from the `--ast-json` (slang serialises `parameter` and `localparam` alike, with the `-G` override applied) and feeds the same `NamedValue` → `IntegerLiteral` substitution the enum fold uses. An enum member of the same name wins; the first declaration of a parameter name wins.

## Measured — `link_ctrl.sv` (an LLM-written FSM, `localparam` state encoding, 8 SVA)

| property | before | after |
|---|---|---|
| `a_err_implies_retry_limit` — `err_q \|-> retry_cnt_q == MAX_RETRIES` | skipped (unknown `MAX_RETRIES`) | **HOLDS** |
| `a_retry_limit` — `!(retry_cnt_q == 2'd3 && state_q == S_RETRY)` | skipped (unknown `S_RETRY`) | **VIOLATED** — the third `nack` in `S_REQ` increments the counter to 3 *and* enters `S_RETRY` |
| `a_no_beats_outside_xfer` — `!(state_q == S_XFER) \|=> …` | skipped (unknown `S_XFER`) | **HOLDS** |
| `a_idle_quiet` — `state_q == S_IDLE \|-> …` | skipped (unknown `S_IDLE`) | **HOLDS** |
| the other four (literal-only atoms) | decided | unchanged |

Every property of the suite now decides (`e2e_632_localparam_atoms_decide_on_link_ctrl`, in the `mununu-sva` image). The VIOLATED verdict is a property of that demo design as written — reported as such, not as a finding about any real system.

## What to update, per consumer

- **Re-run lanes with `skipped` properties whose reason names a parameter.** Each becomes a verdict; a `verify.sh` that pinned `skipped` as the expectation (or counted skips as coverage) will see the count change. A property that was skipped for another reason (an out-of-fragment construct, a non-literal parameter) is unchanged.
- No wire-shape change. Formulas in the report now show the literal (`retry_cnt_q == 3`) where they showed the name.
- `--param NAME=V` overrides are reflected in the folded value (slang elaborates them before the dump).

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | translator; previously skipped properties decide | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; the e2e fixture | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The fold, at the translator (no tools):
cargo test -p mununu-core --lib --features api -- x632_ enum_member_comparison
# 2. The fixture end to end, in the image:
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --lib --all-features -- --ignored e2e_632_
# 3. Your design: diff `sv verify-auto --json` before/after — only skipped → decided moves are expected.
```

## Not covered here

- A parameter whose value is not an integer literal (string, real, type, unpacked) stays a name; the skip reason names it.
- The other three issues the same demo filed (#633 `check-fsm` register addressing, #634 `verify-recoverability --config-values`, #635 `sv mutate` kinds) are separate PRs.
