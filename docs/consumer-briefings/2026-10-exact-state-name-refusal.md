# Consumer briefing — 2026-10 exact engine: an FSM whose state cell is named after its next-state signal is decided, not skipped

> **Audience:** anyone running `sv verify-auto` (exact engine, or the default portfolio whose first member it is) on FSMs lifted from OpenTitan-style RTL — `state_d` / `state_q` pairs with no symbol on the state cell itself.
>
> **Related:** closes [mununu#651](https://github.com/Mumunu-team/mununu/issues/651). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **Verdict-semantics change: properties that were `skipped` now decide.** When the lift leaves the FSM's state cell unnamed, mununu names it after its next-state signal (`state_d`), and the exact engine's "undriven bits" refusal then looked at that next-state *signal* — whose logic reads the design's free inputs — instead of the state cell the property actually reads. It skipped sound properties. On the vendored OpenTitan corpus 7 of 18 properties came back `skipped`; they now decide.

## What changed

- [`signal_reaches_anonymous_input`](../../crates/mununu-core/src/adapter/btor2/parser.rs): a name that `collect_symbols` gives to a state cell is judged on that cell alone — the same node the exact engine binds the property to. The alias / output path the refusal exists for (the yosys-slang partial-write shape, monono#partsel) is unchanged; `e2e_partsel_partial_write_slang_refuses_or_agrees_with_sv2v` still passes.

## Measured (OpenTitan corpus census, `mununu-sva` image, 2026-10-10)

| | before (main a0a1b4d) | after (with #650, #651, #652) |
|---|---|---|
| decided by the exact engine | 9 of 18 (+1 setup error) | **16 of 19** |
| ⊥ | 9 | 3 (input-dependent combinational outputs — a separate, deliberate guard) |

Newly decided: `prim_esc_receiver`, `prim_esc_sender`, `prim_alert_sender`, `usbdev_linkstate` (HOLDS); `rom_ctrl_fsm`, `otbn_start_stop_control` (VIOLATED, as recorded); `keymgr_ctrl` (VIOLATED, via #650).

**One recorded verdict changed direction and you should know why:** `prim_alert_sender` `AG EF Idle` was recorded VIOLATED in July and now decides **HOLDS**. Two independent engines agree on HOLDS over the identical BTOR2 (the exact engine, and the `btor2 verify-recoverability` cube ladder, which never went through the refusal), and the RTL agrees (every phase returns to Idle; `sigint_detected` forces Idle). The July reading was a two-player argument ("a never-acking environment traps it"), which is not what `AG EF` computes. The July VIOLATED could not be reproduced. If you pinned that verdict, re-pin it.

## What to update, per consumer

- **monono / rosf lanes:** properties over OpenTitan-style FSMs that read `skipped — … unwritten bits the RTL front-end left undriven …` will now decide. If a lane pins those skips by name, unpin them.
- **Report parsers:** nothing changed in the shape.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | the exact engine's refusal check | **Yes** |
| `mununu-dev` | test image; carries the new test | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only | No |

## Test the transition

```bash
cargo test -p mununu-core --lib -- x651_
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --test differential_oracle_e2e --all-features -- --ignored --nocapture diff_corpus_verdict_census
```

## Not covered here

- The 3 remaining ⊥ (`prim_arbiter_ppc`, `prim_arbiter_tree`, `prim_fifo_sync`): properties over combinational outputs driven by primary inputs, which the exact engine refuses by design and the cube fallback does not decide — tracked separately.
- The monotone ledger test is not in the nightly sweep; adding it is tracked on #651.
