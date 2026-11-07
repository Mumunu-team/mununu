# Nordic nRF52840 TWIM (I2C) — formal verification across the HW/SW boundary

> Industrial flagship for Document C — HW/SW codesign extraction. Anchored to a
> real vendor's silicon spec (Nordic nRF52840 SVD) and a real vendor's
> production HAL (`nrfx_twim.c`). The whole pipeline — register-map sidecar →
> coupling synthesis → composed verification — runs on the upstream artefact
> and ten protocol-conformance properties verify against a hand-authored
> firmware automaton derived from the HAL's `twim_xfer` source.

## Provenance

| Artefact | Upstream path | Commit | Licence |
|---|---|---|---|
| `upstream/nrf52840.svd` | [`bsp/stable/mdk/nrf52840.svd`](https://github.com/NordicSemiconductor/nrfx/blob/master/bsp/stable/mdk/nrf52840.svd) | `0883a272c34004697dd56dfa44f6e2d0f8705689` | BSD-3-Clause |
| `upstream/nrfx_twim.c` | [`drivers/src/nrfx_twim.c`](https://github.com/NordicSemiconductor/nrfx/blob/master/drivers/src/nrfx_twim.c) | `0883a272c34004697dd56dfa44f6e2d0f8705689` | BSD-3-Clause |
| `upstream/nrfx-LICENSE.txt` | [`LICENSE`](https://github.com/NordicSemiconductor/nrfx/blob/master/LICENSE) | `0883a272c34004697dd56dfa44f6e2d0f8705689` | BSD-3-Clause |
| `upstream/nrfx_twim_buggy.c` | derived from `nrfx_twim.c` — modified in this directory only | — | BSD-3-Clause + planted-bug header |

Retrieval date: 2026-05-14. See [`upstream/ATTRIBUTION.md`](upstream/ATTRIBUTION.md) for the full attribution record, including the correction to Plan 1's
Apache-2.0 assumption (nrfx is BSD-3-Clause, not Apache-2.0).

**Tooling gaps surfaced during this work — both honest and documented inline:**

1. **`mununu codesign import-svd` does not handle Nordic's `<cluster>` vendor extension.** The full nRF52840 SVD contains peripherals (e.g. `ACL`) whose registers are nested inside `<cluster>` elements; the importer aborts on the first one with `peripheral 'ACL' has no registers (or all registers failed to parse)`. Workaround: a TWIM0-only slice of the SVD was hand-extracted and run through the importer. The slice still tripped over the `TXD`/`RXD` EasyDMA `<cluster>` blocks (silently — they were dropped, not erroring). Those six registers were added back by hand against the §6.30 spec; the soundness note is recorded in `register_map.json`'s top-level `description`. **Follow-up issue**: extend `svd_import.rs` to flatten `<cluster>` into prefixed register names.

2. **`mununu codesign extract-c` failed against `upstream/nrfx_twim.c`** because clang requires `<nrfx.h>` plus the CMSIS / Zephyr include tree to parse the upstream source. **Resolution shipped**: a self-contained `firmware.c` / `firmware_buggy.c` in this directory models the same `twim_xfer` register-write sequence using literal MMIO addresses (`*(volatile uint32_t *)0x40003000u + offset`). The LLVM-IR-based extractor processes these files end-to-end (real clang, real IR, real CTXDSL). The synthesised CTXDSL is committed at `firmware.ctxdsl` / `firmware_buggy.ctxdsl`; regenerate via `./regenerate-ctxdsl.sh`. **Follow-up issue still open**: extract-c against the *upstream* source would still need a `--include-stub` mode or a Nordic SDK ingestion recipe. See "CTXDSL provenance" below.

## Planted-bug disclosure (Claims Integrity Rule 2)

> This example contains a deliberately-introduced bug for demonstration
> purposes. It is a pattern study; it is **NOT** a finding about Nordic
> silicon. The pattern is anchored to public errata pedigree (Nordic
> Errata 211 on TWIM frequency-change ordering and the broader family of
> register-write-ordering anomalies on early nRF52 revisions). The
> upstream `nrfx_twim.c` correctly orders register writes before task
> triggers; the `nrfx_twim_buggy.c` variant in this example inverts that
> order to demonstrate mununu's protocol-conformance verification
> capability.

The buggy C source carries the same disclosure verbatim as an inline comment at
the mutation site (`upstream/nrfx_twim_buggy.c`, near line 534).

## Severity statement (Claims Integrity Rule 3)

> This example demonstrates verification capability against a real
> protocol-conformance bug class — register/task write-ordering at the
> firmware/peripheral boundary. It is **not** a vulnerability report.
> The verification verdict applies to the abstracted state-machine
> model documented in this directory, not to production Nordic silicon.

## What this example demonstrates

Real connected-device firmware does not fail at the algorithm. It fails at the
seam where the firmware driver hands control to a peripheral's hardware state
machine — write the buffer pointer one cycle too late and the EasyDMA prefetch
samples uninitialised memory. The bug class is well-known; the public Nordic
errata catalogue lists multiple instances on early nRF52 silicon.

The example shows mununu observing exactly that bug class:

- **Clean firmware** (`firmware.ctxdsl`, derived from `upstream/nrfx_twim.c` by
  inspection): all ten properties HOLD. The hand-authored FSM enforces the
  pointer-before-task ordering by construction, and the verifier confirms the
  property holds across all 101 composed states of `TWIM0System`.
- **Buggy firmware** (`firmware_buggy.ctxdsl`, derived from
  `upstream/nrfx_twim_buggy.c` by inspection): the `data_pointer_set_before_tasks`
  property VIOLATES at the initial state of the 82-state composed system. The
  basic safety property (`nu X. ([] X)`) still holds, because the bug shows up
  as an *additional* admissible edge in the firmware FSM, not as a structurally
  malformed automaton. That asymmetry is the whole point of the codesign
  verifier — it sees the precondition violation that a per-side property
  checker would miss.

`mununu codesign couple` emits 89 rendezvous labels from the 26-register
TWIM0 sidecar; the firmware FSM uses ~24 of those. The composition is
asynchronous, per Document C §C.5: bus arbitration is non-deterministic on
real silicon, so synchronous one-step rendezvous is unsound. Asynchronous
composition is the only correct coupling primitive for codesign verification.

## How to reproduce

```bash
# From the repo root:
cargo build --release -p mununu-cli
./examples/industrial/codesign_nrf52_twim_i2c/validate.sh
```

`validate.sh` runs every relevant `mununu codesign couple` and
`mununu codesign verify` invocation, strips per-run noise (timestamps and ANSI
colour), and writes a byte-deterministic transcript to
[`transcript.txt`](transcript.txt). Re-running against the same commit
reproduces the transcript exactly.

## Expected output

[`transcript.txt`](transcript.txt) (597 lines). Summary:

| Phase | Step | Verdict |
|---|---|---|
| Coupling | emit rendezvous fragment | 89 labels, 26-state peripheral stub |
| Clean | `init_reachable` | HOLDS (101/101) |
| Clean | `safety_protocol_respected` | HOLDS (101/101) |
| Clean | `twim_enable_before_tasks` | HOLDS (101/101) |
| Clean | `data_pointer_set_before_tasks` | HOLDS (101/101) |
| Clean | `no_data_after_error` | HOLDS (101/101) |
| Clean | `stop_after_last_byte_with_shorts` | HOLDS (101/101) |
| Clean | `error_event_cleared_before_retry` | HOLDS (101/101) |
| Clean | `shorts_only_between_transactions` | HOLDS (101/101) |
| Clean | `frequency_only_when_disabled` | HOLDS (101/101) |
| Clean | `suspend_resume_ordering` | HOLDS (101/101) |
| Buggy | `data_pointer_set_before_tasks` | **VIOLATED** at initial state (6/82 satisfy) |
| Buggy | `safety_protocol_respected` | HOLDS (82/82) — see note above |
| Buggy | `data_pointer_set_before_tasks` (`--json`) | structured JSON with `verdict: violated_at_initial` |

## Property catalogue

| Property | What it checks | Source automaton |
|---|---|---|
| `init_reachable` | The `Reset` state is reachable from every state (well-formedness smoke test). | TwimDriver |
| `safety_protocol_respected` | Every reachable composed state respects the protocol — the standard `nu X. ([] X)` over the codesign composition. | TWIM0System |
| `twim_enable_before_tasks` | `ENABLE` was asserted before any `TASKS_STARTTX` / `TASKS_STARTRX`. | TwimDriver |
| `data_pointer_set_before_tasks` | `TXD.PTR` (or `RXD.PTR`) was written before the corresponding task-trigger. | TwimDriver |
| `no_data_after_error` | After `EVENTS_ERROR` rises, `TASKS_RESUME` is not enabled until the error is explicitly cleared. | TwimDriver |
| `stop_after_last_byte_with_shorts` | With `SHORTS.LASTTX_STOP=1`, observing `EVENTS_STOPPED` is the canonical successor to `EVENTS_LASTTX`. | TwimDriver |
| `error_event_cleared_before_retry` | `EVENTS_ERROR` must be cleared (firmware writes 0 to the flag) before any subsequent task-trigger or resume. | TwimDriver |
| `shorts_only_between_transactions` | `SHORTS` writes happen only when no task is in flight. | TwimDriver |
| `frequency_only_when_disabled` | `FREQUENCY` writes only when `ENABLE=0` (i.e. only at `Reset`). | TwimDriver |
| `suspend_resume_ordering` | `TASKS_SUSPEND` blocks `EVENTS_TXSTARTED` until `TASKS_RESUME` issues. | TwimDriver |

## Soundness notes

Each abstraction is documented inline as a `// SOUNDNESS:` comment in the
corresponding `.ctxdsl` or as a `description` field in `register_map.json`.
The substantive ones are:

- **Chaotic peripheral stub.** `mununu codesign couple` emits a chaotic
  peripheral stub: for every register field, the peripheral admits every
  write-then-busy-then-idle interleaving. This is the conservative starting
  point under Document A §2. Safety properties (anything of the form
  `nu X. (P ∧ [] X)`) transfer from the chaotic stub to any real Nordic
  peripheral that behaves at least as chaotically; liveness properties do
  not transfer without a contract that constrains the stub.
- **Register-map slicing.** The TWIM0 register-map sidecar was built from a
  hand-sliced SVD because the upstream importer aborts on Nordic's
  `<cluster>` vendor extension. The slice covers all 20 TWIM0 registers
  the importer can handle plus 6 hand-added TXD/RXD cluster registers; the
  hand-additions follow §6.30 of the nRF52840 Product Specification. See
  the `description` field in `register_map.json` for the verbatim disclosure.
- **Hand-authored firmware automaton.** `mununu codesign extract-c` failed
  against `nrfx_twim.c` because the upstream HAL `#include`s `<nrfx.h>` and
  the CMSIS / Zephyr include tree. The hand-authored firmware FSM in
  `firmware.ctxdsl` is faithful to `twim_xfer` (`upstream/nrfx_twim.c:441-665`)
  at the level of register-write sequencing, task-trigger ordering, event-flag
  polling, error-recovery, and suspend/resume re-entry. It elides EasyDMA
  pointer-validity checks, the busy-flag re-entrancy guard, interrupt-priority
  configuration, and the TX/RX list-mode extensions; the elisions are
  conservative for the safety properties verified.
- **Buggy firmware FSM.** `firmware_buggy.ctxdsl` adds task-trigger edges from
  `ShortsSet` (not just `PointerSet`), mirroring the inverted order in
  `upstream/nrfx_twim_buggy.c`. The FSM still admits the correct (pointer-first)
  path, so it is a strict superset of the clean firmware's behaviour — exactly
  the over-approximation a real buggy firmware would exhibit.
- **Asynchronous composition.** Document C §C.5: bus arbitration is
  non-deterministic, so synchronous coupling is unsound for racy register
  access. `mununu codesign verify` enforces the asynchronous-only constraint.

## Three-surface parity

This flagship exercises the **CLI** surface end-to-end (`mununu codesign couple`,
`mununu codesign import-svd`, `mununu codesign extract-c`, `mununu codesign verify`).
The matching **HTTP API** routes (`POST /api/v1/codesign/verify` and peers) and
**UI** panel (`CodesignPanel` in `mununu-ui`) were validated through the existing
[`codesign_uart`](../codesign_uart/) example and are unchanged here. This
flagship adds no new surfaces — it is a vendor-anchored, real-source
demonstration of the surfaces already in place.

## Files

| File | Purpose |
|---|---|
| `upstream/nrf52840.svd` | Upstream Nordic SVD (BSD-3-Clause), commit `0883a272c34004697dd56dfa44f6e2d0f8705689`. |
| `upstream/nrfx_twim.c` | Upstream Nordic TWIM HAL (BSD-3-Clause), same commit. |
| `upstream/nrfx_twim_buggy.c` | Modified copy with a single planted bug in the `NRFX_TWIM_XFER_TXRX` switch arm. |
| `upstream/nrfx-LICENSE.txt` | Verbatim BSD-3-Clause licence text from the upstream `nrfx` repo. |
| `upstream/ATTRIBUTION.md` | Provenance record: source URL, commit SHA, retrieval date, licence, files derived. |
| `register_map.json` | TWIM0 register-map sidecar. 26 registers; the 20 SVD-derived ones plus 6 hand-added TXD/RXD cluster registers. |
| `firmware.c` / `firmware_buggy.c` | Self-contained C sources that model `twim_xfer`'s register-write sequence using literal MMIO addresses (`*(volatile uint32_t *)BASE + offset`). Compiled by real clang and consumed by the LLVM-IR-based `mununu codesign extract-c`. The buggy variant inverts the TASKS_RESUME / buffer-set ordering in `twim_txrx` (Errata 211 pattern). |
| `firmware.ctxdsl` / `firmware_buggy.ctxdsl` | **Auto-extracted** CTXDSL — synthesised by `regenerate-ctxdsl.sh` from `firmware.c` / `firmware_buggy.c`. Contains automata only (no mu_formulas). Re-run the script to regenerate after touching the C sources. CLAUDE.md claims-integrity rule #10: real-clang extractor output, not a hand-authored placeholder. |
| `firmware.hand_authored.ctxdsl` / `firmware_buggy.hand_authored.ctxdsl` | The original hand-authored automata + mu_formulas. Kept because the protocol-conformance properties (`init_reachable`, `data_pointer_set_before_tasks`, `safety_protocol_respected`, …) reference the hand-authored state names (`Reset`, `Idle`, `Polling`, …). The synthesised state names (`S0`, `S1`, `Loop0`, `Calling_<fn>`, …) differ, so the formulas don't transfer 1:1. `validate.sh` uses these for the verification steps; the auto-extracted files are the extractor's evidence-of-record. |
| `regenerate-ctxdsl.sh` | Re-runs `mununu codesign extract-c` against `firmware.c` / `firmware_buggy.c` and overwrites `firmware.ctxdsl` / `firmware_buggy.ctxdsl`. The single source of truth for the auto-extracted CTXDSL. |
| `validate.sh` | Reproducible byte-deterministic verification run. Uses `firmware.hand_authored.ctxdsl` for the verify steps (because that's where the protocol-conformance formulas live). |
| `transcript.txt` | The transcript `validate.sh` produces; cited as evidence. |

## CTXDSL provenance

Two CTXDSL representations of the same firmware ship in this directory; they capture **two different abstraction stances** and the README is explicit about which is which:

1. **Auto-extracted** (`firmware.ctxdsl`, `firmware_buggy.ctxdsl`). Synthesised by the LLVM-IR-based C extractor from `firmware.c` / `firmware_buggy.c`. Each function becomes one automaton (`Twim_init`, `Twim_tx`, `Twim_rx`, `Twim_txrx`) on the rendezvous-label alphabet `coupling::register_map_labels` emits. A `Driver` automaton dispatches non-deterministically to each entry. **State names are synthesised** (`S0`, `S1`, `Loop0`, …). This is the canonical extractor evidence — CLAUDE.md claims-integrity rule #10's "real C source, real clang, real extraction" target.

2. **Hand-authored** (`firmware.hand_authored.ctxdsl`, `firmware_buggy.hand_authored.ctxdsl`). Crafted by a careful reading of upstream `nrfx_twim.c`'s `twim_xfer` (lines 441-665). Smaller state space than the auto-extracted form (deliberate abstraction), state names match the C-level concepts (`Reset`, `Idle`, `Polling`, `Sending`, …), and crucially the file carries the **mu_formulas** the example's verification properties depend on. `validate.sh`'s verify steps point at this variant because that's where the formulas live.

A follow-up unification would: (a) author mu_formulas against the auto-extracted state names, or (b) teach the synthesiser to inherit state-name hints from the C source (e.g. via `@mununu_state` annotations on labelled blocks). Both are deferred work; this README is the bookkeeping until then.

## References

- [Nordic nRF52840 Product Specification §6.30 TWIM](https://docs.nordicsemi.com/bundle/ps_nrf52840/page/twim.html) — register layout, EasyDMA semantics, task/event ordering.
- [Nordic Errata 211](https://docs.nordicsemi.com/bundle/errata_nRF52840_EngB/page/ERR/nRF52840/EngineeringB/latest/anomaly_840_211.html) — TWIM frequency-change ordering anomaly; pedigree for the planted-bug pattern in this example.
- [Upstream nrfx repo](https://github.com/NordicSemiconductor/nrfx) — source of `nrf52840.svd` and `nrfx_twim.c`.
- [`docs/archive/design/hw-sw-codesign-extraction.md`](../../../docs/archive/design/hw-sw-codesign-extraction.md) — Document C, which this example exercises end-to-end.
- `pre-deal-shipping-nrf52-twim-flagship.md` (a planning note, not tracked) — the execution plan this directory implements (Phase 1).
- [`examples/industrial/codesign_uart`](../codesign_uart/) — the original UART codesign example whose shape this flagship reuses (validate.sh + transcript.txt; same three-surface story).
