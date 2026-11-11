# Consumer briefing: a resumed verify-auto row is checked for vacuity and contradiction; annotation scope fix (2026-11)

> Source of truth: [`apply_resume`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs), [`extract_from_sv_source`](../../crates/mununu-core/src/mununu_annotations/mod.rs) — surface: CLI+API

**Audience:** anyone who reads `sv verify-auto` JSON, from the CLI (`--resume-from`) or `POST /api/v1/sv/verify-auto` (`resume_from`). In particular, a gate that reads `outcome` beside `vacuity`, and authors of `@mununu_guarantee` annotations in modules whose header starts with an attribute.

## TL;DR

- **Resumed rows are now checked against the report's other verdicts.** A row carried from a breadcrumb (`decided_by: "resumed"`) is now judged against the fresh verdicts in the same report, exactly as in an unbroken run:
  - A resumed recoverability `holds` can now carry a `vacuity` finding, with its `vacuous-guarantee` note.
  - A resumed verdict that contradicts a fresh one is withheld.
  - **Before:** a resumed row always shipped `vacuity: null`, which read as a checked, non-vacuous `holds`.
  - **Still not re-checked:** the sec-cm antecedent check, which needs the reach portfolio. The `resumed-from-breadcrumb` note's detail now says so.
- **Module headers that open with an attribute or a lifetime.** A header like `(* keep *) module m (…)`, or `module automatic m`, now opens the module's scope for the annotation scan.
  - Before, the `@mununu_guarantee` lines inside such a module were treated as top-level, so a submodule's row did not follow its instances (#897), and an atom could bind a same-named net of the parent.
  - Such a row can now expand per instance, which changes its name and count.
- **Unchanged:** the JSON shape (`vacuity` already existed on every row), every verdict of an unbroken run, and every row whose module header has no attribute.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | Only if you resume runs (`--resume-from`): a resumed `holds` may now arrive with `vacuity` set. | Treat it as you treat a fresh row's `vacuity`. |
| **monono** | Same for resumed runs. Separately, any `@mununu_guarantee` inside a module whose header starts with `(* … *)` now binds per instance. | Re-run the affected rows once and compare names/counts. |
| **mununu-ui** | None: the UI does not resume runs. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | Yes, to get the fix. | It ships the `mununu` binary. |
| the on-prem bundle (`docker/Dockerfile.bundle`) | Yes. | Same. |
| `mununu/docker/Dockerfile.sva` | Yes, for e2e consumers. | Same. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use, or do not run verify-auto. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **mununu#965**, from the tier-B review of 2026-11-09.
- **Tests:**
  - `a_resumed_holds_is_checked_for_vacuity_against_the_fresh_verdicts`
  - `a_module_header_with_attributes_or_a_lifetime_opens_its_scope`
  - Each one was checked by hand to fail on the parent commit with only the test added.

## Not covered here

- The other two #965 items change no output:
  - the `--ignore-assertions` SOUNDNESS comment;
  - the native-BMC memory error, which no input reaches.
- An immediate `assume` in procedural code is still dropped without a note: mununu#1001.
