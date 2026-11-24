# Consumer briefing: every `sv` verb reports the clock domains it merged (2026-11)

> Source of truth: `lift_clock_notes` in [`adapter/slang/verify_auto_text.rs`](../../crates/mununu-core/src/adapter/slang/verify_auto_text.rs) and `Lifted` in [`adapter/sv_verify.rs`](../../crates/mununu-core/src/adapter/sv_verify.rs) — surface: CLI+API — `sv verify-liveness`, `sv verify-liveness-all`, `sv verify-recoverability`, `sv check-fsm`, `sv lint`, `sv mutate`, `sv verify-auto --plan-only` and their `POST /api/v1/sv/…` routes, `POST /api/v1/sv/plan`, `POST /api/v1/sv/cegar`

**Audience:** anyone who passes `--merge-clock-domains` (API `lift.merge_clock_domains`) to an `sv` verb other than `sv verify-auto`, or who reads those verbs' reports. It follows the briefings of [mununu#1148](2026-11-clock-rooted-at-black-box-note.md) and [mununu#1180](2026-11-clock-roots-constant-port-several-drivers.md) and closes the case both left open.

> **Superseded in part 2026-11-24** by [`2026-11-merged-clock-verdict-transfer-by-shape.md`](2026-11-merged-clock-verdict-transfer-by-shape.md) (mununu#1202): the note's `summary` quoted below has other words now, and "an under-approximation" holds only when every merged clock is on an input of its own. Which verbs carry the note is unchanged.

## TL;DR

- **A verdict under `--merge-clock-domains` now says so on every verb (mununu#1181).** The flag lifts a multi-clock design, or one on a derived clock, onto the model's one clock. That is an under-approximation of the real interleavings, so a HOLDS does not transfer to the design. `sv verify-auto` said so in a `clock-domains-merged` soundness caveat. The other verbs took the flag, lifted the same model and returned their verdict with nothing about it. **If you ran `sv verify-liveness`, `sv verify-recoverability`, `sv check-fsm`, `sv lint` or `sv mutate` with the flag and stored a HOLDS, that result was on the lockstep model and the report did not tell you.** The verdicts themselves are unchanged; what changes is that the report carries the caveat.
- **One new field, `lift_notes`, on those reports.** An array of the typed notes `sv verify-auto` has in `notes`:

  ```json
  "lift_notes": [
    { "kind": "clock-domains-merged", "level": "soundness-caveat",
      "summary": "lift: 2 clock domains merged onto one clock — every verdict assumes they tick together.",
      "detail": "…", "items": ["clk_a (posedge, 1 cell)", "clk_b (posedge, 1 cell)"] }
  ]
  ```

  It is **absent** when the lift has nothing to say, so the report of a design on one input clock is byte-for-byte what it was. The notes are the ones `sv verify-auto` builds, word for word: `clock-domains-merged`, and one of `clock-rooted-at-black-box`, `clock-undriven`, `clock-constant`, `clock-multi-driven` per clock with no driver in the lifted design.

  | where | the field |
  |---|---|
  | `sv verify-liveness` / `POST /api/v1/sv/verify-liveness` (plain, `--fairness`, `--response`) | `lift_notes` |
  | `sv verify-liveness-all` | `lift_notes` |
  | `sv verify-recoverability` / `POST /api/v1/sv/verify-recoverability` (plain and `--refine`) | `lift_notes` |
  | `sv check-fsm` / `POST /api/v1/sv/check-fsm` (with and without `--liveness`) | `lift_notes` |
  | `sv lint` / `POST /api/v1/sv/lint` | `lift_notes` |
  | `sv mutate` / `POST /api/v1/sv/mutate` (a run and `--list`) | `lift_notes` (a run: the notes of the unmutated design's lift) |
  | `POST /api/v1/sv/plan`, `sv verify-auto --plan-only` | `lift_notes` in the JSON; one `note [<level>] <kind>: <summary>` line per note in the text |
  | `POST /api/v1/sv/cegar` | `lift_notes` |
  | `sv verify-auto` / `POST /api/v1/sv/verify-auto` | unchanged: `notes` and `diagnostics.merged_clock_domains` |
  | `sv cdc-lint` / `POST /api/v1/sv/cdc-lint` | unchanged: `domains` and `notes` (the lint merges with or without the flag) |

  The `btor2` routes that share a response type with an `sv` route (`btor2/verify-liveness`, `btor2/verify-recoverability`, `btor2/check-fsm`, `btor2/cegar`) never fill the field: a BTOR2 model has no lift.
- **A clock with no driver in the lifted design now lifts under the flag on every verb.** Since mununu#1148 such a clock (rooted at a black box, a cut net, a constant, a port of unknown direction, several drivers, or undriven) lifted under the flag only on `sv verify-auto` and `sv cdc-lint`; the other verbs refused it with "`--merge-clock-domains` lifts such a clock only on `sv verify-auto` and `sv cdc-lint` … this verb drops the clock domains". That refusal is gone: the verb lifts, and `lift_notes` names the root. Without the flag the clock is refused as before.
- **The default refusals end differently.** The start of each text, up to and including `Clock domains: …` and the design-side remedy, is unchanged.

  | refusal | the end, before | the end, now |
  |---|---|---|
  | more than one clock domain; a derived clock | `… accepts … explicitly — sv verify-auto then reports it (clock-domains-merged), the other verbs report nothing (a mununu limitation).` | `--merge-clock-domains on sv verify-auto / sv verify-liveness / sv verify-liveness-all / sv verify-recoverability / sv check-fsm / sv lint / sv mutate (API lift.merge_clock_domains) accepts … explicitly, and the report then says so in a clock-domains-merged note.` |
  | a clock with no driver | `--merge-clock-domains on sv verify-auto accepts the lockstep model explicitly and names the root in a note — on the other sv verbs the flag does not lift such a clock, since they drop the roots (a mununu limitation).` | the same list of verbs, then `… accepts the lockstep model explicitly, and the report then names the root in a note.` |

  An operation that lifts SystemVerilog and does not take the flag — `sv discover`, `sv cegar` on the command line, `POST /translate` with `sv-yosys`, `verify project`, `context import` — refuses the same designs, and there the sentence starts `this operation has no opt-in to lift the design as it is (a mununu limitation); elsewhere, --merge-clock-domains on sv verify-auto / …`. Before, those operations ended with the sentence of the verbs that take the flag.

- **`sv mutate --list`: a lift failure reads `sv mutate --list: SV → BTOR2 (yosys-slang): <message>`** (CLI and API). Before, it read `sv mutate --list: <message>`. The verb now lifts through the entry the other verbs use, which adds the stage name.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `lift_notes` appears on the reports above when a lift merged clock domains or let a driverless clock through. A driverless clock under the flag lifts where it was refused. | If you pass the flag to one of these verbs: read `lift_notes`, and treat a `soundness-caveat` there as you treat one in `sv verify-auto`'s `notes` (a HOLDS does not transfer). Re-read any stored HOLDS from such a run with that caveat. If your reader rejects unknown fields on these reports, allow `lift_notes`. If you match the refusal texts, match up to `Clock domains:`. |
| **monono** | Same. | Same. |
| **mununu-ui** | No UI affordance for the RTL verbs. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the field and the new refusal texts. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use, or do not verify. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the cross-host review of the fix of mununu#1148, reading the code: the flag is on eight verbs and only one read the clock domains the lift returns (mununu#1181). That fix contained the worst case by refusing a driverless clock on the verbs that could not report it; this one removes the containment by making every verb report.
- **Tests:** `e2e_x1181_every_sv_route_reports_the_merged_clock_domains` (the reproducer, in the `mununu-sva` image: every route and mode on a two-clock design, and a one-clock design that must carry no `lift_notes`; fails on the parent, where no route carried it), `e2e_x1181_without_the_opt_in_a_two_clock_design_is_still_refused`, `x1181_every_merged_shape_has_the_caveat_and_every_free_root_its_note`, `x1181_verify_auto_and_the_other_verbs_word_the_merge_the_same`, `x1181_every_model_only_lift_refuses_the_merge_opt_in`, `x1181_a_clock_refusal_names_the_opt_in_where_the_caller_can_take_it`, `e2e_x1181_a_model_only_lift_says_it_has_no_merge_opt_in`, and the policy tests `every_lift_that_can_merge_clock_domains_reports_them`, `the_clock_refusal_names_every_verb_that_takes_the_merge_flag` and `every_pinned_report_of_a_lifting_sv_verb_has_optional_lift_notes`.
- **Schemas:** nine files under [`docs/api-schemas/`](../api-schemas/) gain the optional `lift_notes` property; the request schema's description of `merge_clock_domains` is reworded.
- **Class:** `clock-net-driver` in [`docs/defect-classes.md`](../defect-classes.md), the row "which verbs carry the note".

## Not covered here

- `sv cegar` on the command line takes none of the shared lift arguments, so the flag cannot reach it; the route does (mununu#1190, open). Its refusal says so, as above.
- The per-module lift (`sv emit-btor2-per-module`, `verify project` with `multi_module = true`) does not look at the clocks at all (mununu#1191, open).
- The lift's other decisions — a module cut because it has no body, a cut point, a parameter override, a retried lift — are still reported by `sv verify-auto` alone (mununu#1189, open). `lift_notes` is where they will go.
- A plan with no property to plan stops before the lift, so it has no `lift_notes`.
- What the merge means for a verdict, and which clocks are refused by default: unchanged, see [Verifying RTL](../verifying-rtl.md), "One clock per model".
