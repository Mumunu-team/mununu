# Consumer briefing: `sv emit-btor2-per-module` lifts each instance as the design elaborates it (2026-11)

> Source of truth: [`translate_sv_per_module`](../../crates/mununu-core/src/adapter/yosys/mod.rs#L2614) and [`sv_emit_btor2_per_module`](../../crates/mununu-cli/src/main.rs#L8151) — surface: CLI+API — `mununu sv emit-btor2-per-module` (CLI); the composition behind `multi_module = true` (`mununu verify`, `POST /api/v1/verify`) reads the same lift and stays refused

**Audience:** anyone who runs `mununu sv emit-btor2-per-module`, reads the BTOR2 files it writes, or parses what it prints; and anyone who reads the text of the `multi_module = true` refusal.

## TL;DR

- **The verb wrote each module type at its default parameter values (mununu#1248).** An instance the design gives other values was written as another module, with exit code 0 and nothing said. Every instance below the top is now lifted as the design elaborates it: with its parameter overrides, `defparam`s, type parameters and the generate branches they choose.
- **File names.** A module type whose instances are all the same model is still written as `<type>.btor2`. A type whose instances differ gets one file per distinct model, named after the first instance of each: `<type>$<instance path>.btor2` (`child$top.u_a.btor2`). Such a name can carry `$`, `.`, `[`, `]` and `-`; quote it in a shell. The yosys log of each file is beside it as before (`<name>.yosys.log`).
- **The content of `<type>.btor2` changes for a type the design instantiates with other values than its defaults.** It is now the model of the instances. For a type instantiated at its defaults the model is the same one.
- **The verb prints one more line per model:** `    instances: top.u_a, top.u_c`, under the `  <name> -> <path> (…)` line, naming the instances the file is the model of. A design with no submodule, where the top itself is written, has no such line.
- **A file an earlier run left is not removed.** A directory that held `child.btor2` and now gets `child$top.u_a.btor2` and `child$top.u_b.btor2` still holds the old `child.btor2`. Use an empty directory, or delete the files of the earlier run.
- **A module with an interface port is lifted, and so is a module type whose own name has a `$`.** Before, the verb failed on both with an error from the reader: the type read as a top of its own has an unconnected interface port, and of `child$v2` only `child` was taken for the type's name.
- **Cost: one yosys run per instance**, where it was one per module type. Each run is capped at 60 s as before.
- **`--output-dir` with a `"` or a line break in its path is no longer refused.** The directory no longer reaches a yosys script.
- **`multi_module = true` stays refused, and the text of the refusal changes** ([briefing](2026-11-multi-module-refused.md)). The composition reads these models, and its second defect (mununu#1249, the top module's own logic) is open. The refusal named both issues; it now names only mununu#1249 (the text is below). It is printed where it was, with the same exit code and the same 400.
- **No verdict and no JSON shape changes.** The verb gives no verdict, and `verify` does not reach this lift while the option is refused.

## What it looks like

A top with three instances of one type, two of them with the same parameter value:

```systemverilog
module child #(parameter bit STUCK = 1) (input logic clk, input logic rst, input logic go, output logic busy);
  always_ff @(posedge clk or posedge rst) begin
    if (rst) busy <= 1'b0;
    else busy <= STUCK ? 1'b0 : go;
  end
endmodule
module top(input logic clk, input logic rst, input logic go);
  logic busy_a, busy_b, busy_c;
  child #(.STUCK(0)) u_a(.clk(clk), .rst(rst), .go(go), .busy(busy_a));
  child #(.STUCK(1)) u_b(.clk(clk), .rst(rst), .go(go), .busy(busy_b));
  child #(.STUCK(0)) u_c(.clk(clk), .rst(rst), .go(go), .busy(busy_c));
endmodule
```

```console
$ mununu sv emit-btor2-per-module top.sv --top top --output-dir /tmp/out-three
Emitted 2 BTOR2 file(s) to /tmp/out-three
  child$top.u_a -> /tmp/out-three/child$top.u_a.btor2 (state_count=2, property_count=0)
    instances: top.u_a, top.u_c
  child$top.u_b -> /tmp/out-three/child$top.u_b.btor2 (state_count=2, property_count=0)
    instances: top.u_b
```

Before, the verb wrote each type once, at its defaults (the rows below). For this design that is one file, `child.btor2`, with the register stuck at 0: the model of `u_b` alone.

The refusal of `multi_module = true`, as `mununu verify` prints it now:

```text
verify config has 1 validation issue(s):
  - [[sources]] `system`: options.multi_module = true is refused — the per-module composition leaves the top module's own logic out of the model (mununu#1249), so a verdict on that model is not a verdict on the design; that is a mununu defect, not something to change in the design, and the option comes back when it is fixed. Remove the key or set `multi_module = false`: the design is then lifted flattened, as one module, and a property names the signals of that model instead of `<instance>__<signal>`
```

## What was lifted before, and what is lifted now

Each row is a small design with one module type `child` whose register follows its input or stays at 0, chosen by what the row names.

| the design | before | now |
|---|---|---|
| the instance takes the defaults | `child.btor2`, the default | the same |
| a parameter override on the instance | `child.btor2`, the default | `child.btor2`, the instance |
| two instances, the same override | `child.btor2`, the default | `child.btor2`, the instances |
| two instances, different overrides | `child.btor2`, the default, for both | `child$top.u_a.btor2` and `child$top.u_b.btor2`, one per instance |
| a `defparam`, a default from a package, a type parameter | `child.btor2`, the default | `child.btor2`, the instance |
| a parameter that chooses the clock in a generate block | `child.btor2`, the default clock structure | `child.btor2`, the chosen one |
| an interface port | an error from the reader | `child.btor2`, the instance |
| an instance in a generate loop, each iteration its own value | `child.btor2`, the default | `child$top.gen[0].u_g.btor2` and `child$top.gen[1].u_g.btor2` |
| an instance below an instance, the outer one overridden | `leaf.btor2` and `mid.btor2`, both at their defaults | `leaf.btor2` and `mid.btor2`, both as instantiated |
| a module type whose own name has a `$` (`child$v2`) | an error from the reader (`'child' is not a valid top-level module`) | `child$v2.btor2`, the instance |

Measured on 2026-11-25 in the `mununu-sva` image (yosys 0.68+120), on debug builds. The "now" column is the table of `x1248_each_instance_is_lifted_as_the_design_elaborates_it` on this change, where it passes. The "before" column is the same designs lifted by the parent commit, row by row; there the test fails at the row "a parameter override on the instance". The two reader errors are as `main` (`3b2dc82b`) prints them.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A lane that matches the text of the `multi_module = true` refusal: the text changed; match the option's name, not the sentence. Otherwise only a lane that runs `sv emit-btor2-per-module`: the files of a type instantiated with non-default values change, a type whose instances differ is written as several files under new names, and the output has the `instances:` lines. | A script that reads `<type>.btor2` by name: read the names the verb prints instead. Run the lane again in an empty directory. If no lane runs the verb: none. |
| **monono** | Same, under the same condition. The whole-design lift (`sv verify-auto` and the other `sv` verbs) is untouched. | Same. |
| **mununu-ui** | Nothing: the verb has no route and no UI affordance. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, if you run `sv emit-btor2-per-module`. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image and run the verb: bump the pin. | They consume the binary. |

## Provenance

- **Change:** pending merge — see branch `fix/per-instance-lift-1248`. Issue: [mununu#1248](https://github.com/Mumunu-team/mununu/issues/1248). Class: [`instance-elaboration`](../defect-classes.md#instance-elaboration). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
- **Found by:** the issue, measured in the `mununu-sva` image on a small design (2026-11-23), while a fix for the clock scan of this lift was being designed.
- **engine:** none in the defect or the fix, which are in the lift: yosys-slang `read_slang --keep-hierarchy`, then per instance `hierarchy -top <type>$<instance path>`, `flatten`, `write_btor`, and the BTOR2 adapter. The differential test's verdicts come from the verify framework's explicit evaluation of two models of one design: the composed per-instance models and the flattened lift.
- **Tests:** `x1248_each_instance_is_lifted_as_the_design_elaborates_it` (the table above, each instance's BTOR2 compared with the lift of a design whose defaults are the instantiated values) and `x1248_the_composition_agrees_with_the_flattened_lift_on_every_row`, both in the image; on every host the seams `x1248_a_keep_hierarchy_snapshot_is_read_one_module_per_instance`, `x1248_instances_are_grouped_by_type_and_model`, `x1248_the_per_instance_script_starts_from_the_read_of_the_discovery_pass` and `x1248_an_instance_module_name_is_checked_before_it_reaches_the_script`.
- **API schemas:** no shape change.

## Not covered here

- **The composition's own defect (mununu#1249).** The per-instance models are what it composes, but it still leaves the top module's own logic out, so `multi_module = true` stays refused.
- **The clock scan of this lift (mununu#1191).** A multi-clock or derived-clock module is still lifted onto one clock with nothing said.
- **A module of the hierarchy whose name the lift cannot put in its script** (an escaped identifier) is refused with the name, as before.
