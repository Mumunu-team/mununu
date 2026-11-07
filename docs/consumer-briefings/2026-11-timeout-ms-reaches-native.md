# Consumer briefing: a raised `--timeout-ms` now reaches the owned members of the full portfolio (2026-11)

> Source of truth: [`portfolio_member_timeouts`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API — `btor2 verify --timeout-ms`, `sv verify-bad`, and every `sv verify-auto` rescue that runs the reach portfolio.

**Audience:** rosf, monono, mununu-ui, and anyone who raises `--timeout-ms` to reach a deeper verdict.

## TL;DR

- **At the default wall (60 s) nothing changes.** Same budgets, same verdicts, same times.
- **A `--timeout-ms` above 60 s now also sets the owned members' budgets** in the full portfolio: native BMC / k-induction's per-query bound and the interpolation member's bounds follow the flag, the way they already did under `--owned-only`. Before, the raised wall reached btormc / Pono / rIC3 and btormc's depth cap only, and native kept its 5 s per-query default whatever the flag said.
- **What you may see:** on a design with memories, a row that was `unknown` with a large `--timeout-ms` can now come back decided by `native`. Two HWMCC'20 array rows measured on 2026-11-07 were `unknown` in a full run with `--timeout-ms 600000` (native timed out at 5 s per query; btormc and rIC3 ran 600 s without a verdict) and `holds` by native k-induction with a 60 s query budget (4 s and 38 s). A run with a raised wall may also take longer on rows nobody decides, since native now uses the time it was given.
- **No shape, flag or route changes.** `decided_by` / `portfolio.decided_by` may name `native` where it did not before.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **monono** | Gate runs that pass `--timeout-ms` above 60 s may decide more rows (by `native`) and may run longer on undecided rows. Default runs are unchanged. | None required. If a ledger pins `decided_by`, expect `native` to appear on rows a raised-budget run newly decides. |
| **rosf** | Same, through `btor2 verify` / `sv verify-bad`. | None required. |
| **mununu-ui** | No UI affordance; the `timeout_ms` request field's effect widens as above. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | Yes, if consumers pin a version tag and raise `--timeout-ms`. | The behaviour comes from the binary. |
| `mununu/docker/Dockerfile.dev` | Only if the dev workflow raises the budget. | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | Yes, if you run gates with a raised `--timeout-ms` in the image. | The binary on the volume changes; the toolchain does not. |
| `mununu/docker/Dockerfile.profile` | No. | Profiling image. |
| `mununu/docker/Dockerfile.extract*` | No. | Extraction images do not verify. |
| rosf `docker/Dockerfile*` | Yes, if rosf pins a mununu version and raises the budget. | Picks up the change. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Yes, if monono pins a mununu version and raises the budget. | Picks up the change. |

## Provenance

- **mununu#892**, found during the N9 owned-only sweep of the HWMCC'20 array track: `zipcpu_zipcpu_dcache-p236` and `-p428`.
- **Tests:** `tests/native_budget_892.rs` (a raised wall becomes native's query budget; the default wall leaves every budget as before; a run budget in effect still wins).

## Not covered here

- **mununu#893**: native's per-query budget is a wall clock, so a ⊥ at the budget's edge can differ between runs and hosts. Open; a resource-count budget (z3 `rlimit`) is the suggested fix.
- `--owned-only` is unchanged: it already gave every owned member the flag's value.
