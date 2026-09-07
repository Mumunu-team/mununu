# LinkedIn post — A UART driver that might never finish (and what a model checker can tell you)

> **Draft for LinkedIn.** Target: 150–200 words. Source artefact: `examples/industrial/codesign_uart/`. Do not publish until the four-gate validation checklist in `publications/README.md` passes.

---

You write `uart_send(byte)`. Three lines: poll STATUS.tx_busy, write DATA, raise CTRL.tx_start. Your reviewer says obvious. Your tests pass. A week later a customer reports the device hangs intermittently. The firmware is correct against its written assumptions — and those assumptions don't include "the peripheral will eventually clear tx_busy."

I asked a formal model checker — mununu — what it actually knows about this driver. The answer is more interesting than pass/fail. Composed with a hand-authored register-map sidecar describing the peripheral, mununu finds 16 reachable states. Safety holds. The smoke test holds. But `sending_reachable` is VIOLATED at the initial state — the conservative peripheral model admits paths where the bus wedges and `uart_send` never returns.

The remedy is not a code fix. The firmware is correct. The remedy is to write down the missing assumption — the peripheral's progress guarantee — and make it part of the proof. That conversation is exactly what compositional verification is for.

Full write-up: [Substack link TBD].
Code: github.com/Mumunu-team/mununu.

#formalverification #embedded #firmware #verification
