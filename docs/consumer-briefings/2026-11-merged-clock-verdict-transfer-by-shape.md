# Consumer briefing: what a verdict under `--merge-clock-domains` is worth depends on the clocks merged (2026-11)

> Source of truth: [`merged_clock_transfer`](../../crates/mununu-core/src/adapter/yosys/mod.rs#L2070) — surface: CLI+API — every `sv` verb and route that takes `--merge-clock-domains` / `lift.merge_clock_domains`: the `clock-domains-merged` note, `diagnostics.merged_clock_transfer` on `sv verify-auto`, the refusal of a design on both edges of one clock, the flag's help text

**Audience:** anyone who passes `--merge-clock-domains` (API `lift.merge_clock_domains`) and acts on a VIOLATED, or who reads or matches on the `clock-domains-merged` note. It follows [every `sv` verb reports the clock domains it merged](2026-11-merged-clock-domains-on-every-sv-verb.md) and replaces what that briefing quotes as the note's `summary`.

## TL;DR

- **No verdict changes. What changes is what the report says a verdict is worth (mununu#1202).** Under the flag the model steps every storage cell on every step. The `clock-domains-merged` caveat said, for every design, that this is an under-approximation: a HOLDS does not transfer, and a VIOLATED is a run of the design in which the domains happen to tick together. The second half is true only for clocks the design receives on different inputs. For both edges of one clock, a divided clock, a clock driven by logic, a clock tied to a constant, or a clock with no driver in the lifted design, the design has no run in which the merged clocks tick together (or it is not known to have one), and **neither verdict transfers**. The caveat now says which case the lift is in.
- **If you treated a VIOLATED under the flag as a run of the design, check the class first.** On a design that uses both edges of one clock, an assertion that holds in the design can be VIOLATED on the merged model; the trace is a run of the model only.
- **New field on `sv verify-auto`: `diagnostics.merged_clock_transfer`.** Present exactly when `diagnostics.merged_clock_domains` is not empty:

  ```json
  "merged_clock_transfer": { "transfers": "neither", "shapes": ["both-edges"] }
  ```

  | field | values |
  |---|---|
  | `transfers` | `violated-if-clocks-coincide` (every shape is `input-clocks`: a VIOLATED is a run of the design in which the merged clocks tick at the same instants) or `neither` |
  | `shapes` | each merged shape once, in this order: `input-clocks`, `both-edges`, `divided`, `constant`, `logic-driven`, `no-known-driver` |

  A HOLDS transfers under neither value.
- **The note's `summary` and `detail` changed on every verb.** `kind`, `level` and the shape of `items` are unchanged.

  | what was merged | `summary` before | `summary` now |
  |---|---|---|
  | clocks on different inputs | `lift: 2 clock domains merged onto one clock — every verdict assumes they tick together.` | `lift: 2 clock domains merged onto one clock — a VIOLATED is a run in which they tick together; a HOLDS does not transfer.` |
  | several domains, some of another shape | the same text | `lift: 2 clock domains merged onto one clock, among them both edges of one clock — neither verdict transfers to the design on its own.` (the shapes are named: `both edges of one clock`, `a divided clock`, `a clock tied to a constant`, `a clock driven by logic`, `a clock with no driver in the lifted design`) |
  | one domain, on a derived clock | `lift: a derived clock merged onto the model's clock — every verdict assumes it ticks on every step.` | `lift: a clock driven by logic merged onto the model's clock — neither verdict transfers to the design on its own.` (or `a divided clock`) |
  | one domain, on a clock with no driver | `lift: a clock with no driver in the lifted design merged onto the model's clock — every verdict assumes it ticks on every step.` | `lift: a clock with no driver in the lifted design merged onto the model's clock — neither verdict transfers to the design on its own.` (or `a clock tied to a constant`) |

  The `detail` keeps its first two sentences on the HOLDS, then says either that a VIOLATED is a run of the design in which the merged clocks tick at the same instants, or that it does not transfer, with one sentence per shape that takes it away and, where there is one, what to change in the RTL (a divider or a gate written as a clock enable).
- **An inverted copy of a clock is the other edge of that clock.** A flop on `posedge clk_n` with `clk_n = ~clk` beside a flop on `posedge clk` was counted as two independent input clocks. It is now both edges of `clk`, and its entry in the domain list (the refusal, `diagnostics.merged_clock_domains`, the note's `items`, the domains of `sv cdc-lint`) names the input and the edge: ``clk_n (posedge, 1 cell, the falling edge of input `clk`)``. A clock used directly on an input keeps its entry as it was. The number of domains does not change.
- **A design on both edges of one clock is refused in other words.** It was refused as "the design has 2 clock domains (…); … lifting it would step every domain together and hide every crossing defect. Run `mununu sv cdc-lint` …", and the flag was offered as accepting "that under-approximation". The refusal now says that both edges of one clock are among the domains, that no verdict would transfer, that mununu has no model for the shape (a mununu limitation; nothing to change in the design), and that the flag accepts "the lockstep model" and reports that neither verdict transfers. The refusals of the other shapes are unchanged.
- **Three notes of `sv verify-auto` no longer claim a transfer past the merge.** `coverage-summary` and `abstraction-posture` say that a definite verdict transfers to the RTL; on a lift that merged clocks the `detail` of each now ends by pointing at the `clock-domains-merged` note, and so does the `summary` of `abstraction-posture`, also in the form a portfolio run gives it. With no merge they are unchanged. `exact-skip-rescue` changed with or without a merge: its `detail` says the verdict is exact for the lifted model and leaves what transfers to `abstraction-posture`.
- **The help text of `--merge-clock-domains`** and the description of `lift.merge_clock_domains` in the API schema say the same as the note.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | The note's `summary` / `detail`, the new field on `sv verify-auto`, the domain entry of an inverted clock, the both-edges refusal. | If a lane runs under the flag and counts a VIOLATED as a finding on the design, gate that on `transfers == "violated-if-clocks-coincide"` (on `sv verify-auto`), or on the note's summary elsewhere. Update any match on the old summary text. |
| **monono** | Same. | Same. |
| **mununu-ui** | No UI affordance for the RTL verbs. | None. |

## Docker rebuild

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | They ship the `mununu` binary. | Yes, to get the new field and texts. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | They build from source per use, or do not verify. | No. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | They consume the binary. | When a lane uses the flag: bump the pin. Otherwise no. |

## Check it on your side

Lift a design of yours that has two clocks on two inputs with `mununu --quiet sv verify-auto <file> --top <top> --merge-clock-domains --json` (API: `lift.merge_clock_domains: true`), and read `diagnostics.merged_clock_transfer`: it is `{"transfers": "violated-if-clocks-coincide", "shapes": ["input-clocks"]}`. On a design with a register on `negedge` of the same clock, or a divided or gated clock, `transfers` is `neither` and `shapes` names what was merged. On a design with one clock used directly, the field and the `clock-domains-merged` note are both absent.

## Provenance

- **Fix:** pending merge — see branch `fix/merged-clock-caveat-transfer-1202`. Issue: [mununu#1202](https://github.com/Mumunu-team/mununu/issues/1202). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
- **Found by:** mununu#1202, from the cross-host review of the clock refusals: the caveat's claim about a VIOLATED was written for two input clocks and applied to every shape the flag lets through. Measuring the both-edges case on the real lift (measured 2026-11-24 in the `mununu-sva` image on a debug build of the mununu#1218 branch, head `be73077d`, with Verilator 5.051 as the oracle, in the issue's comment of that date: an assertion that holds under Verilator is VIOLATED on the merged model) also showed that an inverted copy of a clock was read as an independent clock.
- **engine:** exact-symbolic (ROBDD, full state, bit-blast μ-fixpoint, owned) decides on the merged model, as before; nothing in the engines changes. The classification is a pure function of the clock domains the lift's netlist scan returns.
- **Tests:** `x1202_what_a_merge_transfers_follows_from_the_shapes_merged` (every kind of clock root, alone and beside input clocks; the input cases by input and edge), `x1202_the_caveat_claims_a_violated_only_for_the_class_that_has_one` (the note, over every shape), `x1202_a_refusal_calls_the_merge_an_under_approximation_only_when_it_is_one`, `x1202_the_scan_records_the_input_and_the_edge_behind_an_inverted_clock`, `x1202_the_transfer_notes_point_at_the_caveat_when_clocks_were_merged` (the notes that make a transfer claim, through the portfolio's rewrite too), and on the real lift `e2e_x1202_verify_auto_reports_what_a_merge_transfers_by_the_shapes_merged`.
- **Class:** `clock-net-driver` in [`docs/defect-classes.md`](../defect-classes.md), the rows on what a merge transfers.

## Not covered here

- **The class is a field on `sv verify-auto` only.** The other verbs carry it in the note's text (mununu#1262).
- **No exact model for a design on both edges of one clock** (mununu#1256): the flag gives a model whose verdicts do not transfer, and there is no other way to lift such a design today.
- **Copies of one input on one edge** (a buffered clock that survives the netlist cleaning) are reported as `input-clocks`. They always tick together, so the report's caution about a HOLDS is more than is needed there.
- **The per-module lift** (`sv emit-btor2-per-module`, `verify project` with `multi_module = true`) does not scan clocks at all (mununu#1191).
