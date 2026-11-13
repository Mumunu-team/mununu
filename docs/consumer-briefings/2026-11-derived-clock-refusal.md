# Consumer briefing: a design clocked only by a derived clock is refused at lift (2026-11)

> Source of truth: [`refuse_merged_clock_domains`](../../crates/mununu-core/src/adapter/yosys/mod.rs) — surface: CLI+API

**Audience:** anyone who lifts SystemVerilog through `mununu sv …` (CLI) or the `/api/v1/sv/*` routes, and anyone who reads `diagnostics.merged_clock_domains` or the `clock-domains-merged` note.

## TL;DR

- **The defect.** A design whose flops all sit on a *derived* clock lifted silently as if that clock ticked on every step. Examples of a derived clock: a gated clock (`gclk = clk & en`), a divided clock, or a muxed clock. In the lifted model, the gating logic was computed and never read.
  - For `always_ff @(posedge gclk)` with `gclk = clk & en`, the model counted on every step, whatever `en` was.
  - A verdict on such a model is a claim about a different design.
- **Now:**
  - The lift **refuses** such a design. The error says "the clock net `<net>` is derived from logic (driven by `<cell>`: a gated, divided or muxed clock)" and names `--merge-clock-domains` as the opt-in.
  - The existing more-than-one-domain refusal (mununu#762) is unchanged.
- **With `--merge-clock-domains`** (API: `"merge_clock_domains": true`), the design lifts as before. The run then reports:
  - `diagnostics.merged_clock_domains`: one entry, suffixed `, derived: driven by \`$and\`` (or the driving cell);
  - the `clock-domains-merged` SoundnessCaveat note, whose summary reads "a derived clock merged onto the model's clock — every verdict assumes it ticks on every step."
- **Where a clock is a root.** A clock driven by a pass-through (`$not`, `$buf`, `$pos` and their gate-level forms) is traced back to its source. A clock driven by a top-level port, or by a black box (a PLL or clock-gating cell the design does not define), counts as a root and lifts as before.
- **Latches are not affected.** They already lift correctly at cycle level.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A design clocked only by gated or divided logic now fails the lift with the message above, where it used to return verdicts. | If such a design is intended, pass `--merge-clock-domains` and read the `clock-domains-merged` caveat. Better: model the gate as an enable on the root clock. |
| **monono** | Same. | Same. |
| **mununu-ui** | No SV lift view consumes this. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** roadmap B2 (the clock-domain track). Track B as written was largely already covered by mununu#762, the multi-domain refusal. A measurement in `mununu-sva` (yosys 0.68 with slang) found the remaining hole: a lone derived clock.
- **Tests:**
  - `clock_domains_marks_a_gated_or_divided_clock_as_derived`: the classifier, against a netlist fixture;
  - `a_lone_derived_clock_domain_is_refused_unless_merged`: the refusal and the opt-in, at the seam;
  - `e2e_a_design_clocked_only_by_a_gated_clock_is_refused_at_lift`: the real lift, in the image.
- **JSON shape.** It is unchanged: `merged_clock_domains` was already a list of strings. Only its description changes, so the schemas `sv-verify-auto-response` and the `sv-*-request` family differ in text only.

## Not covered here

- **A design with a root clock and a derived one** was already refused, as two domains (mununu#762). It is still refused, but now with the derived-clock message, which lists every domain and marks the derived one. A gate that matches the old "clock domains" wording should match on the refusal itself, not on its text.
- **Clock-enable flops** (`$dffe`) use a root clock with an enable. They are not derived clocks and are unaffected.
