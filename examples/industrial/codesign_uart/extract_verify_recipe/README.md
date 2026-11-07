# extract + verify — end-to-end codesign recipe

This directory is the canonical recipe for running mununu's full codesign
workflow against a real C firmware: extract the firmware's transition system
from C source, compose it with a peripheral protocol-spec automaton, and
evaluate cross-boundary mu-calculus properties on the composition.

> Source of truth: [`run.sh`](run.sh) — surface: CLI — the script invokes
> `mununu codesign extract-c` ([`crates/mununu-cli/src/main.rs`](../../../../crates/mununu-cli/src/main.rs#L946))
> and `mununu context eval`, both shipped CLI subcommands.

## What this recipe demonstrates

Three artefacts shipped in PR #50 / #51 / #52 made this end-to-end flow
practical for the first time:

- **PR #50** — the protocol-spec peripheral pattern. `verify.template.ctxdsl`
  carries a hand-authored `UartPeripheral` (Idle / Loaded / Transmitting) that
  composes against the firmware on the rendezvous-label alphabet. Under the
  composition, properties about cross-boundary state (e.g. *the peripheral
  never receives `wr_ctrl_tx_start` while it's mid-transmit*) are first-class.
- **PR #51** — `MUNUNU_STATE("Polling" | "Ready" | "Sending")` markers in
  `firmware.c`. Without them the synthesised CTXDSL would name states `S0` /
  `S1` / `S2` / `S3` and the user would have to guess which numeric state to
  reference in formulas.
- **PR #52** — the `controllable { label X; }` keyword fix on the synthesised
  output, which was the last step keeping `extract-c → splice → eval` from
  working without manual edits.

## The pipeline

```
firmware.c                                       (auto-extracted)
     │
     │   $ mununu codesign extract-c \
     │       firmware.c \
     │       --register-map ../register_map.json \
     │       --synthesize-automaton \
     │       --cmsis-stubs
     ▼
{ "functions": [{ ..., "automaton_ctxdsl": "..." }] }   (JSON; jq extracts
                                                         the automaton block)
     │
     ├──► (1.5) project per-access (kind, register, field) tuples into a
     │         `["label_1", ...]` array of rendezvous labels
     │
     │   $ mununu codesign reconcile-labels firmware_labels.json \
     │       --peripheral-register-map ../register_map.json
     │   (gate; mismatch reports firmware_only / peripheral_only labels)
     │
     │   python3 splices `automaton_ctxdsl` into the {{AUTOMATON_CTXDSL}} placeholder
     ▼
verify.template.ctxdsl  +  hand-authored UartPeripheral + composition + formulas
     │
     │   $ mununu context eval verify.ctxdsl --formula F --automaton A
     ▼
Verdict: states satisfying F, initial-state satisfaction
```

## Running the recipe

```bash
$ ./examples/industrial/codesign_uart/extract_verify_recipe/run.sh
```

Requires `clang`, `jq`, and `python3` on the path. The script regenerates
[`transcript.txt`](transcript.txt) byte-deterministically — re-running against
the same commit must produce identical output.

## Reading the transcript

The script evaluates three formulas:

1. **`firmware_reaches_sending` over `Uart_send_byte`** — verifies that
   the firmware automaton (in isolation, with no peripheral coupling) has a
   reachable `Sending` state. The result reads:

   ```
   States satisfying: 2/5
       Ready, Sending
   Initial states satisfying: 0/1
       (none)
   ```

   This is the **Skolem-paradigm result**, not a "may-reach" reachability check.
   mununu's `<>` modal evaluates as "the controller has a strategy that
   forces reaching Sending against an adversarial environment". From `Polling`
   and `Loop0` the only outgoing transitions are uncontrollable
   `rd_status_tx_busy` reads — the environment can keep the polling loop
   running indefinitely, so the controller cannot **force** Sending from
   there. Only `Ready` (one controllable `wr_data_byte` away from `Sending`)
   and `Sending` itself satisfy.

   This is the right answer for codesign — a verification verdict that says
   "from `Polling` the controller can force `Sending`" would be wrong, because
   the peripheral's `tx_busy` clearing is genuinely outside firmware control.

2. **`peripheral_transmits` over `UartPeripheral`** — the protocol-spec
   peripheral's `Transmitting` state is reachable from `Idle`. Holds for the
   peripheral in isolation because every state has a path to `Transmitting`
   under some sequence of firmware-driven labels.

3. **`safety_protocol_respected` over `UartProtocolSystem`** — the composed
   firmware × peripheral system. The verifier enumerates the **6 reachable
   composed states**:

   ```
   Idle|Loop0, Idle|Polling, Idle|Ready, Idle|S3, Loaded|Sending, Transmitting|S3
   ```

   The absence of `Transmitting|Sending` from this list is the protocol-
   conformance evidence — the firmware never tries to issue `wr_ctrl_tx_start`
   while the peripheral is already transmitting. This is what cross-boundary
   verification actually buys you.

## Soundness posture

- The firmware extraction is **sound for safety** by the
  [`docs/archive/design/c-extraction-correctness-scope.md`](../../../../docs/archive/design/c-extraction-correctness-scope.md)
  framing: LLVM-IR-based lifting over-approximates concrete C executions, and
  every register access in the synthesised automaton corresponds to an actual
  load/store the compiler emitted.
- The peripheral protocol-spec is **hand-authored** against the canonical UART
  handshake shape. It is not derived from any RTL implementation — the
  "trust me, this is what the protocol shape looks like" boundary is at the
  hand-authored `UartPeripheral` automaton, not at any extraction tool.
- Asynchronous composition is the only sound choice (Doc C §C.5) — bus
  arbitration on real silicon is non-deterministic, and synchronous one-step
  rendezvous would be unsound for racy access.

## What's missing — gaps the audit names

Gaps (3), (4), (5) from the verification-stack audit (conversation 2026-05-15)
are still open:

- **Gap (3)** — RTL/C label reconciliation. Today the peripheral side is
  hand-authored. Extracting it from RTL via the SV adapter requires a
  label-renaming pass that maps signal-level events to rendezvous labels using
  the register map's `sv_signal` bindings.
- **Gap (4)** — value tracking. The synthesised automaton sees register
  **accesses** but not the **values** written. Properties about specific
  payloads (e.g. "the firmware never writes `0xFF` to CTRL") aren't expressible.
- **Gap (5)** — timing primitives. The `tick` label is the only proxy for
  silicon-time progress; bounded-latency properties (e.g. "tx completes within
  10 cycles") aren't expressible.

These are queued for follow-up work. The recipe in this directory is the
known-good ceiling on what the M4 codesign stack can verify today.
