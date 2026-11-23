# Consumer briefing: every `sv` verb reports what its lift cut, re-sized or re-ran (mununu#1189, 2026-11)

> Source of truth: `lift_decision_notes` and `cdc_lint_lift_lines` in [`adapter/lift_decisions.rs`](../../crates/mununu-core/src/adapter/lift_decisions.rs) and `Lifted` in [`adapter/sv_verify.rs`](../../crates/mununu-core/src/adapter/sv_verify.rs) — surface: CLI+API — `sv verify-liveness`, `sv verify-liveness-all`, `sv verify-recoverability`, `sv check-fsm`, `sv lint`, `sv mutate`, `sv verify-auto --plan-only`, `sv cdc-lint` and their `POST /api/v1/sv/…` routes, `POST /api/v1/sv/plan`, `POST /api/v1/sv/cegar`; `sv verify-auto` for two reworded notes and for `control-slice` under a clock note

**Audience:** anyone who runs an `sv` verb other than `sv verify-auto` on a design with a module that has no body, with `--cutpoint` or with `--param` (API `lift.cutpoints`, `lift.params`), or who reads those verbs' reports; and anyone who matches on the text of `sv verify-auto`'s `parameter-override` or `lift-retry` note, or reads its `control-slice` note on a lift with merged clock domains. It follows [the briefing of mununu#1181](2026-11-merged-clock-domains-on-every-sv-verb.md), which added `lift_notes` for the clock domains and left this case open.

## TL;DR

- **A result on a cut or re-sized model now says so on every verb.** A lift decides more than the model: it cuts every module the sources hold no body for (its outputs become free values), it cuts the nets the request names, it elaborates with the parameters the request sets, and on one backend error it runs its script a second time. `sv verify-auto` reported each in `notes`. The other verbs lifted the same model and returned their result with nothing about it. **If you ran `sv verify-liveness`, `sv verify-recoverability`, `sv check-fsm`, `sv lint` or `sv mutate` on a design with a bodiless module or with `--cutpoint`, the result was on a model with free nets and the report did not tell you.** No verdict changes; the report now carries the notes.
- **`lift_notes` gains five note kinds**, after the clock notes it already had. The field, its place in each report and its shape (`kind`, `level`, `summary`, `detail`, `items`) are those of mununu#1181.

  | kind | level | when | `items` |
  |---|---|---|---|
  | `lift-retry` | `info` | the lift ran its yosys script twice | none |
  | `parameter-override` | `scope-caveat` | `--param` / `lift.params` | `NAME=VALUE` each |
  | `control-slice` | `scope-caveat` | `--cutpoint` / `lift.cutpoints` | the nets asked for |
  | `flop-stub` | `info` | a known flop primitive with no definition got a behavioural model (the plan and `sv mutate` only: they share `sv verify-auto`'s lift) | the primitives |
  | `blackbox-cut` | `soundness-caveat` | a module with no body in the sources | the modules |

  ```json
  "lift_notes": [
    { "kind": "blackbox-cut", "level": "soundness-caveat",
      "summary": "1 module(s) cut (no body) — their outputs are free values and the state inside them is not modelled.",
      "detail": "…", "items": ["vendor_cell"] }
  ]
  ```

- **`lift_notes` is no longer absent on every one-clock design.** It is absent when the lift made none of these decisions and has no clock note. A design on one input clock with a bodiless module, a cut point or a parameter override now has the field where it had none. A reader that took "`lift_notes` present" to mean "clock domains were merged" must look at `kind`.
- **The two cut notes are worded per verb, not copied from `sv verify-auto`.** With no clock note on the lift, a free net is an over-approximation of the design, and what that allows depends on what the verb computes. The `detail` says which of the verb's results still holds on the design:

  | verb | transfers to the design (no clock note on the lift) |
  |---|---|
  | `sv verify-liveness`, `sv verify-liveness-all` | a HOLDS; a VIOLATED may follow a run only a free value allows |
  | `sv verify-recoverability` | neither verdict on its own (`AG EF` needs both directions) |
  | `sv check-fsm` | a register reported as staying within its encodings |
  | `sv check-fsm --liveness` | an encoding listed as never reached; every other entry may rest on a free value |
  | `sv lint` | a finding describes the model with the net free; a register not in the model is not checked |
  | `POST /api/v1/sv/cegar` | a HOLDS of an all-universal formula, a VIOLATED of an all-existential one |
  | `sv mutate`, `sv mutate --list`, the plan | they point at `sv verify-auto`'s note of the same kind, or say the list is that of the lifted model |

  **With a clock note on the same lift, nothing in the table transfers.** Merged clock domains (`--merge-clock-domains`) and a clock with no driver the model can follow take runs away from the model, and the free nets add some: the model is neither an over- nor an under-approximation of the design. The cut notes then do not make the over-approximation claim. They name the clock notes (`clock-domains-merged`, or the clock's net and its note kind) and say that neither verdict of `sv verify-liveness`, no answer of `sv check-fsm` and no verdict of `sv/cegar` transfers on its own. The rows that claim no transfer read the same.

  Do not match on the `detail` text; `kind`, `level` and `items` are the contract.
- **`sv cdc-lint` gets one line per decision in `notes`** (its notes are plain strings): `black-boxed modules (…): their outputs are unclocked, so crossings inside or through them are not seen`, `cut points requested (…): …`, `parameters overridden before the lift (…): …`, and `lift: retried — …`. The crossings it lists are unchanged.
- **`sv verify-auto`: two notes reworded, and `control-slice` corrected under a clock note.** The note kinds, levels and items are unchanged, and so are the verdicts. The texts now shared with the other verbs no longer say "verdicts", since a lint and a plan have none:

  | note | before | now |
  |---|---|---|
  | `parameter-override`, `summary` | `… — verdicts hold for THESE parameter values.` | `… — every result here is for THESE parameter values.` |
  | `parameter-override`, `detail` | `… so the design verified here is the one sized by these parameters. …` | `… so the design lifted here is the one sized by these parameters. …` |
  | `lift-retry`, `detail` | `… so the model has the same registers and the verdicts are over the same design. …` | `… so the model has the same registers and is of the same design. …` |

  Its `control-slice` note said that a definite HOLDS of a safety property transfers to the RTL, and since mununu#1180 excepted the storage cells on a clock with no driver. On a lift with a clock note that contradicted the `clock-domains-merged` note beside it, which says a HOLDS does not transfer, and a lift with a driverless clock always has that note. The note now makes the claim only when the lift has no clock note. Under one, its summary ends `… not an over-approximation of the design.`, and its detail names the clock notes and says no verdict under the cut points transfers on its own. **If you took a HOLDS from `sv verify-auto` with `--cutpoint` and `--merge-clock-domains` together as a result on the design, re-read it with both notes.** With no clock note the text is the one it was.

- **The `control-slice` note on the other verbs says "requested".** The lift does not return which cut-point names it applied, and a name outside the net grammar is skipped without a message (mununu#1217, open). The note lists what was asked for and says so.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `lift_notes` appears on the reports above for a lift with a bodiless module, a cut point, a parameter override or a retry — also on a one-clock design. `sv cdc-lint`'s `notes` may hold new lines. Two `sv verify-auto` note texts are reworded. | Read `lift_notes` by `kind` and `level`, and treat a `soundness-caveat` there as you treat one in `sv verify-auto`'s `notes`. Re-read any stored result of these verbs on a design with a bodiless module or a cut point with the table above. If you match the two reworded texts, match on `kind`. |
| **monono** | Same. | Same. |
| **mununu-ui** | No UI affordance for the RTL verbs. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the notes. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use, or do not verify. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the fix of mununu#1181, reading the code: the lift returns the cut modules and the retry beside the clock domains, and the verbs that had just learned to report the domains still dropped the rest (mununu#1189). No wrong result was observed on a real design; the defect is that a result on a model with free nets was reported as if it were on the design.
- **Tests:** `x1189_every_reader_reports_every_lift_decision` (decision × verb, every pair), `x1189_a_free_net_is_worded_for_what_the_reader_computes`, `x1189_a_clock_note_withdraws_the_cut_notes_claim`, `x1189_a_clock_note_changes_the_rule_of_the_readers_that_transfer`, `x1189_a_cut_note_names_each_clock_note_once`, `x1180_the_control_slice_claim_is_made_only_without_a_clock_note` (`sv verify-auto`'s note), `x1189_the_decisions_of_a_report_are_its_diagnostics_and_the_request`, `x1189_cdc_lint_has_a_line_for_every_lift_decision`, `x1189_a_crossing_through_a_cut_net_is_not_seen`, `x1189_verify_auto_reports_the_lift_decisions_under_the_shared_kinds`; in the `mununu-sva` image, `e2e_x1189_every_sv_route_reports_the_cuts_and_the_parameters_of_its_lift` (the reproducer: one design with a bodiless module, a cut point and a parameter through every route; fails on the parent, where no route but `sv/verify-auto` carried the notes; it also checks that each route's cut notes state the rule of that route's verb) and `e2e_x1189_under_merged_clock_domains_no_route_claims_a_cut_transfers` (two merged clock domains and a cut point through every route). A retried lift needs a design the yosys backend refuses the first time; it is covered by the table tests, not by a lift.
- **Schemas:** no shape change. The `description` of `lift_notes` in nine files under [`docs/api-schemas/`](../api-schemas/) is rewritten.
- **Class:** `lift-decision-report` in [`docs/defect-classes.md`](../defect-classes.md), added by this fix.

## Not covered here

- A source replaced by `--lift-override` (API `lift.lift_overrides`) is named in no report, `sv verify-auto`'s included (mununu#1214, open).
- The value the lift reads an `x` constant as is in no report (mununu#1216, open).
- The operations that lift and return the model alone — the CLI's model loader and `POST /translate` with `sv-yosys`, `context import`, `verify project`, `sv discover`, the per-module lift — report none of these decisions (mununu#1215, open); nor does `sv cegar` on the command line, which takes none of the shared lift arguments (mununu#1190, open).
- Which cut-point names the lift applied (mununu#1217, open).
- `sv verify-auto`'s own `blackbox-cut` note, and its `control-slice` note on a lift with no clock note: unchanged.
- Whether `control-slice` should be a `soundness-caveat` when the property read on the cut model is an alternating one (`sv verify-recoverability`, a nested formula of `sv/cegar`, an `AG EF` row of `sv verify-auto`): it is a `scope-caveat` on every verb today (mununu#1219, open).
- A plan with no property to plan stops before the lift, so it has no `lift_notes`.
- What each verb concludes on a free net is stated in [Verifying RTL](../verifying-rtl.md), "What the lift decided besides the model".
