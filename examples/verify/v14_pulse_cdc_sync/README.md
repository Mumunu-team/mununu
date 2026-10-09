# `pulse_cdc` — a two-flop pulse synchroniser with bound SVA (mununu#637, the #646 regression)

> Source of truth: [`e2e_646_an_input_antecedent_under_a_next_cycle_consequent_is_not_refuted`](../../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API (`sv verify-auto`, `POST /api/v1/sv/verify-auto`)

Vendored from [`Mumunu-team/monono`](https://github.com/Mumunu-team/monono) (`rtl/sys/pulse_cdc`, Apache-2.0), unchanged: `pulse_cdc.sv` (toggle on the source side, two unreset synchroniser flops and a reset edge register on the destination, `pulse_out = sync_q ^ sync_d_q`) and `pulse_cdc_sva.sv` (two tier-1 assertions and two `@mununu_guarantee` recoverability annotations, bound by module name).

Two properties on this block exposed two different defects on 2026-10-09:

- `a_out_is_the_edge: (pulse_out == 1) |-> (sync_q != sync_d_q)` — a tautology over registered state. The **explicit** engine refuted it ([mununu#637](https://github.com/Mumunu-team/mununu/issues/637)): the cube lift bound the combinational output `pulse_out` to the nearest state cell (`sync_q`) by the loose resolver, turning the atom into `sync_q == 1`.
- `a_toggle_moves_only_on_a_pulse: (pulse_in == 0) |=> (tog_q == $past(tog_q))` — an input antecedent with a next-cycle consequent. The antecedent-propagation pass (#629/#646) rewrote it as if the consequent were read in the antecedent's cycle, dropped the antecedent, and the **exact** engine refuted a property that holds; before the pass the exact engine skipped it ("atom references primary input") and the explicit engine held it.

Run the e2e in the sva image:

```bash
docker run --rm -v "$(pwd)":/work -w /work -v mununu-target:/cargo-target \
  mununu-sva cargo test -p mununu-core --lib --all-features -- --ignored e2e_646_ e2e_637_
```
