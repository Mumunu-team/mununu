# Consumer briefing — a violated `ltl … W …` row now ships its counterexample (2026-10)

> Source of truth: [`translate_universal_text`](../../crates/mununu-core/src/ltl/universal.rs) + [`safety_witness`](../../crates/mununu-core/src/adapter/reach_rescue.rs) — surface: (CLI+API) — every `@mununu_guarantee ltl …` row of `sv verify-auto` / `POST /api/v1/sv/verify-auto`, and CTXDSL `ltl` bodies.

**Audience:** rosf, monono and anyone who reads a `trace` out of a VIOLATED annotation row or replays it with `--emit-testbench`. mununu#821.

## TL;DR

- **Bug fix, no new flag.** The universal LTL translation spelled `p W q` as `nu W. (q || (p && [] W))`; the one-path safety witness monitor recognises a weak-until block only in the And-root spelling `nu W. ((q || p) && (q || [] W))`, so a VIOLATED `G (a -> X (!a W b))` annotation shipped with no path and no note, and `--emit-testbench` wrote nothing for it. The translation now uses the And-root spelling (same reading); the row carries its path, found by native BMC on the monitor (rIC3 first where present).
- **Second site, same bug report:** the report's notes rebuild (`build_notes`) discarded every per-property note a verdict pass had pushed before it — the `witness-unavailable` of a VIOLATED row with no path (mununu#803), `expression-leaf-unresolved` (mununu#717), `engine-isolation` (mununu#543), `array-atom-unsupported` (mununu#518) and the fault posture's `antecedent-*` notes. They now reach the report.
- **What you may see:** `trace` on violated `ltl` rows that had none; the row's `formula` text changes spelling (`(nu W2. ((q || p) && (q || ([] W2))))`); the five note kinds above where they were silently missing; verdicts do not move.
- CTXDSL `ltl` bodies with `W` translate to the same new spelling (the evaluator reads both identically).
- JSON shape, schemas, flags, exit codes: unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | violated weak-until annotation rows gain a `trace`; a stored `formula` string for such a row differs in spelling | none required; compare formulas by verdict, not by text |
| **monono** | same; `--emit-testbench` now writes a bench for these rows | none required |
| **mununu-ui** | CTXDSL `ltl … W …` bodies translate to the new spelling; evaluation unchanged | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the translator lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | the verify-auto path consumers run |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| `an_ltl_weak_until_violation_ships_its_path_through_the_safety_witness` (two 1-bit registers; `G (a_q -> X (!a_q W b_q))`) | the And-root spelling; `safety_witness` returns a 3-cycle path with `a_q` at cycles 1 and 2; `attach_safety_witnesses` attaches it to a violated row |
| `a_violated_row_the_witness_monitor_cannot_compile_says_so_in_a_note` (the old spelling as the row's formula) | `attach_safety_witnesses` leaves a `witness-unavailable` note at stage `monitor` ("the body is not a bounded-depth box / boolean shape") |
| `the_notes_rebuild_keeps_what_the_verdict_passes_said` | the note survives `build_notes`; a stale copy of a regenerated kind is not duplicated |
| `e2e_a_violated_annotation_ships_its_path_or_says_why_not` (`mununu-sva`, 2026-10-28: slang lift, exact engine, the post-passes) | PASS — the `ltl` row is VIOLATED with a replayable path in the new spelling; the old-spelling row is VIOLATED with no path and a `witness-unavailable` note at stage `monitor` |
| mununu 0.5.5 probe (the tutorial's block, old spelling as a mu body) | VIOLATED, no path, **no note** in the shipped JSON — the rebuild dropped it |
| the tutorial's block (private material), re-run on the 0.5.6 release binary (`7586f36`) | the `alternates` row (`G (start_a -> X (!start_a W start_b))`, VIOLATED by `exact-symbolic`) ships a 4-cycle path — start on pump A, stop, start on A again; `--emit-testbench` writes `replay/draft1/alternates` (16 checks); Verilator `make` → `MATCH`, `make trace` → `MATCH` + `replay.vcd`. On 0.5.5 the same row shipped no path and no note. |

## Provenance

- mununu#822, merged 2026-10-28 as `05538b9` (closes mununu#821, filed from the tutorial material's first run on 0.5.5); released in 0.5.6 (#823, `7586f36`).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

The exact engine's own witness extractor still has no path shape for nested-ν formulas; the path comes from the safety-witness post-pass, which runs for every violated safety row whichever engine decided it.
