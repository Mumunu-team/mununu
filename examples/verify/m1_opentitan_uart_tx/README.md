# M.1 — OpenTitan `uart_tx` KMTS-lifter milestone

> Second industrially-realistic validation milestone in the KMTS pivot
> (`you-are-a-formal-vast-lake.md` (a planning note, not tracked) §10.3 M.1).
> Sits at the closing gate of R.2 + R.3 — the BTOR2 → KMTS lifter and
> the KleeneDomain evaluator must process a real OpenTitan RTL module
> with counters and produce a verdict on a declared property.

## Fixture

[`uart_tx.sv`](https://github.com/lowRISC/opentitan/blob/master/hw/ip/uart/rtl/uart_tx.sv) — the UART transmit module from OpenTitan's UART IP. 79 LOC,
single module, no SVA, no typedef enum, no multi-clock. The design
has:

- A 4-bit baud-rate divider counter (`baud_div_q [0..15]`)
- A 4-bit frame-bit counter (`bit_cnt_q [0..11]`)
- An 11-bit shift register (`sreg_q`)
- A 1-bit serial output register (`tx_q`)
- A 1-bit per-bit baud strobe register (`tick_baud_q`)

The `idle` output is computed combinationally: `idle = tx_enable ? (bit_cnt_q == 4'h0) : 1'b1`.

## Sidecar

[`source/uart_tx.mununu.json`](source/uart_tx.mununu.json) declares:

- `bit_cnt_q` and `baud_div_q` as `bounded_counter` with bounds 11 and 15.
- `sreg_q` as `discover` (SMT enumerates significant values).
- `tx_q` and `tick_baud_q` as `boolean`.
- Inputs `wr_data` as `ignored` (8-bit payload; not relevant to FSM-state safety).
- Two properties:
  - `idle_reachable_from_every_state`: `nu X. ((<> idle) && [] X)` — from every reachable state, idle is eventually reachable.
  - `idle_when_disabled`: `nu X. ((!tx_enable -> idle_after_step) && [] X)` — when `tx_enable` is low, the transmitter reports idle on the next cycle.

## Source pinning

[`source/uart_tx.sv`](source/uart_tx.sv) is a vendored copy from upstream OpenTitan,
pinned to the commit in [`source/UPSTREAM_COMMIT.txt`](source/UPSTREAM_COMMIT.txt). To refresh:

```bash
curl -sL https://raw.githubusercontent.com/lowRISC/opentitan/master/hw/ip/uart/rtl/uart_tx.sv \
  -o examples/verify/m1_opentitan_uart_tx/source/uart_tx.sv
git ls-remote https://github.com/lowRISC/opentitan.git HEAD | awk '{print $1}' \
  > examples/verify/m1_opentitan_uart_tx/source/UPSTREAM_COMMIT.txt
```

## Running the milestone

```bash
bash examples/verify/m1_opentitan_uart_tx/validate.sh
```

The script runs `mununu context eval --adapter sv-yosys` and confirms a non-vacuous verdict.

## Out of scope at M.1

- **SBY oracle cross-check.** Deferred; the M.1 mu-calc formula uses `<>` (existential) which doesn't map directly to SBY's bounded-safety shape.
- **Production-parameter scale.** uart_tx is fixed-shape; no scale-up planned for this fixture.
