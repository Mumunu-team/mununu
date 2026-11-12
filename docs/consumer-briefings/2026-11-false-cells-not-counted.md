# Consumer briefing: a `violated` row with no cell count omits `false_cells` (2026-11)

> Source of truth: [`VerifyOutcome::false_cells_counted`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: CLI+API

**Audience:** anyone who reads `false_cells` or `detail` on `sv verify-auto` rows, from the CLI's `--json` output or from `POST /api/v1/sv/verify-auto`.

## TL;DR

- **The defect.** Some paths decide a row `violated` without counting cube cells:
  - a row resumed from a breadcrumb (`decided_by: "resumed"`);
  - a constant formula;
  - a reach-portfolio or rescue verdict.

  These rows reported `false_cells: 0`, which reads as a count of zero, not as "not counted".
- **Now:**
  - Such a row **omits `false_cells`**, the same way a non-`violated` row already does.
  - Its `detail` reads `"cell count not computed"`. A resumed row keeps its `"resumed from the breadcrumb of build …"` detail.
  - The CLI text says `VIOLATED (cell count not computed)` instead of `VIOLATED (0 cell(s))`.
- **Why 0 is never a count.** A row the cube engine decides `violated` has at least one definite-false cell, so a counted figure is always ≥ 1.
- **Verdicts do not change.** `outcome`, `decided_by` and the trace are unchanged.
- **JSON shape.** `false_cells` was already optional (`integer | null`, omitted when absent), so the schema type is unchanged and only its description changes. A consumer that read `p["false_cells"]` on every `violated` row now gets a missing key on these rows.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A `violated` row decided by the portfolio or resumed has no `false_cells`. | If a gate reads the field, use `.get("false_cells")` and treat a missing key as "not counted", never as 0. |
| **monono** | Same. | Same. |
| **mununu-ui** | The SV verify-auto view shows `detail` as text. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** mununu#946, found by the R0 delivery-lane baseline review.
- **Test:** `a_violation_decided_without_counting_cells_omits_the_count`, at the report-to-wire seam.
- **Sibling sites swept:**
  - every `Violated { false_cells: 0 }` constructor in `verify_auto.rs` (resumed, constant, reach-rescue and the portfolio paths) and `resume.rs`;
  - the three renderers: the wire view in `api/models.rs`, the CLI text, and the `expectations` failure message.

  The constructors are unchanged, because the rule sits at the renderers: `0` means not counted.

## Not covered here

- The `cegar` responses' `cells.false_cells` is a real cube count, so it can be 0. It is unchanged.
