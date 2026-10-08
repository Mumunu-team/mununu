# v12 — `link_ctrl`, an LLM-written FSM with `localparam` state encodings

> Source of truth: [`link_ctrl.sv`](link_ctrl.sv) — surface: CLI+API (`sv verify-auto`, `sv check-fsm`, `sv mutate`, `sv verify-recoverability`, all with `--frontend slang`)

A request/acknowledge link controller with retry, error and fatal handling — generated from a
natural-language spec by an LLM for a 2026-10-08 demonstration, kept here **as written** (its
assertion suite included) because four `sv` verbs disagreed on it and each disagreement became
an issue:

| issue | verb | what the module exposed |
|---|---|---|
| [mununu#632](https://github.com/Mumunu-team/mununu/issues/632) | `sv verify-auto` | SVA atoms comparing against `localparam` names (`state_q == S_XFER`, `retry_cnt_q == MAX_RETRIES`) were skipped as "unknown register/signal"; one of them is **VIOLATED** on the design. The translator now folds the elaborated constant (`e2e_632_localparam_atoms_decide_on_link_ctrl`). |
| [mununu#633](https://github.com/Mumunu-team/mununu/issues/633) | `sv check-fsm` | the counters were classified as FSM registers (false VIOLATED) and the real state register was not scanned |
| [mununu#634](https://github.com/Mumunu-team/mununu/issues/634) | `sv verify-recoverability` | `--config-values rst_n=1` returned an empty refinement where `--refine` partitions over the same input |
| [mununu#635](https://github.com/Mumunu-team/mununu/issues/635) | `sv mutate` | `--list` advertised mutation kinds `--mutation` did not accept |

Design shape: a 6-state FSM (`S_IDLE … S_FATAL`, 3 bits, `localparam`), a 2-bit retry counter
with a `MAX_RETRIES` limit, a 3-bit beat counter, registered outputs, `S_FATAL` with no exit but
reset. Eight inline concurrent assertions, labelled.

```bash
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --lib --all-features -- --ignored e2e_632_
```

Run on the host only through the `mununu-sva` image (slang-gated; see CLAUDE.md).
