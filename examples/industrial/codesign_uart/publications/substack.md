# A UART driver that might never finish — and what a model checker can tell you about it

> **Draft for Substack publication.** Source: `examples/industrial/codesign_uart/`. The transcript embedded below reproduces byte-for-byte by running `./examples/industrial/codesign_uart/validate.sh` against the pinned commit. **Do not publish until the four-gate validation checklist in `publications/README.md` passes.**

---

You wrote a UART driver. It looks like this:

```c
void uart_send(uint8_t byte) {
    while (UART->STATUS.bit.tx_busy)
        ; // poll until peripheral is idle
    UART->DATA = byte;
    UART->CTRL.bit.tx_start = 1;
}
```

Three lines, no surprises. Real firmware has thousands of these — the boring core of every embedded device on the planet. Your code review says "obvious". Your unit tests pass. You ship.

A week later a customer reports the device hangs intermittently on first boot. The hang isn't reproducible at your bench. You add a watchdog. The hang doesn't go away — it just looks like a reset cycle. You stare at the busy loop.

The bug isn't in the firmware. The firmware is *correct against its written assumptions*. The bug is that the written assumptions don't include "the peripheral will eventually clear `tx_busy`." A real UART always does. A model checker doesn't know that.

This post walks through what mununu — a formal model checker for compositional reactive systems — actually says about this driver when you ask it. The answer is more interesting than "it passes" or "it fails." It says "it depends on a vendor guarantee you have not written down." And then it tells you which guarantee.

## The example, end to end

The full setup lives at [`examples/industrial/codesign_uart/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/codesign_uart) in the mununu repository. You can reproduce every line by running `./examples/industrial/codesign_uart/validate.sh`. The transcript is byte-deterministic.

The setup has three pieces:

1. **The peripheral's register map.** Three registers: `CTRL` (RW), `STATUS` (RO), `DATA` (RW). Bit fields named the way a datasheet would name them: `tx_start`, `enable`, `tx_busy`, `rx_ready`. Each field is annotated with the SV signal it maps onto (`uart_inst.ctrl_reg[0]`) and the C accessor a firmware engineer would write (`UART->CTRL.bit.tx_start`). 30 lines of JSON.

2. **The firmware as a state machine.** Init → Polling → Ready → Sending → Init, plus an explicit reset edge from every state. The state names match what the C source's control flow would do: poll until peripheral is idle, write data, raise tx_start, return.

3. **A property to verify.** Three of them, in fact: `init_reachable` (smoke test), `safety_protocol_respected` (every reachable state respects the protocol), and `sending_reachable` (the firmware can actually send a byte).

Run it:

```text
$ mununu codesign verify register_map.json firmware.ctxdsl --formula init_reachable
codesign verify — peripheral `UART_LITE` (base 0x40010000), composition `UART_LITESystem`
  composed with firmware member(s): UartDriver

  formula `init_reachable` over `UART_LITESystem`
    states satisfying: 16/16
    initial states satisfying: 1/1
      initials: Idle|Init
      satisfying: Idle|Init
    verdict: HOLDS
```

Behind the scenes mununu has done something you would not normally do by hand. It took your register-map sidecar, generated a peripheral state machine from it — a *chaotic stub* with no behavioural assumptions — and composed that with your firmware state machine through an asynchronous product on the register-access labels (`rd_status_tx_busy`, `wr_data_byte`, `wr_ctrl_tx_start`). Sixteen reachable states emerged from the 4×4 product. The smoke test is that the system can always return to the initial state from any of them. It can.

Run the safety property:

```text
$ mununu codesign verify register_map.json firmware.ctxdsl --formula safety_protocol_respected
  formula `safety_protocol_respected` over `UART_LITESystem`
    states satisfying: 16/16
    initial states satisfying: 1/1
    verdict: HOLDS
```

Every reachable composed state respects the protocol. Useful, but not surprising — the formula is `nu X. ([] X)`, which is essentially well-formedness. Safety under over-approximation is sound: if it holds in the chaotic-stub composition, it holds in any real device whose peripheral behaves at least as chaotically as the stub.

Now the interesting one. Run `sending_reachable`, which asks: "is there a path from `Init` to `Sending`?"

```text
$ mununu codesign verify register_map.json firmware.ctxdsl --formula sending_reachable
  formula `sending_reachable` over `UART_LITESystem`
    states satisfying: 8/16
    initial states satisfying: 0/1
      initials: Idle|Init
    verdict: VIOLATED at initial state(s)
```

**VIOLATED.** Half the composed states satisfy the property — they can reach Sending. The initial state cannot.

## What the verdict is actually saying

Half the states reach Sending. The other half are wedged. The initial state is in the wedged half.

To see why, think about what the chaotic peripheral stub admits. Every register access is a rendezvous between firmware and peripheral. When firmware fires `rd_status_tx_busy`, the peripheral can transition `Idle → Idle` (status read returned, peripheral does nothing) or `Idle → Busy_status` (status read returned, peripheral now in a transient state). The peripheral chose. Mununu does not assume which.

When firmware fires `wr_data_byte`, the peripheral can transition `Idle → Busy_data` or `Busy_data → Idle`. Same chaos: no assumption about ordering or duration.

Now follow a bad path:

```
(Idle, Init)
  → fire wr_data_byte
  → peripheral chooses Idle → Busy_data
  → firmware advances Ready → Sending
state = (Busy_data, Sending)

  → fire wr_ctrl_tx_start
  → peripheral chooses Busy_data → … hmm, no such transition.
```

`Busy_data` is a state the peripheral enters on a data access. Looking at the chaotic stub, the only transitions out of `Busy_data` are on labels for the DATA register: `wr_data_byte` and `rd_data_byte`. There is no `wr_ctrl_tx_start` edge out of `Busy_data`. Asynchronous composition requires that whichever side fires the rendezvous label has a transition on it. The peripheral does not. The firmware's `Sending → Init` transition is blocked.

The composition is deadlocked at `(Busy_data, Sending)`. The firmware will sit there forever.

Mununu does not know that real silicon will fire some peripheral-internal tick that returns `Busy_data → Idle`. The chaotic stub admits the *worst-case* peripheral behaviour, which is "never make progress out of `Busy_data`." Under that worst case, your `uart_send` does not return.

Three observations about this:

**The verdict is correct given what was modelled.** The chaotic stub really does admit the wedged path. Nothing in the model says it cannot.

**The verdict is over-pessimistic given what is true.** Real silicon has internal ticks. The chaotic stub does not.

**The verdict is useful.** It says exactly what assumption is missing: "the peripheral makes progress out of `Busy_data`." Until you write that assumption down, no model checker can prove your driver terminates.

This is the cross-boundary observation that pure per-side verification would have missed. The firmware in isolation looks fine. The peripheral in isolation looks fine. The product surfaces the dependency.

## The remedy is a contract, not a fix

You do not change the firmware. The firmware is correct. You add a *guarantee on the peripheral side* and tell mununu to assume it.

The vendor's datasheet — or the OpenTitan UART spec, or the relevant CMSIS-SVD entry — almost certainly says something like "every register access completes within K cycles." That is the missing assumption. In mununu's contract framework you write it as a `@mununu_guarantee` annotation on the peripheral RTL:

```verilog
(* mununu_guarantee = "G(start -> eventually done)" *)
module uart_lite (...);
```

The discovery pipeline reads the annotation, the HITL stage-4 review surfaces it for your sign-off (it is a *trust* decision — the vendor's word becomes part of your proof), and the discharge graph wires it into the verification. With the guarantee in place, `sending_reachable` will hold.

Note what just happened. The verdict went from `VIOLATED` to `HOLDS` *not* by changing the firmware, *not* by changing the peripheral, but by writing down a previously-tacit assumption and making it part of the model. That is the assume-guarantee discipline in industrial form. The proof now says, explicitly: "the firmware sends correctly *if* the peripheral honours its progress guarantee." That conditional is the right shape — it transfers to any real device whose peripheral does honour the guarantee.

## What this example is honestly *not*

A few things this post has not claimed, and will not claim, because they would be false:

- It is **not** a claim that mununu found a bug in any commercial UART. The register map is hand-authored from a stylised example. No specific vendor silicon was modelled.
- It is **not** a claim that the peripheral state machine is faithful to any specific UART IP. It is a *chaotic stub* — the conservative starting point under mununu's chaotic-stub default. A real verification would replace it with a vendor-supplied contract.
- It is **not** a claim that the firmware was extracted from real C source. The firmware automaton is hand-authored CTXDSL. Auto-extracting it from a real `.c` file via libclang is the next slice of mununu's codesign work and has not landed yet.
- The `VIOLATED` verdict on `sending_reachable` is **not** a bug report. It is the *expected* outcome under the conservative chaotic-stub default. It is the model telling you which assumption you have not yet declared.

The whole point of the exercise is that mununu makes that conversation explicit. The chaotic stub is the default, not the punchline. The user reads the verdict and authors the assumption.

## How mununu made this possible

The mununu features the worked example exercises are mostly ones that have been there for a few releases:

The **register-map sidecar** is the new piece that makes codesign tractable. It is a small JSON file mapping each register field to both an SV signal and a C accessor. It is descriptive, not prescriptive — the schema names what *is*, not what mununu *enforces*. Tools like IP-XACT, SystemRDL, and CMSIS-SVD all describe the same shape; mununu intentionally chose a superset so importers from any of those formats are a one-pass mechanical mapping.

`mununu codesign couple` reads the sidecar and emits a CTXDSL fragment — alphabet, peripheral stub, asynchronous composition block — that you splice into your firmware CTXDSL. That is the "connecting tissue" the codesign workflow needs. `mununu codesign verify` does the splicing for you, realises the composed context, and evaluates a formula. Three surfaces — CLI, HTTP, and a Codesign tab in mununu's UI — share the same payload shape.

The *async composition* is a soundness choice. Synchronous composition (one-step rendezvous on every shared label) is unsound for properties about racy access — bus arbitration is non-deterministic, and the verifier must respect that. The asynchronous composition admits the bus-level non-determinism honestly.

The *chaotic-stub default* is the heart of the whole story. It is the most general environment for a black-box module — every behaviour the interface admits, at every timing. Pessimism is mandatory because the alternative is silent over-approximation, which is unsound. The trade-off is that liveness verdicts under chaos are vacuously violated until the user authors a progress guarantee. That trade-off is the right one: the user knows exactly what assumption they are accepting.

The *interleaved counterexample tagger* (when the verifier produces a counterexample trace, not the bare verdict we showed above) classifies each trace step as `[SW]` / `[HW]` / `[BUS]` so a debugger can see which side caused which step. The same library is what would tag the wedged path on `sending_reachable` in a follow-up slice that wires counterexamples through.

None of these are individually novel. The combination — a register-map sidecar, an asynchronous composition, an explicit chaotic-stub default, a discharge-graph-driven trust boundary, and one verifier that runs the whole thing — is the integration that mununu adds.

## Where this fits

This is the closing post of a four-post arc. The earlier three:

- [Black-box modules in compositional extraction](../../secure_boot_rom/publications/substack.md) — the chaotic-stub default, the discharge graph, the lightweight McMillan check.
- [Two RTL frontends, one IR](../../dual_frontend_soc/publications/substack.md) — why mununu has two SystemVerilog pipelines and what stays separate.
- [A contract corpus for hardware verification](../../tls_handshake/publications/substack.md) — corpus + source-comment annotation grammar + the `@mununu_*` tag vocabulary.

The codesign workflow uses all three. The chaotic stub is from post 1. The peripheral RTL is either custom-SV or yosys-extracted per post 2. The `@mununu_guarantee` annotation that resolves the `sending_reachable` violation is from post 3. The capstone here is putting them together to verify a UART driver that nobody could have verified per-side.

The Document C design doc for this work lives at [`docs/design/hw-sw-codesign-extraction.md`](https://github.com/Mumunu-team/mununu/blob/main/docs/design/hw-sw-codesign-extraction.md). The implementation plan, the worked example, the soundness considerations specific to codesign, and the open questions for follow-up are all there.

The code: [github.com/Mumunu-team/mununu](https://github.com/Mumunu-team/mununu).
The example: [`examples/industrial/codesign_uart/`](https://github.com/Mumunu-team/mununu/tree/main/examples/industrial/codesign_uart).

— Mariano Cerrutti
