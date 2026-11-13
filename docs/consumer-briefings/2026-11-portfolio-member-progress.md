# Consumer briefing: why each reach-portfolio member gave no verdict (2026-11)

> Source of truth: [`ReachOutcome::undecided`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API

**Audience:** anyone who reads the member lists of `btor2 verify` (CLI `--json`, `POST /api/v1/btor2/verify`) or the `portfolio` block of `sv verify-auto` rows.

## TL;DR

- **The defect (mununu#1022).** Before this change a reach-portfolio `unknown` could arrive with no reason for any member:
  - **`btor2 verify`** printed `refused` but never `abstained`, so a budget stop was invisible there.
  - **`--owned-only`** said nothing about members still running when its wall deadline came: an `unknown` at 300 s had `refused: []` and nothing else.
  - **A member that searched to its own bound** dropped how far it got. Native BMC / k-induction reports the depth and k reached; interpolation reports its reason.
- **Now:**
  - **`btor2 verify`** gains two optional lists, both shaped `[{member, reason}]` like `refused` and both omitted when empty:
    - **`abstained`:** budget stops. Under `--owned-only` this includes `"still running at the owned-only wall deadline (N ms); no result was collected"`.
    - **`undecided`:** members that searched to their bound, each with its progress, e.g. `"BMC found no counterexample through depth 40 and k-induction found no proof up to k = 40 (bound 40)"`.
  - **`sv verify-auto`** rows gain `portfolio.undecided` beside `portfolio.abstained`. A ⊥ row's `bottom_reason.detail` and its portfolio note's `items` name the same progress.
  - **The liveness notes** (justice / l2s, response k-liveness) list undecided members too.
- **A member cancelled because another one decided first is in no list,** as before.
- **Verdicts do not change.** `outcome`, `reachable_by`, `unreachable_by` and traces are unchanged. Every addition is optional.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `btor2 verify` JSON may carry `abstained` and `undecided`; verify-auto rows may carry `portfolio.undecided`. | None required. To explain an `unknown`, read the three lists together: `refused` → route around the member; `abstained` → a larger budget may decide; `undecided` → a deeper bound or another engine. |
| **monono** | Same. | Same. |
| **mununu-ui** | The verify-auto view renders `detail` as text, which may now be longer. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Origin:** mununu#1022, measured on HWMCC'20 `krebs.3.prop1-func-interl` (`btor2 verify --owned-only --timeout-ms 300000`).
- **Tests:**
  - `a_native_member_that_searched_to_its_bound_reports_its_progress` and `the_portfolio_carries_an_undecided_members_progress` (member → outcome);
  - `the_owned_driver_names_the_members_still_running_at_its_deadline` (owned-only deadline);
  - `a_portfolio_bottom_names_how_far_each_undecided_member_got` (verify-auto ⊥);
  - the extended `a_portfolio_budget_stop_reaches_the_row_as_abstained_not_refused` (wire);
  - the extended `the_l2s_abstention_note_names_the_default_cap_and_every_member_reason` (justice note).
- **Sibling sites swept:** both portfolio drivers; the btor2 verify response on CLI and API; the verify-auto attribution and its ⊥ detail; the justice / l2s note; the response k-liveness notes; the liveness-rescue member notes.

## Not covered here

- The subprocess members (btormc, Pono, rIC3) report no progress of their own; a clean `unknown` from them stays in no list.
- The owned-only deep counterexample search (`cex` / `boolector`) reports no depth when it ends without a hit inside its deadline.
