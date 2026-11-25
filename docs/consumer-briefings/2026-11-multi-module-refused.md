# Consumer briefing: `multi_module = true` on an `sv-yosys` source is refused until its two defects are fixed (2026-11)

> Source of truth: [`VerifyConfig::validate`](../../crates/mununu-core/src/verify/config.rs#L868) and [`asks_for_per_module_composition`](../../crates/mununu-core/src/verify/config.rs#L703) — surface: CLI+API — `mununu verify`, `mununu verify --print-alphabet`, `POST /api/v1/verify`

**Audience:** anyone whose `verify.toml` (or the `config` of a `POST /api/v1/verify` request) has an `sv-yosys` source with `[sources.options] multi_module = true`.

> **Later (2026-11).** The first of the two defects below, mununu#1248, is fixed ([briefing](2026-11-per-instance-lift.md)). The option stays refused for the second, mununu#1249, and the refusal's text now names only that one; the text quoted below is the one this change printed.

## TL;DR

- **Only a project that sets `multi_module = true` on an `sv-yosys` source is affected.** The option is off by default; without it nothing changes.
- **Such a project is now refused, with a validation error, before anything is read or lifted.** It used to run and print verdicts. The refusal is on `mununu verify`, `mununu verify --print-alphabet` and `POST /api/v1/verify` (a 400 whose message is the same text).
- **Why: the verdicts of that run were not about the design.** The option asks for the per-module composition: each module is lifted by itself and the instances are composed. That composition has two open defects:
  - it builds each module with its **default parameter values**, not the values the design instantiates it with (mununu#1248). On a design whose instance overrides a parameter, a safety property the design violates was reported `SATISFIED`;
  - it leaves the **top module's own logic** out of the model: only the instances are composed, and a net the top drives reads as a free input of the instance it feeds (mununu#1249). On a design whose top has logic of its own, a reachability property the design does not have was reported `SATISFIED`.
- **Treat the verdicts of earlier `multi_module = true` runs as unconfirmed** and run them again flattened. The two defects change the model when an instance overrides a parameter or when the top module has logic of its own; nothing in an old report says whether either applied.
- **What to do: remove the key, or set `multi_module = false`.** The design is then lifted flattened, as one module, which is the default and has neither defect. On the example project (`examples/verify/sv_multi_module`) the atom `u_consumer__state` of the composition is `u_consumer_state` in the flattened model. A property whose atom names a variable the flattened model does not have is refused, and the refusal lists the variables the model has (measured 2026-11-24 in the `mununu-sva` image, on this change).
- **The option comes back when the two issues are fixed.** The refusal is containment; it fixes neither.

## What the refusal looks like

`mununu verify` and `mununu verify --print-alphabet` print this and exit with code 1. `--fail-on none` does not change the exit code, and under `--json` the same text is printed and no JSON report. `POST /api/v1/verify` answers 400 and its message is the same text: the handler and the command both print the one error (read from the handler; not sent over HTTP for this briefing). `system` is the `id` of the source.

```text
verify config has 1 validation issue(s):
  - [[sources]] `system`: options.multi_module = true is refused — the per-module composition builds each module with its default parameter values, not the values the design instantiates it with (mununu#1248), and leaves the top module's own logic out of the model (mununu#1249), so a verdict on that model is not a verdict on the design; both are mununu defects, not something to change in the design, and the option comes back when they are fixed. Remove the key or set `multi_module = false`: the design is then lifted flattened, as one module, and a property names the signals of that model instead of `<instance>__<signal>`
```

Printed by the binary built from this change, on the example project, 2026-11-24, `mununu-sva` image.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A lane whose project file sets `multi_module = true` on an `sv-yosys` source fails at validation instead of printing verdicts. | Remove the key or set it to `false`, rename the atoms of the properties to the flattened model's signals, run again. Treat that lane's earlier verdicts as unconfirmed. If no lane sets the option: none. |
| **monono** | Same. | Same. |
| **mununu-ui** | The typed client of `POST /api/v1/verify` gets a 400 with the message above for such a project. No shape change. | None in the client. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the refusal. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Change:** pending merge — see branch `fix/refuse-multi-module-1248-1249`. Issues: [mununu#1248](https://github.com/Mumunu-team/mununu/issues/1248), [mununu#1249](https://github.com/Mumunu-team/mununu/issues/1249); both stay open. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
- **Found by:** the two issues, each measured in the `mununu-sva` image on a small design (2026-11-23).
- **engine:** none in the defects, which are in the lift and the composition (yosys-slang to BTOR2 per module, KMTS lift, synchronous composition). The verdicts that were wrong came from the verify framework's explicit evaluation of the composed model, which answered correctly for the model it was given. The refusal runs before any lift or engine.
- **Tests:** `x1248_the_per_module_composition_is_refused_exactly_where_it_would_run` (the table: the option's values and the adapters), `x1248_the_refusal_names_the_source_the_option_and_a_key_the_file_can_set`, and through the two entry points `x1248_verify_refuses_the_per_module_composition_before_reading_a_file`, `x1248_the_alphabet_inspection_refuses_it_too`, `x1248_the_flattened_lift_of_the_same_project_is_not_refused`; and the remedy followed once, in the image: `x1248_the_remedy_the_refusal_names_gives_the_example_its_verdicts` (the example project lifted flattened gets its verdicts).
- **API schemas:** no shape change.

## Not covered here

- **`mununu sv emit-btor2-per-module`** wrote one BTOR2 file per module type, each at the module's default parameters, when this refusal was written. It gives no verdict. The fix of mununu#1248 changes what it writes: each instance as the design elaborates it ([briefing](2026-11-per-instance-lift.md)). The refusal stays until mununu#1249 is fixed too.
- **The example `examples/verify/sv_multi_module`** keeps `multi_module = true` as the option's example; its transcript is the refusal until the fix.
- **The end-to-end test of the composition under `verify`** is removed with the refusal and returns with the fix. The composition's own tests stay.
