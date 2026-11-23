# Consumer briefing: one wording for a solver query stopped at its limit, and a cancelled member names no cause (2026-11)

> Source of truth: [`query_limit_fact`](../../crates/mununu-core/src/adapter/btor2/native_interp.rs#L955) — surface: CLI+API — the `interp` member's reason in the member lists of `btor2 verify` / `POST /api/v1/btor2/verify` and in the `portfolio` block of `sv verify-auto` rows

**Audience:** anyone who reads or matches the `reason` text of the `interp` member in `abstained` / `undecided` (`btor2 verify`) or `portfolio.abstained` / `portfolio.undecided` (`sv verify-auto`).

## TL;DR

- **Text only. No verdict, field, flag or route changes.** The lists, their shape and when a member is in which list are as before.
- **A solver query stopped at its limit reads the same whichever mechanism stopped it (mununu#1186).** The interpolation member gives each solver query a limit. cvc5 normally stops by itself at that limit; if it does not, mununu kills it 2 s later. The two stops had two texts, and only the first said what the stop was and what to change:

  | which stop | before | now |
  |---|---|---|
  | cvc5 exits by itself at the limit | `cvc5 stopped at its <n> ms query limit (exit 134) — a budget stop, not an interpolation gap; the member's budget follows the run budget (…)` | unchanged |
  | mununu kills the solver past the limit | `cvc5 exceeded <n>ms` | `cvc5 stopped at its <n> ms query limit (killed 2000 ms past it: it did not stop by itself) — a budget stop, not an interpolation gap; the member's budget follows the run budget (…)` |

  Which of the two fires depends on host load, so the same run could give either text. In the member lists both arrive after the prefix `interpolation unavailable: `, as before.
- **The interpolation member's own deadline names the same remedy.** `overall timeout (<n>ms)` and `overall timeout (<n>ms) at suffix depth <k>` are now followed by `; the member's budget follows the run budget (…)`, the clause the query-limit stop ends with. The start of the text is unchanged.
- **A member stopped by its caller says `cancelled by the caller` and no more.** Before, it said `cancelled — another portfolio member decided first`, also when the portfolio's wall or the run budget had stopped it, so an `abstained` entry could read `cancelled — another portfolio member decided first; stopped by the portfolio's wall or run budget before a verdict`. Now that entry reads `cancelled by the caller; stopped by the portfolio's wall or run budget before a verdict`. The member cannot know why it was stopped; the portfolio does, and its half of the sentence is unchanged.
- **A request that is gone stops every solver query.** When an HTTP request times out or its client disconnects, a cvc5 or MathSAT query still running for it is killed by the same check that kills yosys, btormc, Pono and rIC3 (mununu#1147). Inside the reach portfolio the shared stop already reached cvc5; a refinement query outside it (MathSAT, or cvc5 run without the portfolio) had no such stop and ran to its limit.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | The `interp` member's `reason` text, in the cases above. | If you match `cvc5 exceeded` or `another portfolio member decided first` in a reason, match `query limit` and `cancelled by the caller`. Otherwise none. |
| **monono** | Same. | Same. |
| **mununu-ui** | No UI affordance; the reason is rendered as text. | None. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production), the on-prem bundle (`docker/Dockerfile.bundle`), `mununu/docker/Dockerfile.sva` | Yes, to get the new texts. | They ship the `mununu` binary. |
| `Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No. | They build from source per use, or do not verify. |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image: bump the pin. | They consume the binary. |

## Provenance

- **Found by:** a test of the first wording that failed once in a full-suite run on a loaded host with `cvc5 exceeded 1ms`, and passed five times out of five alone (mununu#1186). The fix of mununu#1129 had reworded the solver's own exit and left the kill as it was.
- **Tests:** `x1186_a_cvc5_killed_by_the_backstop_is_the_same_budget_stop` (the reproducer: a stand-in solver that never answers), `x1186_every_way_a_solver_child_stops_is_named` (a reply, its own exit at the limit, the kill past the limit, the caller's flag, a cancelled run, a failure exit, a signal), `x1186_a_query_limit_stop_reads_the_same_whichever_mechanism_fired`, `x1186_a_cancelled_search_does_not_say_why_the_flag_went_up`, `x1186_the_search_deadline_names_the_run_budget_remedy`.
- **Class:** `member-stop-reason` in [`docs/defect-classes.md`](../defect-classes.md).

## Not covered here

- The other subprocess members (btormc, Pono, rIC3) return no text of their own when they are stopped; the portfolio words their entry, and that is unchanged.
- The refinement queries outside the reach portfolio (the experimental `btor2 verify --engine ic3`, the must-precondition step of the predicate-abstraction refinement) run on the same solver runner, but their callers do not report why a query gave no predicate (mununu#1188, open).
- How long a query's limit is, and what sets it: unchanged.
