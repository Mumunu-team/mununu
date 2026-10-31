# Consumer briefing — `@mununu_template` instances next to the design, the `assumes=` row modifier, and refused annotation lines that now reach the report (2026-10)

> Source of truth: [`scan_annotation_properties_with`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`plan_assumptions`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`split_tag_head`](../../crates/mununu-core/src/mununu_annotations/mod.rs) + [`extract_from_sv_source_checked`](../../crates/mununu-core/src/mununu_annotations/mod.rs) — surface: (CLI+API) — every `--source` of `sv verify-auto` and `POST /api/v1/sv/verify-auto`.

**Audience:** rosf, monono and anyone who writes `@mununu_*` annotations into SystemVerilog, drives `sv verify-auto --template`, or parses the `sv verify-auto` JSON (`sv-verify-auto-response.schema.json`).

## TL;DR

- **New annotation tag `@mununu_template`.** A property-template instance can be written next to the design instead of as `--template` / `--template-arg` flags: `// @mununu_template(trip_reach) rtl_reachable TARGET="state_q == 3"`. The row is labelled by the name in the parentheses (or `tmpl.<instance>` when unnamed, the label `--template` rows have always had), its `formula` is the instantiated mu-calculus, and the instance as written rides beside it as `template:` (text) / `template` (JSON). The row has its own `source:` line (the comment). Both surfaces read the sources, so the API gets it with no new request field.
- **The `--template` flag is now rendered as that annotation** internally. Labels, formulas and verdicts of existing `--template` rows are unchanged; such a row additionally carries `template` in the JSON. A flag that cannot be instantiated is still a hard error.
- **New row modifier `assumes=ignore`**, in the tag's parentheses after the name, on `@mununu_guarantee` and `@mununu_template` rows: `// @mununu_template(trip_holds, assumes=ignore) rtl_hold_until HOLD=lof_trip RELEASE=stop_cmd`. Under `--assumes precondition` the row is judged on every trace (its formula is not relativized to the assumption monitor) while the other rows are judged under the applied assumptions — the verdict `--assumes ignore` would give that row, in the same run as the assumed ones. The row says so: `assumes: exempt — <what the modifier did>` (text), `assumes_exempt` (JSON). In every other mode the modifier changes nothing, and the same field says why. `assumes=precondition` is accepted as the default said out loud; anything else is refused.
- **Behaviour change — refused annotation lines are reported.** A `@mununu_…` comment the scanner could not read — a tag typo (`@mununu_gurantee`), a malformed head (`@mununu_guarantee(a b)`, `(oops`, `()`) — used to vanish with no message: the obligation was silently not checked. It is now a `skipped` item of the `annotation-properties` note (`<file>:<line>: \`@mununu_…\` — <reason>`), which turns that note's level to a scope caveat. A run that silently dropped a line before now says so; its verdicts do not change.
- **A new modifier key or value the tool does not know is refused**, with the key named (`unknown modifier \`foo\``), rather than ignored.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the `sv verify-auto` JSON row gains two optional fields, `template` and `assumes_exempt`; a `--template` row now carries `template`. A source you lint may contain `@mununu_template` lines — they are meaningful to verify-auto. If any of your sources had a malformed `@mununu_` line, it now appears under `notes[kind=annotation-properties].items` as `skipped: …` and that note becomes a caveat | none required; optional: read `template` instead of reconstructing the instance from the label; check your sources' notes once for new `skipped:` items |
| **monono** | same JSON additions; the same note change | none required |
| **mununu-ui** | none (RTL verb; the UI does not render these rows) | none |
| **template packs** (`--templates-file`, `verify.toml [templates] files`, `mununu.toml`) | a `@mununu_template` row is instantiated against the same layered catalog the flags use; an id a pack does not define is a `skipped` item with the registry's message | none |

## JSON shape (`sv-verify-auto-response.schema.json`, regenerated)

| field | where | when present | meaning |
|---|---|---|---|
| `template` | `properties[]` | the row is a `@mununu_template` or `--template` instance | the instance as written, e.g. `rtl_reachable TARGET="state_q == 3"`; `formula` is what it instantiated to |
| `assumes_exempt` | `properties[]` | the row's head says `assumes=ignore` | what the modifier did in this run (judged on every trace / changed nothing, with the reason) |

Both are optional and additive; nothing existing moved or changed meaning.

## Text output

```
  [assert] ann_guarantee_3: HOLDS
        label: trip_holds
        source: pump_pair.sv:19
        template: rtl_hold_until HOLD=lof_trip RELEASE=stop_cmd
        formula: nu X. ((((!(lof_trip)) || stop_cmd) || [] lof_trip) && [] X)
        assumes: exempt — judged on every trace (annotation modifier assumes=ignore); the other rows are judged under the applied assumptions
```

The assumptions note lists the exempt rows by label: `Judged on every trace by annotation (\`assumes=ignore\`): trip_holds.`

## Grammar

```systemverilog
// @mununu_template(<name>[, assumes=ignore|precondition]) <id> KEY=VALUE KEY="a value with spaces" …
// @mununu_guarantee(<name>[, assumes=ignore|precondition]) <mu-calculus | ltl …>
```

The name comes first and once; a VALUE with whitespace is double-quoted, no escapes; a KEY without `=`, a repeated KEY, an unknown id or parameter, a quote that never closes: the line is skipped with that reason. `mununu templates --domain rtl` lists the ids and their parameters.

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the scanner and the JSON fields live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the image gate below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |

## Provenance

Branch `feat/template-annotations`. Host: the scanner tests (`a_template_annotation_is_a_named_row_with_its_instance_beside_it`, `a_template_flag_becomes_a_template_annotation_under_its_old_label`, `a_bad_template_annotation_is_skipped_with_the_reason`, `the_assumes_modifier_marks_a_row_exempt_and_a_bad_one_is_refused`, `an_exempt_row_is_not_relativized_and_the_report_says_what_the_modifier_did`, `a_refused_annotation_head_is_skipped_with_its_reason_not_dropped` — the last one fails on the parent commit: nothing was reported), the annotation-grammar tests, the schema drift tests. Image: the `#[ignore]`d e2e named in the PR body.

## Not covered here

Statement-level counterexample slicing and repair-candidate localisation (the next two PRs of the same plan); a `--properties FILE` form outside the RTL comments (not built; the annotation form is the persisted one).
