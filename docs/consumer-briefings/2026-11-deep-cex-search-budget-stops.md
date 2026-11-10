# Consumer briefing: the owned-only deep counterexample search stops at its deadline and names the stop (2026-11)

> Source of truth: [`bmc_cex_until`](../../crates/mununu-core/src/adapter/btor2/native_bmc.rs#L1039) — surface: CLI+API — `btor2 verify --owned-only` (and the API's owned-only reach path) without the `boolector` feature.

**Audience:** rosf, monono, mununu-ui, and anyone who runs `btor2 verify --owned-only`.

## TL;DR

- **Verdicts do not change.** The deep counterexample member (`cex`) still only ever contributes `violated`, from a concrete z3 model.
- **It no longer runs past its deadline.** Since mununu#893 each of its z3 checks was bounded only by the deterministic resource limit (15 000 ms nominal), so one check could run well past the portfolio's wall. Every check is now also capped by the time left to the deadline, and a peer's decision interrupts the check in flight, as it already did for native k-induction.
- **Its budget stops are now reported.** A deadline stop or a resource-limit stop lands in `portfolio.abstained` as `("cex", <cause>)`, worded like native's (`native BMC: the wall-clock budget expired at depth …` / `… the deterministic query budget (… z3 resource units …) ran out at depth …`). Before, both read as a silent miss.
- **No shape, flag or route changes.** `abstained` may carry a `cex` entry where it carried none.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **monono** | `--owned-only` rows that stop on a budget may list `cex` in `abstained`; an owned-only row's wall stays closer to `--timeout-ms`. | None required. A ledger that counts `abstained` entries will see more on undecided rows. |
| **rosf** | Same, through `btor2 verify --owned-only`. | None required. |
| **mununu-ui** | No UI affordance. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | Yes, if consumers pin a version tag and use `--owned-only`. | The behaviour comes from the binary. |
| `mununu/docker/Dockerfile.dev` | Only if the dev workflow runs `--owned-only`. | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | Yes, if you run owned-only gates in the image. | The binary on the volume changes; the toolchain does not. |
| `mununu/docker/Dockerfile.profile` | No. | Profiling image. |
| `mununu/docker/Dockerfile.extract*` | No. | Extraction images do not verify. |
| rosf `docker/Dockerfile*` | Yes, if rosf pins a mununu version and uses `--owned-only`. | Picks up the change. |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Yes, if monono pins a mununu version and uses `--owned-only`. | Picks up the change. |

## Provenance

- **mununu#955**, found by the tier-B review of 2026-11-09.
- **Tests:** in `adapter/btor2/native_bmc.rs`: `the_deep_cex_search_stops_at_its_deadline_and_names_the_stop`, `the_deep_cex_search_names_a_resource_limit_stop`, `the_deep_cex_search_honours_a_cancel_inside_a_check`.

## Not covered here

- Builds with the `boolector` feature, where the in-process Boolector BMC is the `cex` member instead; its stops are unchanged.
- The full (non-owned-only) portfolio, which does not run this member.
