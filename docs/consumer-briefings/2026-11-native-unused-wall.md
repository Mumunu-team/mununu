# Consumer briefing: native's "undecided" reason says how much of the wall it left (2026-11)

> Source of truth: [`native_unknown`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API — `btor2 verify` (full portfolio and `--owned-only`) and every verb that runs the reach portfolio

**Audience:** anyone who reads the reach portfolio's `undecided` list.

## TL;DR

- **The native BMC / k-induction member's `undecided` reason gains a suffix (mununu#1038).** It now reads `…(bound 40); it stopped at its depth bound with <s> s of the wall left`. Before, nothing showed that the member stopped at its fixed depth of 40 with most of `--timeout-ms` unused.
- **The depth bound stays 40.** Raising it was measured first and did not help. Native k-induction ran with no depth cap on the two HWMCC'20 k-liveness monitors in the issue: plain k-induction reached depth 68–78 in 60 min, and with simple-path constraints it reached 37–65 in 30 min. Neither proved. Each step had grown to minutes. The owned IC3 member (#1042, opt-in) proves one of them in under a second.
- **No verdict changes.** Only the reason text changes. The list's shape is unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the `native` entry in `undecided` has a longer reason | If you match the reason text exactly, accept the suffix. |
| **monono** | same | same |
| **mununu-ui** | no UI surface | none |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Only to get the new text. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image. | They consume the binary. |

## Provenance

- **Found by:** the C4 owned k-liveness run on the box (`btor2 verify --owned-only --timeout-ms 3600000`): `lmcs06srg5p0` and `cunim1ro` came back `unknown` at 158 s and 287 s of a 3,600 s budget.
- **Test:** `the_portfolio_carries_an_undecided_members_progress` (a 60 s wall; native stops at its bound in milliseconds).

## Not covered here

- A depth bound that scales with the budget: not shipped, because it was measured not to decide anything (above).
- The owned IC3 member (#1042) and the budget-stop silence in #1043.
