# Consumer briefing: a `btor2 cegar` VIOLATED tally says whether it is established at a reset state (2026-11)

> Source of truth: [`CegarTrace::violation_at_reset`](../../crates/mununu-core/src/adapter/btor2/cegar.rs) and [`CegarVerdictSummary::violation_at_reset`](../../crates/mununu-core/src/api/models.rs) — surface: CLI+API — `mununu btor2 cegar` (the `outcome` line and the `--json` key `violation_at_reset`) and `POST /api/v1/btor2/cegar` (`cells.violation_at_reset`)

**Audience:** anyone who reads a `btor2 cegar` VIOLATED verdict as "the property fails from reset", on designs where the cube path's cycle-0 filter kept an initial cube on doubt (mununu#1085: an inconclusive query, an expired budget, a memory `init` the encoder could not take, …).

## TL;DR

- **New field, additive (mununu#1106, after mununu#1085).** `cells.violation_at_reset` is present whenever `false_cells > 0` on the final summary: `{ "established": bool, "detail": "…" }`. `established: true` means an initial cube proved inhabited at cycle 0 falsifies the formula. `established: false` means either every falsifying initial cube was kept on doubt (the `detail` names each cube, its doubt and the remedy — the same text as the `[mununu#1085]` warning) or no initial cube falsifies at all (the falsifying cells lie elsewhere in the cube, which the tally counts and a reset state may never reach).
- **The counts do not change.** `true_cells` / `false_cells` / `unknown_cells` / `unlifted_cells` are what they were: a tally over every lifted cell. The CLI's `outcome` line gains the same reading after a semicolon: `PROPERTY VIOLATED — N cell(s) falsify the formula; NOT established at a reset state: …`. Until now a definite VIOLATED stood beside the doubt warning with nothing tying them together.
- **Per-iteration summaries** (`iterations[].cells`) carry no reading (`violation_at_reset` absent); the final `cells` does.
- **Schema:** `docs/api-schemas/cegar-response.schema.json` regenerated.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A gate that treats `false_cells > 0` as "fails from reset" should read `cells.violation_at_reset.established` first; `false` with a doubt in `detail` is the same situation `sv verify-auto` reports as `violation-at-unestablished-initial-cubes`. | Branch on `established`; surface `detail`. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: the UI does not read the cegar response. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the field. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the review of mununu#1105 (the laptop, 2026-11-19): `btor2 cegar` shipped a definite `violated` beside the "not established" warning; recorded on mununu#1106 as a sibling.
- **Tests:** `x1106_a_cegar_violation_over_doubt_kept_initial_cubes_says_it_is_not_established_at_reset` (the reproducer: the mununu#1061 fixture with a malformed memory `init`, so every initial cube is doubt-kept; compiles on the parent and fails there on the missing key), `x1106_a_violated_tally_is_read_against_the_initial_cubes` (the seam: the three readings and the none case).

## Not covered here

- Reachability from an established initial cube to a falsifying cell (a violation established at reset but only after some steps) is not computed: `established` is decided on the initial cubes themselves, which is also the counterexample trace's own start criterion. A violation only reachable from reset reads as `NoInitialCellFalsifies` here; `sv verify-auto`'s cube path withholds it the same way.
