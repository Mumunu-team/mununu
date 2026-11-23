# Consumer briefing: the clock scan counts every register of the model, also one that no output port reads (mununu#1229, 2026-11)

> Source of truth: [`clock_domains_with_hierarchy`](../../crates/mununu-core/src/adapter/yosys/mod.rs) (the scan) and [`plan_lift`](../../crates/mununu-core/src/adapter/yosys/mod.rs) (the script it reads the copy from) — surface: CLI+API — every `sv` verb and route that lifts SystemVerilog; `mununu sv cdc-lint` / `POST /api/v1/sv/cdc-lint`

**Audience:** anyone who lifts SystemVerilog with more than one clock in the design, and above all anyone holding a `holds` on such a design in which one clock's registers are read only by assertions; anyone who runs `sv cdc-lint`; anyone whose lift reports a `lift-retry` note; anyone with `MUNUNU_LIFT_CACHE_DIR` set.

## TL;DR

- **A soundness bugfix on the multi-clock refusal.** The BTOR2 model has one clock, so the lift counts the design's clock domains and refuses more than one unless `--merge-clock-domains` is given (mununu#758). It counted them on a copy of the design that kept only what reaches an output port. A register that no output port reads is still in the model when it has a name, and a register read only by an assertion is the common case. Its clock was not counted.
- **What that gave:** a design with two clocks, one of them clocking only such registers (or a memory read only through a net that an assertion reads), lifted onto one clock with no refusal and no `clock-domains-merged` caveat, and `sv verify-auto` could answer `holds` on it. The model steps both domains together, which is an under-approximation of the real interleavings, so that `holds` does not transfer. `sv cdc-lint` read the same copy and answered `holds` with one domain listed.
- **Now:** the copy keeps every net that has a name in the design, which is what the BTOR2 backend keeps. Such a design is **refused by default**, naming both domains, on every `sv` verb that lifts. With `--merge-clock-domains` (API `merge_clock_domains`) it lifts with the `clock-domains-merged` caveat, as any two-clock design does. `sv cdc-lint` lists the second domain and classifies the crossings between flops.
- **The other clock checks read the same copy.** A clock that is derived from logic, cut, undriven, tied to a constant or driven by several cells is refused or noted from the cells the scan sees. One that clocks only registers that no output port reads was outside the copy, and is inside it now. This follows from the change; it was not run case by case.
- **A retried lift keeps its registers.** When yosys's BTOR backend reports a "topological loop", the lift reruns its script once with `opt_clean` before `write_btor` (the `lift-retry` note). That retry dropped every register that no output port reads from the model itself, while the note said the model was the same design. By the cleaning rule a property that resolved had its whole cone in the model, and one that names a dropped register could not resolve; that is read from the rule, not measured on a run, and no verdict is known to be wrong. The retry now keeps the named nets first. On OpenTitan `csrng` (by hand with the yosys of the `mununu-sva` image, 0.68+120, 2026-11-23) the retried model had 206 of the design's 222 registers before, and has all 222 now; 234 named nodes of the BTOR2 come back and none is lost.
- **No JSON field, flag or route is added, renamed or removed.** The `lift-retry` note's `detail` text changes.
- **The lift cache refills once.** A cache entry's key holds the lift script, and the script changed, so every entry stored by an earlier build is a miss the first time.

## What to check

A `holds` you hold on a design is affected when all three are true: the design has more than one clock (or a clock that is not a plain top-level input); one of those clocks drives only registers or memories that no output port reads, for example monitor or shadow registers that only assertions read; and the report had no `clock-domains-merged` note. Lift the design again with this build. A refusal that names two domains means the earlier verdict was computed with both domains stepping together and nothing said so.

The defect was found by reading the lift script and confirmed on small hand-written designs. It was not met on a consumer's design, and no recorded verdict is known to be wrong.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A design of the shape above goes from a lift (and possibly a `holds`) to a lift error that names its clock domains. `sv cdc-lint` may list more domains and report crossings it did not see. A lift that reports `lift-retry` has more registers in its model, and a property over one of them that was unsupported may now resolve. | Lift again. For a refused design, run `sv cdc-lint` on it, and pass `--merge-clock-domains` only when the lockstep model is what you mean; read the caveat in the report. |
| **monono** | Same. | Same. |
| **mununu-ui** | Nothing: these are RTL verbs, not CTXDSL capabilities. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. No tool was added to or removed from any image. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

No cache directory needs clearing: the old entries are never hit again and can be removed at leisure.

## Provenance

- **Found by:** the work on mununu#1189, whose image test cut a design's only output port and never saw the merged-domains note it was written to check. The cause was read off the lift script and reproduced with yosys by hand (mununu#1229).
- **Class:** `cleaned-netlist-coverage` in [`docs/defect-classes.md`](../defect-classes.md), which holds the case table: what reads the second clock's storage, and what the model and the scan do with each.
- **Tests:** `e2e_x1229_a_clock_domain_only_assertions_read_is_refused_at_lift` (the reproducer: a register on a second clock that only an assertion reads lifts on the parent and is refused here), `e2e_x1229_the_scan_counts_a_domain_exactly_when_the_model_holds_its_storage` (the ten cases), `e2e_x1229_the_backend_loop_retry_writes_every_register_of_the_first_script`, `e2e_x1229_cdc_lint_sees_a_domain_whose_storage_no_output_port_reads`; without yosys, `x1229_every_opt_clean_of_the_lift_runs_right_after_the_keep_pass` and `x1229_the_scan_counts_a_domain_whose_storage_no_output_port_reads`. `e2e_opentitan_ip_csrng_full` pins the register count of the retried `csrng` lift.
- **API schemas:** no shape change.

## Not covered here

- **A register that nothing reads and that has a name is counted too.** The backend writes it into the model and a property can name it, so its clock is a clock of the model. A design with such a register on a second clock is refused like any other.
- **A register the model does not hold is not counted:** one whose net was cut with `--cutpoint`, a memory nothing reads, a register whose only name is an escaped identifier starting with `$`. Cutting the crossing register's own net is still the way to lift one domain of a two-clock design.
- **`sv cdc-lint` does not classify a crossing into a memory's write port** (mununu#1230). It now lists the memory's domain; before this change it listed one domain and said `holds`. The crossing itself is still not reported.
- **The per-module lift has no clock scan at all** (`sv emit-btor2-per-module`, `verify project` with `multi_module = true`): mununu#1191.
- **A design whose "topological loop" survives the retry** fails with the lift error it had before the retry existed. There is no second retry with a bare `opt_clean`: it would write a model with registers missing. No such design is known; `csrng`'s loop is broken with the named nets kept.
- **The cost of the larger copy.** On `csrng` the clock snapshot grows from 6,839,257 to 7,719,108 bytes (by hand, the same yosys and date). The time of a lift was not measured on a corpus.
