# Cross-boundary protocol-conformance verification — UART codesign

> Demonstrates **verification gap (2)** from the C-extractor audit
> (conversation 2026-05-15): the second-largest leverage point for
> turning the extracted firmware automaton into actual cross-boundary
> verification evidence. The chaotic peripheral stub
> `mununu codesign couple` emits is over-approximated by design;
> meaningful protocol-conformance properties need a peripheral side
> that knows what "good" looks like.

## What this directory adds

`verification_with_peripheral_spec.ctxdsl` is a single self-contained
context containing:

1. The same `UartDriver` firmware automaton hand-authored in
   `firmware.ctxdsl` (Init / Polling / Ready / Sending states).
2. A new `UartPeripheral` protocol-spec automaton with three states
   (Idle / Loaded / Transmitting) modelling the canonical UART
   handshake:
   - `wr_data_byte` moves the peripheral from `Idle` to `Loaded`.
   - `wr_ctrl_tx_start` moves it from `Loaded` to `Transmitting`.
   - The autonomous `tick` event eventually drops `Transmitting` back
     to `Idle`.
   - **Crucially:** `Transmitting` has *no outgoing* `wr_ctrl_tx_start`
     transition. That absence is the protocol-conformance assertion —
     the peripheral refuses to accept a second start while busy.
3. An asynchronous composition `UartProtocolSystem = UartDriver ||
   UartPeripheral`.
4. Four mu-calculus formulas — see below for what each one tells you.

## The substantive verification result

Run the safety formula over the composition:

```text
$ mununu context eval verification_with_peripheral_spec.ctxdsl \
    --formula safety_protocol_respected --automaton UartProtocolSystem
Formula 'safety_protocol_respected' over automaton 'UartProtocolSystem':
  States satisfying: 7/7
    Init|Idle, Init|Transmitting, Polling|Idle, Polling|Transmitting,
    Ready|Idle, Ready|Transmitting, Sending|Loaded
  Initial states satisfying: 1/1
    Init|Idle
```

**7 reachable composed states.** The protocol-conformance evidence is in
what's **absent** from this list: **`Sending|Transmitting` is not
reachable.** That state would be reached if the firmware tried to issue a
`wr_ctrl_tx_start` write while the peripheral was already transmitting —
the peripheral spec's missing outbound transition makes that combination
structurally unreachable in the product.

Under the chaotic peripheral stub `mununu codesign couple` produces, every
peripheral state has self-loops on every label, so every firmware-side
state pairs with every peripheral-side state and the conformance evidence
is lost — the verifier sees a state space too over-approximated to
distinguish conformant from non-conformant firmware. **This is the
specific gap a real protocol spec closes.**

## Why this matters for the bigger picture

The C extractor (phases L1–L8 + L5.5) produces firmware automata on a
shared rendezvous-label alphabet. The label alphabet is sound and the
extraction is faithful at the access-ordering level — that piece works.
What was missing for "actually verify a driver against the protocol
shape" was a peripheral-side automaton that knew the protocol. This
file is one such automaton, hand-authored against the canonical UART
handshake. The same shape generalises to other peripherals: write a
small peripheral-spec CTXDSL automaton, compose it with the
extracted firmware automaton, and the verifier produces real
protocol-conformance evidence.

## Soundness posture

The peripheral model is an **over-approximation in the safe direction**:
the `tick` transition lets the peripheral make autonomous progress at
any time, modelling "eventually the transmission completes" without a
specific cycle bound. Any safety property proven under this model
transfers to any real silicon that conforms to the protocol-spec
ordering. The model does *not* model timing constraints, bit-field
semantics, or value tracking — those are gaps (4) and (5) from the
audit and are out of scope for this demonstration.

## What to read next

- `firmware.ctxdsl` — the original hand-authored firmware automaton
  this example reuses verbatim. Same state names, same labels.
- `register_map.json` — the source-of-truth for the rendezvous-label
  alphabet (`rd_status_tx_busy`, `wr_data_byte`, `wr_ctrl_tx_start`).
- `docs/archive/design/hw-sw-codesign-extraction.md` §C.5 — the soundness
  posture for asynchronous composition (Doc C's mandate for HW/SW
  codesign).
- Verification-gap audit answer (conversation 2026-05-15) — the
  ranked priorities of which gaps in the C-extraction-to-verification
  pipeline matter most for industrial use.
