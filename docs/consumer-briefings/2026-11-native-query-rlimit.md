# Consumer briefing — the native member's per-query budget is a resource limit, not a clock (mununu#893, 2026-11)

> Source of truth: [`native_bmc::QueryBudget`](../../crates/mununu-core/src/adapter/btor2/native_bmc.rs) and [`BmcError::ResourceLimit`](../../crates/mununu-core/src/adapter/btor2/native_bmc.rs); [`ReachOutcome::abstained`](../../crates/mununu-core/src/adapter/reach_portfolio.rs); the `abstained` field of [`PortfolioAttributionView`](../../crates/mununu-core/src/api/models.rs) — surface: CLI+API (`sv verify-auto`, `btor2 verify*`; `--json` and `POST /api/v1/sv/verify-auto`).

> **Audience:** rosf and monono (gate on verdicts; parse the report), anyone comparing a verdict across machines or runs, the second e2e host.
>
> **TL;DR — a verdict-semantics fix: a row can no longer flip between ⊥ and `holds` across runs because of the native k-induction member's 5 s wall clock.** That per-query budget is now a deterministic z3 resource limit (the same stop on every host, under any load). A stop on it is **named**: the row's `portfolio.abstained` lists `native` with the units and the depth, and the note is `portfolio-budget-stop` (new kind). Nothing a sound run decided before changes; a row that used to be ⊥ *because the clock landed on the wrong side* now either decides or abstains the same way every time. One additive field (`portfolio.abstained`), one new note kind, two new environment variables. Schema regenerated.

## What changed

| item | before | after |
|---|---|---|
| native BMC / k-induction per-check budget | z3 `timeout` = 5 s wall (the raised `--timeout-ms` under #892) | z3 `rlimit` = nominal ms × 3 000 units (5 s nominal = 15 M units; the raised `--timeout-ms` scales it the same way). Calibrated on the development laptop: 2.3–3.9 M units/s on a hard query, so about 5 s there, 2–3× that on a slower host — the **same** stop. |
| a check that hits the budget | `Unknown`, silently merged as ⊥ | `Err(ResourceLimit { k, units })` inside the member → `portfolio.abstained: [{member: "native", reason: "native BMC: the deterministic query budget (… units, about … ms nominal) ran out at depth k; …"}]` |
| the run budget's wall (`MUNUNU_PROPERTY_BUDGET_MS`, `MUNUNU_VERIFY_BUDGET_MS`) | capped every check | unchanged — and a stop on it is named as the wall (`budget-expired`) |
| note on a row nothing decided | `portfolio-refused` whenever no member decided, advice "more time will not help" | `portfolio-refused` when a member **refused**; **`portfolio-budget-stop`** when none refused but one stopped on its budget (advice: a larger `--timeout-ms` scales the budget) |
| `properties[].portfolio` | `{decided_by, refused}` | `{decided_by, refused, abstained}` — `abstained` is `[{member, reason}]`, empty when none stopped |
| env | — | `MUNUNU_NATIVE_QUERY_RLIMIT` (units; `0` = none), `MUNUNU_NATIVE_QUERY_TIMEOUT_MS` (opt-in wall, off by default) |

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** | A gate that saw an occasional ⊥ ↔ `holds` flip on the same row across runs loses that flake; verdicts on rows the native member decides are now reproducible across your runners. One additive key under `portfolio`; one new note kind. A row whose `portfolio.abstained` names `native` is a candidate for a larger `--timeout-ms`, unlike a `refused` row. | None required. If the report model is strict, add `portfolio.abstained` (array, default empty) and accept the note kind `portfolio-budget-stop`. |
| **monono** | Same, through rosf. Re-recorded baselines may show formerly-⊥ rows deciding (or the reverse) — once, deterministically. | Re-run the baseline once on the pinned engine and keep the new one; a row that still differs between two runs of the SAME build is a bug to file. |
| **mununu-ui** | `portfolio.abstained` can be shown beside `refused`, with the opposite advice. | Optional. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images / the on-prem bundle | Yes — this is a verdict-semantics fix. | The member's budget semantics changed. |
| rosf `docker/Dockerfile*` | Only to pick up the new mununu. | Consumer. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Same. | Same. |

## Provenance

mununu#893, found in the N9 owned-only HWMCC'20 array sweep on the second host (`zipcpu_dcache-p588`: ⊥ on one binary, `holds` on another, nothing in the diff touching the queries). Falsifier-first: a probe (`probe_rlimit_semantics_and_calibration`, `#[ignore]`) established that z3's solver-level `rlimit` is counted per check (0 of 300 trivial checks hit a 500-unit limit), that a wall stop reads `"timeout"` and an rlimit stop `"canceled"`, and the units/second on the development host. Reproducer `crates/mununu-core/tests/native_query_budget_893.rs` (parent-compatible API; `make verify-repro` against `main`). Unit tests pin: the stop is named and identical twice; the opt-in wall stop is named as the wall; the limit is per check (an 8-bit counter runs all 82 checks under a per-check limit); the reason classification; the nominal → units conversion and both environment overrides; the abstention reaches `abstained` not `refused`, in fixed engine order; the attribution's bottom reason and note kind; the view.

## Not covered here

The owned deep-counterexample search (`bmc_cex_until`) also runs under the resource limit, but its budget miss is still a plain `None` (an honest miss by design); the interpolation member's own per-query budget (`INTERP_QUERY_TIMEOUT_MS`, cvc5 subprocess) is unchanged and remains a wall clock — filed separately as the asymmetry #893 names. A recalibration of `UNITS_PER_NOMINAL_MS` on the second host is a measurement, not a behaviour change.
