# Consumer briefing — a mux on internal logic is no longer read as its register (soundness fix, 2026-10)

> Source of truth: [`follow_state_alias`](../../crates/mununu-core/src/adapter/btor2/parser.rs) — surface: (CLI+API)

**Audience:** every client of `sv verify-auto` and the other verbs that bind a property's atoms to the lifted model (rosf, monono, mununu-ui). A verdict-semantics fix: some verdicts change, each one toward the correct answer.

## TL;DR

- **The defect.** Resolving a name to the register it aliases, mununu followed an `ite` whose one arm is a constant and whose other arm reaches a register, as the async-reset register mux (`q = rst_n ? state : resetval`). It did so whatever the `ite`'s condition was. A combinational output with that shape on ordinary logic, such as `ch_trip = link_fault ? 3'b111 : data_q`, was therefore read as `data_q`, and every atom over it evaluated the register instead of the output.
- **What it produced.** On the tutorial's `sensor_monitor`, `link_fault |-> ch_trip == 3'b111` holds by construction, yet it came back **VIOLATED** with a counterexample, while all six safety engines (exact, native BMC, SPACER, btormc, pono, rIC3) report `link_fault && ch_trip != 7` unreachable on the very model verify-auto runs. A false HOLDS was equally possible with the opposite polarity.
- **The fix.** An `ite` is followed as an alias only when its condition is a constant, as a pinned reset is. It is then followed exactly, into the branch the constant selects. A condition that is still a free primary input (an unpinned reset, where registers are named before any pin) keeps the previous async-reset reading. A mux on internal logic or on state is evaluated as itself.
- **Residual, stated:** a mux on a free primary input that is *not* the reset (`o = bypass_i ? '0 : q`, with `bypass_i` left free) still has the unpinned reset's shape and is still read as `q`.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a property over a combinational output of the `cond ? CONST : reg` shape may change verdict, toward the correct one | re-run your lanes on this build; a changed row is the old verdict being wrong |
| **monono** | the same | re-run `make formal-docker`; review any flipped row against the RTL |
| **mununu-ui** | none (verdicts come from the backend) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes | verdict correctness |
| `mununu/docker/Dockerfile.dev` | yes | the UI's dev backend |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | rosf and monono lanes run the binary from it |
| `mununu/docker/Dockerfile.profile` | yes | its scripts read verdicts |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction is unchanged |
| rosf `docker/*` | yes, at the next pin | verdict correctness |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes, at the next pin | verdict correctness |

## Verification steps for every consumer

```bash
# sensor_monitor: ch_trip = link_fault ? 3'b111 : data_q, with `a_fail_safe: link_fault |-> ch_trip == 3'b111`
mununu sv verify-auto sensor_monitor.sv --source sensor_monitor_sva.sv --top sensor_monitor --pin rst_n=1
#   a_fail_safe: HOLDS (VIOLATED before this fix)
```

## Provenance

- Found preparing the EmTech tutorial recordings (segment 3, owned vs. third-party IP). Localised by probing property shapes on the same model: `link_fault |-> ch_trip[0]` (decided through the bit expansion) held while `link_fault |-> ch_trip == 7` failed, and a plain `bad` for `link_fault ∧ ch_trip ≠ 7` on the dumped model (`MUNUNU_SHADOW_BTOR2_DUMP`) was unreachable for every engine.
- Reproducer: `adapter::btor2::symbolic_bitblast::tests::a_mux_on_internal_logic_is_read_as_the_mux_not_its_register` (the exact engine's verdict on a BTOR2 of the same shape: `Holds`, `Violated` on the parent). Resolver unit test: `adapter::btor2::parser::tests::a_mux_on_a_non_constant_condition_is_not_an_alias`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md), [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- The residual non-reset free-input mux case above.
