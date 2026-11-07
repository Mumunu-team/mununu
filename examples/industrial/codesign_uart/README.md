# UART codesign — formal verification across the HW/SW boundary

> **Industrial example** for [Document C — HW/SW codesign extraction](../../../docs/archive/design/hw-sw-codesign-extraction.md) §C.4 and §C.10. Exercises the **register-map sidecar → coupling synthesis → composed verification** chain end-to-end against the mununu binary on `main`. Closes the four-document arc (A → B → D → C) with the capstone use case.

## What this example is

A small UART driver written as if you were extracting it from real firmware, composed with a UART peripheral's register map. The firmware drives the peripheral by reading STATUS, writing DATA, then setting CTRL.tx_start — the canonical `uart_send(byte)` flow that appears in every commercial MCU SDK.

```
┌─────────────────────────────────────────────────┐
│ Firmware (open, fully verifiable)               │
│  ├─ poll STATUS.tx_busy                         │
│  ├─ write DATA                                  │
│  └─ raise CTRL.tx_start                         │
└─────────────────────────────────────────────────┘
                    │
                    │  register-map sidecar
                    │  (`register_map.json`)
                    ▼
┌─────────────────────────────────────────────────┐
│ Peripheral RTL (closed-IP, chaotic stub)        │
│  ├─ CTRL  (RW)  : tx_start, enable              │
│  ├─ STATUS (RO) : tx_busy, rx_ready             │
│  └─ DATA  (RW)  : 8-bit payload                 │
└─────────────────────────────────────────────────┘
```

The coupling is the register-map sidecar (Doc C §C.3.2). Each field on each register maps to:
- An **SV signal** (e.g. `uart_inst.ctrl_reg[0]`), authoritative for the RTL side.
- A **C accessor** (e.g. `UART->CTRL.bit.tx_start`), authoritative for the firmware side.

`mununu codesign verify` reads both sides, synthesises an asynchronous composition over rendezvous labels (one per register-field read/write), and runs the standard `μ`-calculus evaluator. Counterexample traces can be tagged with `[SW]` / `[HW]` / `[BUS]` via the C3 trace origin classifier.

## What this example demonstrates

Every concept Document C introduces is exercised by `validate.sh`:

| Concept (Doc C §) | How the example exercises it |
|---|---|
| Register-map sidecar (§C.1, §C.3.2) | `register_map.json` declares CTRL / STATUS / DATA registers with per-field `sv_signal` + `c_accessor` mappings. Schema at [`tools/register_map_schema.json`](../../../tools/register_map_schema.json). |
| Coupling synthesis (§C.3, §C.7) | Step 1 of `validate.sh` runs `mununu codesign couple`. The output (transcript lines 6–68) is the CTXDSL fragment a user splices into a hand-authored context: 8 rendezvous labels + the chaotic peripheral stub + an asynchronous composition block. |
| Two-reactive-modules model (§C.2) | The composition is asynchronous (Doc C §C.5 — bus arbitration is non-deterministic; synchronous coupling is unsound for racy access). The product is 16 reachable states (4 peripheral × 4 firmware). |
| Controllability rule at the register boundary (§C.2, Doc A §4) | Firmware drives the `wr_*` labels (the firmware automaton declares them `controllable`). The peripheral's chaotic stub declares **nothing** as controllable — reads are environment-driven, writes are observed. This is enforced by the realiser; a buggier emitter that double-declared the labels would fail at realise time. |
| Safety / liveness soundness asymmetry (Doc A §2) | `init_reachable` and `safety_protocol_respected` both HOLD across all 16 composed states. `sending_reachable` is VIOLATED — the chaotic peripheral admits paths where it wedges in a `Busy_<reg>` state, preventing the firmware from progressing to `Sending`. Tightening this verdict requires authoring a peripheral latency contract (Doc A §3 HITL review surface, PR #20). |
| Three-surface parity (CLAUDE.md) | `mununu codesign verify` is wired in CLI + HTTP (`POST /api/v1/codesign/verify`). The UI surface lands in a companion mununu-ui PR. |

## What this example does *not* claim

Per the [CLAUDE.md claims-integrity rules](../../../CLAUDE.md):

- It does **not** claim mununu found a bug in any commercial UART. The register map is hand-authored from the §C.4 worked-example shape, not derived from any specific vendor silicon.
- It does **not** claim the peripheral stub is faithful to any specific UART IP. It is a *chaotic* stub — the conservative starting point under Doc A §2's chaotic-stub default.
- It does **not** prove that any real device is correct. The proof is conditional on the contracts the user authors on top of the chaotic stub; the contracts are conditional on the vendor honouring its datasheet.
- It does **not** auto-extract the firmware automaton from C source. That's Task C5 (libclang) — explicit follow-up. The firmware lives in `firmware.ctxdsl` as hand-authored CTXDSL, exactly the slice-1 path Doc C §C.7 recommends.
- The `VIOLATED` verdict on `sending_reachable` is **not** a bug in mununu nor in the firmware — it is the *expected* result under the chaotic-stub semantics. The README explicitly calls this out as the soundness story the codesign workflow is designed to make visible.

## How to run it

From the repo root:

```bash
./examples/industrial/codesign_uart/validate.sh
```

The script builds the `mununu` binary, runs every command exercised in the demonstration, strips per-run noise, and writes a byte-deterministic transcript to `transcript.txt`. Re-running against the same commit produces an identical transcript.

### What `validate.sh` does, step by step

1. **`mununu codesign couple register_map.json --firmware-member UartDriver`** — emits the CTXDSL coupling fragment from the register-map sidecar. The output is 60+ lines of CTXDSL: alphabet declarations for 8 rendezvous labels, a 4-state chaotic peripheral stub, and an asynchronous composition block. A user can splice this verbatim into a hand-authored `context { … }` block with their firmware automaton.

2. **`mununu codesign verify … --formula init_reachable`** — composes the register-map sidecar with `firmware.ctxdsl` and evaluates `init_reachable` over the codesign composition `UART_LITESystem`. Holds across all 16 composed states. Smoke test.

3. **`mununu codesign verify … --formula safety_protocol_respected`** — same composition, evaluates `nu X. ([] X)` (every reachable state respects the protocol). Holds across all 16 composed states. Sound under chaotic-stub default (Doc A §2): safety verdicts over an over-approximation transfer to any real system whose peripheral behaves at least as chaotically.

4. **`mununu codesign verify … --formula sending_reachable`** — evaluates `mu X. (Sending || <> X)` over the composition. **VIOLATED at the initial state.** The chaotic peripheral admits paths where it wedges in a `Busy_<reg>` state and the firmware cannot drive it back to a state where `Sending` is reachable. This is the kind of cross-boundary observation a per-side verifier cannot make. The honest remedy is to author a peripheral latency contract (`@mununu_guarantee = "G(start -> eventually done)"` per Document D §D.5) and re-run; that's the HITL stage-4 review flow shipped in PR #20.

The expected transcript is checked into `transcript.txt`.

## Files

| File | Purpose |
|---|---|
| `register_map.json` | The UART_LITE register-map sidecar (Doc C §C.3.2 schema). Three registers (CTRL, STATUS, DATA) with per-field SV signal + C accessor mappings. |
| `firmware.c` | The C source the firmware automaton corresponds to: `uart_send(uint8_t)` polls `STATUS.tx_busy`, writes `DATA.byte`, raises `CTRL.tx_start`. Uses the canonical `#define UART ((volatile UART_TypeDef *)0x40010000u)` macro form most MCU SDKs ship — the LLVM-IR backend handles it without an `extern` workaround. Carries `@mununu_guarantee` + `@mununu_assume` annotations the discovery pipeline lifts into proposed contract clauses. |
| `firmware.ctxdsl` | Hand-authored firmware automaton modelling `uart_send(byte)`: Init → Polling → Ready → Sending → Init. Uses the rendezvous label names `mununu codesign couple` produces. Adds the internal `tick` cycle marker and the system-reset transitions that the IR-based synthesiser cannot infer from the C source — those are environment events with no syntactic anchor in the firmware. |
| `validate.sh` | Reproduces the transcript end-to-end. Does **not** depend on clang — keeps the transcript reproducible on any host. |
| `extract-c-demo.sh` | Opt-in demonstration of the LLVM-IR-based C extractor (Doc C phase L3): runs `mununu codesign extract-c firmware.c --register-map register_map.json --synthesize-automaton`. Lifts the three register accesses (`rd_status_tx_busy`, `wr_data_byte`, `wr_ctrl_tx_start`), recognises the polling loop at the IR level (back-edge analysis), collapses the bit-field load-modify-store sequence into a single write, and synthesises a CTXDSL automaton with the canonical S0 → Loop0 ⤴ → S1 → S2 → S3 shape. Requires `clang` on `$PATH`. Covers Example 1 (macro base pointer) + Example 3 (bit-field RMW) from the plan. |
| `motivating_examples/` | Four real C files validating extractor claims end-to-end via real clang (CLAUDE.md claims-integrity rule #10). `example_2_typecast_register_access.c` exercises `*(volatile uint32_t *)0xADDR` (the Zephyr/FreeRTOS BSP convention). `example_4_helper_function_pointer_param.c` exercises pointer-parameter alias tracking — a helper takes the peripheral pointer as a parameter, the extractor follows the alloca-store-load round-trip clang emits at `-O0` and lifts the helper's STATUS read into the caller's automaton (phase L5.5). `example_5_isr_with_main_thread.c` exercises `@mununu_isr` + asynchronous composition emission. `example_6_multi_entry_driver.c` exercises `--driver-mode` dispatch over three entry points. Run `motivating_examples/validate-motivating-examples.sh` for a byte-deterministic transcript covering all four. |
| `transcript.txt` | The byte-deterministic transcript `validate.sh` produces; cited as evidence. |

## Provenance

This example was authored for Document C's capstone industrial demonstration. It is not derived from any specific commercial UART implementation. The register layout follows the Doc C §C.4 worked-example shape; the firmware automaton corresponds to the `uart_send(byte)` C source in §C.4. No real silicon was modelled.

## Related

- **[secure_boot_rom](../secure_boot_rom/)** — M1.c industrial example. RTL-only, no codesign. Demonstrates Doc A's chaotic-stub default + discharge graph + lightweight McMillan check.
- **[dual_frontend_soc](../dual_frontend_soc/)** — M2.c industrial example. RTL-only, both frontends. Demonstrates Doc B's "two pipelines, one IR" principle.
- **[tls_handshake](../tls_handshake/)** — M3.c industrial example. RTL-only, exercises Doc D's corpus + annotation grammar via the TLS handshake's AES + TRNG components.
- **This example** — M4.c. Adds the **HW/SW codesign** layer on top, completing the four-document arc.
