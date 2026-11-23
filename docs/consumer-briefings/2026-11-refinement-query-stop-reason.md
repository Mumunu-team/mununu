# Consumer briefing: a refinement query the solver did not answer is reported as that, not as a grammar ceiling (2026-11)

> Source of truth: [`RefineGap::cut_short`](../../crates/mununu-core/src/adapter/btor2/refine.rs#L146), [`barren_refinement`](../../crates/mununu-core/src/adapter/btor2/abs_safety.rs#L81), [`discovery_stop_detail`](../../crates/mununu-core/src/adapter/recoverability.rs#L3559) and [`craig_fallback_warning`](../../crates/mununu-core/src/adapter/btor2/cegar.rs#L1926) — surface: CLI+API — the `detail` of `mununu btor2 verify --engine ic3` (CLI only) and `--engine cube` / `POST /api/v1/btor2/verify` with `engine: "cube"`; the `warnings` of `mununu btor2 cegar` / `POST /api/v1/btor2/cegar` under the `craig` predicate source

**Audience:** anyone who reads the `detail` text of `btor2 verify` under `--engine ic3` or `--engine cube`, anyone who selects the `craig` predicate source (`--predicate-source craig`, `predicate_source: "craig"`, the sidecar's `cegar.predicate_source`), and anyone who installed cvc5 to get that source.

## TL;DR

- **No verdict changes.** The same runs return the same `outcome`. What changes is the text that says why a refinement gave nothing, and where it is said at all.
- **`--engine ic3`: "grammar ceiling" now means what it says.** The abstract IC3 engine refines its abstraction with interpolation queries to cvc5. When a refinement step gave nothing new, `detail` said "grammar ceiling" whatever had happened — also when cvc5 was not installed, or stopped at the query limit (mununu#1188). The words are now kept for a step whose every query was answered. Otherwise `detail` names the query that was not: the solver was not found, it stopped at its limit, it failed, the run was cancelled, or the query could not be posed.
- **The query limits are fixed, and the text says so.** They are 10 s per query on the abstract IC3 engine and on the `craig` source, and 4 s on the cube's predicate discovery. No flag, variable or sidecar key changes them, so a stop at one is reported as a mununu limitation with `--engine portfolio` as the way out, and never with a budget to raise.
- **`--engine cube`: `detail` can now be set.** It was always `null` for the cube. When the cube's last-resort predicate discovery stopped on an unanswered query, `detail` says so. On an `unknown` that is a reason the cube may have lacked a predicate; on `holds` / `violated` the note says the verdict does not depend on the discovery: it only proposes predicates for the cube's abstraction, and a definite verdict is sound whichever predicates that is built on (the ones a stopped discovery had already returned are used like any others).
- **The `craig` predicate source runs MathSAT 5, and now says so.** Its start-of-run check looked for cvc5, which the source does not run. A host with cvc5 and no MathSAT refined with `wp` and reported nothing; a host with MathSAT and no cvc5 was told the source was off. The check now looks for MathSAT, and the warning names `MUNUNU_MATHSAT_PATH`.
- **A `craig` query that MathSAT did not answer is a warning**, once per kind of stop per run; that iteration refines with `wp`, as before. A query MathSAT answered with no usable interpolant is not a warning.
- **The `mununu-sva` image does not ship MathSAT** (checked 2026-11-23). In that image `--predicate-source craig` has been refining with `wp`; it now says so. The user documentation that called `craig` a cvc5 feature is corrected.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | If a lane reads `detail` of `btor2 verify --engine ic3` or `--engine cube`: the text differs, and for the cube it may now be present. If a lane selects the `craig` source in the `mununu-sva` image: its `warnings` now has one more entry, which says the run refined with `wp`. | Do not match the `detail` or warning prose. To use the `craig` source, install MathSAT 5 and set `MUNUNU_MATHSAT_PATH`. |
| **monono** | Same. | Same. |
| **mununu-ui** | None: these are RTL verbs, not CTXDSL capabilities. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the fix. | They ship the `mununu` binary. No tool was added to or removed from any image. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** the review of the solver-child fix (mununu#1186), which followed the stop reason of a solver query to its callers and found three that dropped it (mununu#1188). Read from the code; the reproducer below is the first run of it. The MathSAT findings came from following the `craig` source's query to the solver it starts.
- **Class:** `member-stop-reason` in [`docs/defect-classes.md`](../defect-classes.md), which holds the case table.
- **Tests:** `x1188_a_refinement_cut_short_at_the_query_limit_is_not_called_a_grammar_ceiling` (the reproducer: the abstract IC3 engine with a stand-in cvc5 that stops at its limit) and its control `x1188_a_refinement_whose_every_query_was_answered_is_the_grammar_ceiling`; three tests over every kind of gap (`x1188_a_step_is_cut_short_exactly_by_the_gaps_the_solver_did_not_answer`, `x1188_every_gap_kind_decides_whether_a_barren_refinement_is_the_grammar_ceiling`, `x1188_the_cube_reports_a_discovery_stop_exactly_when_a_query_was_not_answered`); `x1188_every_way_cvc5_ends_a_refinement_query_has_its_kind` and `x1188_a_discovery_cut_short_at_the_query_limit_carries_the_stop` at the seam with the solver runner; and for the `craig` source `x1188_the_craig_source_warns_at_loop_start_exactly_when_mathsat_is_not_found`, `x1188_a_missing_cvc5_does_not_change_what_the_craig_source_says` and `x1188_a_craig_step_cut_short_is_reported_once_per_kind_and_an_answered_one_never`.
- **API schemas:** no shape change. `detail` and `warnings` are existing fields.

## Not covered here

- **The limits stay fixed.** This change reports a stop at one; it does not make the limit follow the run's budget. A limit that followed a wall clock would make the refinement, and with it an `unknown`, depend on the host.
- **`--engine ic3` is experimental and CLI-only**, as before. The default portfolio does not run it.
- **The `verify` project flow's cube rows carry no detail**: its report has no field for one, so a discovery stop is not shown there (mununu#1225).
- **Two other ways the cube returns `unknown` still give no reason** (an error of its refinement loop; a `violated` withheld on a design with `constraint` lines): mununu#1225.
- **A bounded-reachability check that errors inside the abstract IC3 engine is still dropped**, which is the one way that engine can say "every interpolation query was answered" about a step that asked none (mununu#1224).
- **Whether another image ships MathSAT was not checked.** Three code comments name a second image as the place to run the MathSAT-gated tests; they are left as they were.
- **The refinement query the abstract IC3 engine asks is unchanged.** Its single-step query does not use the counterexample's cube (mununu#1227); this change reports how a query ended, not which query is asked.
- **An operator the encoder does not take is reported with a sentence about the sampling inference** on paths that have none (mununu#1228). The query is reported as one that could not be posed; the borrowed sentence is still in the text.
