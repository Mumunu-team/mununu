# Consumer briefing — the exact engine's build, relation and fixpoint steps, and the owned driver, honour the run budget (mununu#816, 2026-10)

> Source of truth: [`charge_iteration`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs#L5149) + [`check_node_budget`](../../crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs#L4095) + [`decide_reach_owned_only`](../../crates/mununu-core/src/adapter/reach_portfolio.rs#L772) + [`run_budget::expired`](../../crates/mununu-core/src/adapter/run_budget.rs#L152) — surface: (CLI+API) — every budgeted `sv verify-auto` / `btor2 verify --owned-only` run and the HWMCC adjudication harness.

**Audience:** rosf, monono and anyone who runs mununu under a wall-clock budget (`MUNUNU_VERIFY_BUDGET_MS`, `MUNUNU_PROPERTY_BUDGET_MS`, `MUNUNU_RESCUE_SLICE_MS`, the HWMCC harness's `MUNUNU_HWMCC_TIMEOUT_MS`) and reads `bottom_reason` / the abstention kind.

## TL;DR

- **Behaviour change, budgeted runs only.** Three places still ignored the caller's run budget after #810 (mununu#804): the exact engine's bit-blast build and its squaring relation build, and the detached member threads of the owned reach driver (`btor2 verify --owned-only`, the owned HWMCC ceiling). All three poll it now. In addition, a fixpoint step — one uninterruptible OxiDD apply — is refused **before it starts** when the run budget could not hold even the previous step. That bounds a gradually growing fixpoint; it does not bound a step that jumps (bufferAlloc's third step runs past 24 min after a 1.9-s second step, see Measured).
- **What you see instead:** the row abstains with the `deadline` abstention kind (`bottom_reason: budget-expired`, host-dependent) and one of three messages: `abstained on the caller's RUN budget … while building the BDD`, `squaring: declined on the caller's RUN budget while …`, or `abstained on the caller's RUN budget before fixpoint step N: the previous step took X ms and Y ms remain`. The predictor message appears only when the previous step already shows the budget cannot hold; a step that jumps still runs to its end.
- **Unbudgeted runs are unchanged.** No run budget installed ⇒ none of the polls fire and the step predictor is inert; the model's own `deadline` (the ladder's rung slice, the opt-in `MUNUNU_BDD_TIME_BUDGET_MS`) is deliberately NOT predicted, so default ladder verdicts are identical (the gates below).
- **New instrument:** `MUNUNU_REPORT_BDD_ITERS=1` prints one stderr line per fixpoint step (step index, total steps, live nodes, step duration).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | a budgeted row whose exact fixpoint used to overrun its budget by a whole step now abstains before that step, with `deadline`; `--owned-only` runs honour your run budget for the first time | none; if a row that used to decide near a budget edge now abstains, raise the budget — the message names the step and the time left |
| **monono** | same | none |
| **mununu-ui** | none (RTL verbs) | none |
| **HWMCC / corpus harnesses** | a per-instance budget is now a bound on the exact member's wall (within one step's growth) | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the polls live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | — |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | — |

## Measured

All image runs are in `mununu-sva` on the merge commit `098eecd` (the branch merged with `main`), one heavy workload at a time on an otherwise idle host. The readings were written before each run.

| instrument | result |
|---|---|
| `make verify-repro TEST=the_callers_run_budget_stops_the_bitblast_build` | **CONFIRMED**: fails at the parent, passes here |
| host: `exact_run_budget` (3 tests), `reach_portfolio` lib tests (15), workspace clippy | pass |
| `vis_arrays_bufferAlloc` alone, 300-s budget, `MUNUNU_REPORT_BDD_ITERS=1` | step 1 took 0 ms, step 2 took 1.9–2.2 s, step 3 did not return within 25 min (killed at 1,500 s) |
| the same, with the harness wall | `WALL` at 315 s, Unknown, `walled 1`; the test passes in 315 s |
| `diff_corpus_monotone_verdict_ledger` | pass, 40 s |
| `e2e_opentitan_sva_census` (self-pinned totals) | pass, 1,603 s |
| `e2e_opentitan_ip_edn_full` (67 per-row pins) | pass, 888 s |
| HWMCC'20 bv sweep, 68 instances under 60 KB, 300-s budget per instance | finishes in 6,414 s: 52 decided (28 reachable, 24 unreachable), 16 abstained (2 walled), 0 contradictions, 0 label mismatches |

**The sweep, against the 2026-10-27 run on `main`.** That run adjudicated 67 instances and then sat on `vis_arrays_bufferAlloc` for 27 min until it was stopped by hand. Here all 68 finish. The 67 verdicts are identical instance by instance, and bufferAlloc is Unknown. Two instances reach the harness wall, both at exactly 315 s: `vis_arrays_bufferAlloc` and `krebs.3.prop1-func-interl`. Krebs was Unknown on 10-27 too, so it moves no verdict. Per-engine contributions: rIC3 44, SPACER 14, pono 8, btormc 6, exact 6, native 4. This is reading S1 of the pre-registered three.

**What the step predictor does not do.** On bufferAlloc a 1.9-s step is followed by a step longer than 24 min. A predictor that sees only the previous step cannot refuse that step. The predictor bounds the overshoot when steps grow gradually; it does not bound a sudden jump. The harness wall is what bounds this instance, and the wall lives in the measurement harness only.

## Provenance

- Issue: mununu#816 (class `silent`, `found:hwmcc`); parent #804 (#810), whose briefing recorded the build as "bounded by the bit cap, not by time" under "Not covered here".
- Diagnosis: the CLI on the 0.5.3 release binary (`39e99da`; the exact engine byte-identical to `main`) with `RUST_LOG=mununu_core::adapter::btor2::symbolic_bitblast=debug` + `MUNUNU_REPORT_BDD_LEVELS=1`, killed at 150 s: the build finished at 12 s (44-bit cone, 1,928 ops), nothing printed for the next 138 s.
- Policy: [`docs/policies/failure-paths.md`](../policies/failure-paths.md) Rule 1 and the sibling sweep, [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- A step that is itself longer than the whole budget still runs to its end on the production path (`sv verify-auto`, `btor2 verify`): one OxiDD apply cannot be interrupted (oxidd 0.13 has no cancellation), and the predictor only sees the previous step. A hard wall would need the exact member on a thread abandoned at the deadline, which #504 rejected for the memory ceiling's sake; it stays rejected there. The HWMCC adjudication harness does wall each instance at its budget plus 15 s (the abandoned member holds one core until its apply ends); that is a measurement-harness choice, not a product behaviour.
- The model's own `deadline` is not predicted; a ladder rung's slice can still be overrun by one step, as before.
