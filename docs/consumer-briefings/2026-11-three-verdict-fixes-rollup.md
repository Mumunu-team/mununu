# Consumer briefing: three verdict fixes that shipped without a briefing — SAT BMC under constraints, disagreeing justice rungs, resume over an edited header (2026-11)

> Source of truth: [`dynamic_search`](../../crates/mununu-core/src/adapter/btor2/sat_bmc.rs#L341), [`merge_parallel_rungs`](../../crates/mununu-core/src/adapter/justice.rs#L518) and [`run_fingerprint`](../../crates/mununu-core/src/adapter/resume.rs#L274) — surface: CLI+API — every verb that runs the reach portfolio (`btor2 verify`, `sv verify-auto` and the verbs built on them, with their `POST /api/v1/…` peers); `btor2 verify-justice` / `sv verify-liveness` with `--parallel-rungs`; `sv verify-auto --resume-from` / API `resume_from`

**Audience:** rosf, monono, mununu-ui, and anyone who reads a SAT BMC depth claim, a justice row run with `--parallel-rungs`, or a resumed `sv verify-auto` run.

## TL;DR

Three PRs merged on 2026-11-20 changed what a verb can return and shipped no briefing. The cadence review of 2026-11-23 found them. This page is their briefing.

| PR | issue | before | now |
|---|---|---|---|
| #1157 | #1141 | The owned SAT BMC's dynamic step (the default) asserted every window frame's `constraint`. A `bad` at frame f whose every continuation broke a constraint later in the window was reported refuted: the run said "no counterexample through depth N" and missed a real counterexample. It never gave a wrong `violated`. | Each frame's constraint is tied to a selector, so a `bad` at frame f needs the constraints on frames 0..=f only, as BTOR2 defines. That counterexample is now found: `violated` where the row used to be `unknown` with a depth claim, and a `shortest` witness that is really the shortest. |
| #1159 | #1140 | With `--rung both --parallel-rungs`, the first definite rung verdict won. A rung that finished just after it and disagreed was never read, and a rung that panicked left the row `unknown` with no note. | The rungs are read for 250 ms more after the first verdict. Two definite verdicts that disagree (HOLDS against VIOLATED) give `unknown` with a soundness-alarm note naming both rungs, and a mununu defect to report. A panicked rung is named in the notes. An agreeing run gets the same verdict as before. |
| #1163 | #1146 | The run fingerprint hashed the include directories' paths, not the files in them. After an edit to an `` `include``d header, `--resume-from` accepted the old breadcrumb and kept its `holds` / `violated`. | The fingerprint hashes every file the include directories reach, by path and content. A resume after a header edit is refused with the existing run-mismatch message, and the run starts fresh. |

- **No JSON shape, field, note kind or flag changed in any of them.** #1159 adds note *text* (notes are free text); #1157 and #1163 change only which verdict or refusal a run reaches.
- **Which way the verdicts move.** #1157 turns some `unknown` rows into `violated`. #1159 turns a contradictory pair into `unknown`; it could hide a verdict only when two engines disagree, and that disagreement is itself a defect to report. #1163 turns a wrongly accepted resume into a refusal.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | A design with SVA `assume`s (lifted to BTOR2 `constraint`) can now get a `violated` row where it got `unknown` with "no counterexample through depth N". | Re-run any gate whose baseline holds such a row; a new `violated` is a real counterexample (the SAT model was always a real path). |
| **monono** | Same as rosf for constrained designs. If you run `sv verify-liveness --parallel-rungs`, a contradiction note can now appear. | Treat the contradiction note as a mununu defect and report it with the design; do not retry it away. |
| **mununu-ui** | No new field or kind. Notes are shown verbatim. | None. |
| **any caller of `--resume-from`** | A breadcrumb from before a header edit is now refused. | Start the run without `--resume-from`, or from a breadcrumb written after the edit. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, from a `main` at or after e15260c4 (#1163, the last of the three). No release carries them yet: v0.5.11 predates all three. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin when a release carries the fixes. | They consume the binary. |

## Provenance

- **Found by:** the laptop's cadence review of 2026-11-23, gate A2 (Cross-Repo Impact Reporting): three verdict-semantics changes with no briefing.
- **Tests:** each PR's own reproducer, named in its `Repro:` trailer:
  - #1157: `x1141_the_dynamic_step_assumes_constraints_only_up_to_the_bad_frame`, which fails on its parent. The differential tiers `sat_bmc_dynamic_step_agrees_with_native_bmc_on_constrained_draws` and `ic3_agrees_with_native_bmc_on_constrained_draws` back it.
  - #1159: `x1140_disagreeing_parallel_rungs_merge_to_unknown_with_a_contradiction_note`. It is inconclusive on its parent because the merge function is new.
  - #1163: `x1146_an_edited_included_header_changes_the_run_fingerprint`, which fails on its parent.

## Not covered here

- The sequential justice ladder (no `--parallel-rungs`) and the response verbs' proof fallback. Both run a later rung only on `unknown`, so they never hold two verdicts to compare; they are unchanged.
- The `MUNUNU_*` budget variables, which stay outside the run fingerprint. They can turn a verdict into `unknown`, never flip one.
