# Consumer briefing — the engine modules deny `unwrap`; an exhausted BDD arena on the cube path is now an abstention, not a crash (2026-10)

> Source of truth: [`AbstractRelation::evaluate`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs) + the module-level `#![deny(clippy::unwrap_used)]` in the twelve engine files — surface: (CLI+API) — every verb that runs the exact or cube engines (`sv verify-auto`, `btor2 verify`, `btor2 game`, the recoverability verbs and their `/api/v1/...` peers)

**Audience:** rosf, monono and anyone who gates on mununu's exit code or parses its report. First landing of `docs/policies/failure-paths.md` Rule 2 (M1 of the 2026-10-25 plan).

## TL;DR

- **Behaviour change, no new flag.** On the predicate-cube path (the `AbstractRelation` evaluator, its game predecessors, `verdict_at`, `may_targets`, the minterm helpers), an OxiDD `OutOfMemory` used to be an `unwrap` panic — exit 101, or SIGABRT when the unwind dropped the exhausted manager — with no report. It is now the attributed NODE-budget abstention (`⊥`, reason "BDD arena exhausted … raise `MUNUNU_BDD_ARENA_NODES`, or use `--engine explicit`"), carried in the report like every other abstention. Designs that never exhausted the arena see no change; verdicts do not move.
- **Build-time guarantee.** `#![deny(clippy::unwrap_used)]` on `symbolic_bitblast`, `bit_blast`, `native_bmc`, `native_spacer`, `native_interp`, `native_boolector`, `fault`, `bad_monitor`, `cegar`, `reach_portfolio`, `reach_rescue`, `fault_adversary`: a new `unwrap` in an engine does not compile. The witness / strategy extractors of the exact engine still carry a marked, shrinking allow list (M1 PR 2).
- Two small paths say what happened instead of panicking: CEGAR's exhausted-source return with no iteration (an `IrConsistencyError`), and cvc5's stdin not being piped (an error string).
- JSON shape, schemas, flags, exit codes: unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a run that used to die with exit 101 / 134 on a wide cube now returns a report with `⊥` and the arena reason; your "crash" bucket shrinks, your `unknown` bucket may grow by the same rows | none required; if you special-cased exit 101, read the `bottom_reason` instead |
| **monono** | same | none required |
| **mununu-ui** | nothing (RTL verbs) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the abstention lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow needs the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | the gates below ran here |
| `mununu/docker/Dockerfile.profile`, `.extract*` | no | unaffected |
| rosf / monono images | behaviour updates on the next binary pull | — |

## Measured

| run | result |
|---|---|
| `an_arena_exhausted_in_the_cube_path_abstains_instead_of_panicking` (new) | scans arena sizes over a 10-bit incrementer with eight predicates: no size panics, at least one decides, at least one abstains with the reason |
| `an_arena_exhausted_inside_the_evaluator_abstains_instead_of_panicking` (#730, unchanged) | still green |
| host: `cargo clippy -p mununu-core --all-targets --all-features -- -D warnings`; unit tests of the touched modules | clean; `symbolic_bitblast` 122 passed, `cegar` 62, `symbolic_engine` 9, `kmts_lift` 85 |
| verdict-identity gate in `mununu-sva` (2026-10-26): `e2e_opentitan_sva_census` on this branch vs `main` bc82372 | **0 verdict moves** — 191 rows, 173 holds / 7 violated / 11 skipped / 0 ⊥ on both (2,297 s vs 1,961 s) |
| `diff_corpus_monotone_verdict_ledger` on this branch | PASS (28 s) |

## Provenance

- Branch `fix/deny-unwrap-engine-modules`; policy `docs/policies/failure-paths.md` Rule 2; plan `rosf-rescope-and-claude-config.md` M1.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- The fifteen witness / strategy extractors (`exact_reachable_stall_lasso`, `two_player_strategy`, …) and `exact_model_partitioned_with_lift` keep their `unwrap`s under `#[allow(clippy::unwrap_used)] // M1 pending`; an arena exhaustion there still panics inside `catch_unwind` (no witness) or aborts — M1 PR 2.
- `expect` is not denied: it is permitted when its message names an invariant the code itself established.
