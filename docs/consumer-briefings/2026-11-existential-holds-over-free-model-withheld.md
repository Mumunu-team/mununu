# Consumer briefing: a cover or `EF` reached only through a free net is withheld (2026-11)

> Source of truth: [`BottomReason::ExistentialHoldsOverClockAsData`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs), [`BottomReason::ExistentialHoldsOverFreeLeaves`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) and [`withhold_violated_over_free_leaves`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API — `mununu sv verify-auto` / `POST /api/v1/sv/verify-auto`, the rows of kind `cover` and any `EF`-rooted property

**Audience:** anyone who gates on a cover's `holds` (reachability) from `sv verify-auto`, or who branches on `bottom_reason.kind`.

## TL;DR

- **Verdict change (mununu#1126).** The pass that withholds a `violated` whose cone reads an undriven net (`violation-over-free-leaves`, mununu#750) or a clock net read as data (`violation-over-clock-as-data`, mununu#1030) now reads both directions by the formula's **path quantifier** (its modalities under negation, not its fixpoint kind): a universal (`[]`-only) `holds` and an existential (`<>`-only) `violated` transfer; a universal `violated`, an existential `holds` and either verdict of a branching formula (`AG EF`) are withheld. Two consequences beyond the covers: a `violated` `s_eventually b` over such a cone (a `mu` that is universal) is now withheld, where it shipped before; an `AG EF` `holds` over such a cone is withheld. On the mununu#1030 latch, `cover property (r != dq)` is unreachable on the hardware and shipped `holds` with no note; it is now `unknown`.
- **Two new `bottom_reason.kind` values:** `existential-holds-over-clock-as-data` (the clocks named in `detail`) and `existential-holds-over-free-leaves` (the undriven nets named). `determinism` is `reproducible`; no trace (a cover carries none); the row is not re-planned (every engine would reach the same run). A universal `holds`, an existential refutation (`violated` on a cover = unreachable) and every row whose cone reads neither kind of net are unchanged.
- **Shape:** two new values of an existing string field; the tag table on `kind` in the schema description lists them. No field is added or removed.
- **Not covered:** a `holds` on a universal row keeps its non-vacuity result, which is itself existential (the antecedent was reached) and has the same doubt over these cones; the parse-failure exits of the pass skip a row silently (tracked under mununu#966).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A cover that was `holds` over such a cone is now `unknown` with one of the two kinds. A gate that treats every `unknown` as "retry with more budget" must not on these (they say `Retrying cannot help`). | Branch on the two kinds beside `violation-over-free-leaves` / `violation-over-clock-as-data`. |
| **monono** | Same. | Same. |
| **mununu-ui** | The verify-auto panel shows the row as `unknown` with the kind and detail it already renders. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary; the verdict changes. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the cadence review of 2026-11-20 (mununu#1126), by reading `withhold_violated_over_free_leaves`; confirmed on the box in the image with the mununu#1030 latch plus a registered copy of `d` (the falsifier runs are on the issue).
- **Tests:** `x1126_an_existential_holds_over_a_clock_read_as_data_is_withheld` (the reproducer: a cover `holds` over the clock-read cone becomes `unknown` with the kind, a `violated` `s_eventually` over it is withheld, an `AG EF` `holds` is withheld, a cover over an undriven net gets the free-leaves kind; the data-enabled cover, the universal `holds` rows and the existential refutation keep their verdicts; compiles on the parent and fails there), and `x1126_path_quantification_reads_the_modalities_under_polarity` (the walk).

## Not covered here

- **Precision, not soundness:** a cover the hardware does reach is withheld too when its cone reads such a net (on the falsifier design, `cover (r == 1)` — reachable on the hardware — is withheld beside the unreachable `cover (r != dq)`): the pass cannot tell the two apart without modelling clock levels, so it withholds every existential `holds` over such a cone. Deciding such a cover is a mununu limitation today; the `detail` says what the design can change.

- The exact engine carries no witness out for a cover row, so the reachable path cannot be replayed under Verilator; that is a separate gap, not changed here.
