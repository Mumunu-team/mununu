# `sv_multi_module` — SV multi-module composition via the sv-yosys path

> **Source of truth:** [`crates/mununu-core/src/verify/orchestrator.rs`](../../../crates/mununu-core/src/verify/orchestrator.rs) (`dispatch_sv_yosys`) — surface: CLI+API+UI.

> **Refused at present.** `mununu verify` and `POST /api/v1/verify` answer a
> project that sets `multi_module = true` with a validation error, until
> mununu#1248 and mununu#1249 are fixed: the per-module composition builds
> each module with its default parameter values, not the values the design
> instantiates it with, and leaves the top module's own logic out of the
> model, so a verdict on that model is not a verdict on the design. This
> example is kept as the option's example for when it comes back.
> `transcript.txt` holds what the tool prints for it today, which is the
> refusal; everything below describes the composition the option asks for,
> not something the tool runs now. To verify a design of this shape today,
> remove the key or set `multi_module = false`: the design is then lifted
> flattened, as one module, and the consumer's state is `u_consumer_state`
> in that model, not `u_consumer__state`.

A producer and a consumer SystemVerilog module composed through the KMTS
`sv-yosys` multi-module pipeline. A **top module**
(`producer_consumer_system_top.sv`) structurally instantiates both
submodules and wires the producer's `valid` output to the consumer's
`valid` input; the orchestrator elaborates each submodule to BTOR2, lifts
it to a KMTS, renames its port labels to the connected nets (discovered
from the top module's Yosys netlist), and synchronously composes the
instances. The composed automaton is named `Circuit`.

## What it demonstrates

- **sv-yosys multi-module composition.** One `[[sources]]` declaration
  with `files = [top.sv, module_a.sv, module_b.sv]` and
  `[sources.options] multi_module = true` (+ `top`) drives the
  multi-module entry point (`compose_sv_multi_module`).
- **Shared-net rendezvous.** The producer drives `valid`; the consumer
  reads it. The `consumer_reacts` property — the consumer eventually
  reaches BUSY (`u_consumer__state == 1`) — holds **only** because the
  rendezvous propagates the producer's `valid` to the consumer. It is
  vacuous if the rendezvous is dropped.
- **Instance-qualified predicates.** Composed-state valuations are
  numeric and instance-qualified (`u_consumer__state == k`), so
  properties reference a submodule's state by value across the product.

## Files

| File | Purpose |
|---|---|
| `producer_consumer_system_top.sv` | Top module instantiating `producer` + `consumer`, wiring the shared `valid` net. |
| `producer.sv` | RTL for the producer module (3-state FSM driving `valid`). |
| `consumer.sv` | RTL for the consumer module (3-state FSM reacting to `valid`). |
| `verify.toml` | Single source with `files = [top, producer, consumer]` + `multi_module = true`. |
| `validate.sh` | Runs `mununu verify` on the project and writes the transcript (guards on `yosys`, which the composition needs when it is back). |
| `transcript.txt` | Checked-in output: at present, the refusal. |

## Reproduce

Requires `yosys` (≥ 0.67, with `read_slang`) on `PATH` (`validate.sh` exits
with a clear error if it is missing). It is not bundled with the repo — the
`mununu-sva` image provides it.

```bash
bash examples/verify/sv_multi_module/validate.sh
```

## Authoring shape

For any SV multi-module composition under the verify framework, the
convention is a top module + the submodules it instantiates:

```toml
[[sources]]
id = "system"
adapter = "sv-yosys"
files = [
    "top.sv",        # structurally instantiates the submodules
    "module_a.sv",
    "module_b.sv",
    # ... one entry per submodule
]

[sources.options]
multi_module = true
top = "top"          # the top module's name
```

Instance connectivity is discovered from the top module's Yosys netlist
(`hier.json`), so the wiring lives in the RTL itself — no separate
connections sidecar.

## What this slice does not cover

- **Multi-bit shared buses.** The shared `valid` net is 1-bit, so the
  value-encoded rendezvous is exact. A multi-bit data bus would surface
  the §7.2 spurious-cross-data precision gap (see the R-MM plan).
- **Cross-source file references.** A `verify.toml` source references
  files in its own `files` list, not files belonging to another source.
  Each source is self-contained.
