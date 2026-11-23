# Consumer briefing: a stop at a time limit names the setting that set the limit, or says there is none (2026-11)

> Source of truth: [`SetBy`](../../crates/mununu-core/src/adapter/run_budget.rs#L56) — surface: CLI+API — the member reasons in `abstained` / `undecided` of `btor2 verify` / `POST /api/v1/btor2/verify`, the `portfolio` block and the notes of `sv verify-auto` rows

**Audience:** anyone who reads or matches the `reason` text of a reach-portfolio member, the l2s note of a justice verdict, the `detail` of the `time-budget-exceeded` / `memory-budget-exceeded` escalation notes, or the reason text of a `budget-expired` row.

## TL;DR

- **Text only. No verdict, field, flag or route changes.** Which member lands in which list, and when, is as before.
- **A report of a stop at a time limit ends with one closing clause (mununu#1197).** The clause names the setting that set that limit on this run, in the names of the surface that ran it, or says that no setting does. Before, three reports said "the run budget", which is not the name of anything a user sets, and two named `--timeout-ms` on verbs that have no such flag.

  | who set the limit | the closing clause |
  |---|---|
  | `--timeout-ms` of the CLI verb | ``this limit follows `--timeout-ms`: raise it to give the search longer`` |
  | `timeout_ms` of the HTTP request | ``this limit follows the request's `timeout_ms`: raise it to give the search longer`` |
  | one of `sv verify-auto`'s budgets | the same sentence with `MUNUNU_VERIFY_BUDGET_MS`, `MUNUNU_PROPERTY_BUDGET_MS` or `MUNUNU_RESCUE_SLICE_MS`, whichever set the deadline that was reached |
  | a constant of mununu's own | `this limit is mununu's own and no setting changes it (a mununu limitation: nothing to change in the design)` |

- **Two more forms, for a member that stopped on its default limit.** The interpolation member's two limits are default constants unless a larger limit replaces them:

  | the run | the closing clause |
  |---|---|
  | `btor2 verify` (CLI or API) with a timeout at or under 60000 ms | ``this limit is the member's default: it follows `--timeout-ms` once that is above 60000 ms`` (the API names the request's `timeout_ms`) |
  | `sv verify-auto` with its budgets disabled (each set to `0`; they are on by default) | ``this limit is the member's default: a limit given with `MUNUNU_RESCUE_SLICE_MS` replaces it``, or with `MUNUNU_PROPERTY_BUDGET_MS` on a step the slice does not cover |

  Under `--owned-only` the member's per-query limit is a constant, and a stop on it says the limit is mununu's own.

- **The reports that changed:**

  | report | before | now |
  |---|---|---|
  | a member the wall or the run budget stopped (`abstained`) | `<progress>; stopped by the portfolio's wall or run budget before a verdict` | `<progress>; stopped by the portfolio's wall before a verdict; <clause>`, or `… by the run budget …`: the one that stopped it |
  | a member with no report when the wall passed | ``stopped at the portfolio's wall deadline (<n> ms: `--timeout-ms` on the `btor2` verbs, the run budget on `sv verify-auto`) before it reached a verdict`` | `stopped at the portfolio's wall deadline (<n> ms) before it reached a verdict; <clause>` |
  | a member with no report when the run budget expired | `stopped when the run budget expired, before it reached a verdict` | the same, then `; <clause>` |
  | the exact member abandoned inside one BDD operation | `abandoned at the run budget's deadline: one BDD operation was still running (…)` | the same, then `; <clause>` |
  | a member still running at the `--owned-only` deadline | `still running at the owned-only wall deadline (<n> ms); no result was collected` | the same, then `; <clause>` |
  | the interpolation member at its per-query limit or its overall deadline (`undecided`) | ``<fact>; the member's budget follows the run budget (`--timeout-ms` on the `btor2` verbs, the run budget on `sv verify-auto`)`` | `<fact>; <clause>`, the clause of the limit it hit |
  | the same member when z3 gave no answer while confirming a counterexample | `solver timeout confirming reachability` | `z3 gave no answer within its <n> ms query limit while confirming reachability; <clause>` |
  | the bit-level members' model build stopped by the run budget | `…(<n> nodes so far); more time on the run's budget, or a smaller design` | `…(<n> nodes so far); <clause>` |
  | the same build stopped at a member's own deadline | `…(<n> nodes so far); more time on the verb's timeout, or a smaller design` | `…(<n> nodes so far)`: the fact alone (see "Not covered here") |
  | the predicate-cube engine's cycle-0 doubt on a spent budget | `a re-run with a larger run budget may decide it` | ``the budget follows `<setting>`: a re-run with a larger one may decide it``, or the mununu's-own clause |
  | `detail` of the escalation note `time-budget-exceeded` | `… what the engines can decide. Raise or disable the budget (`0`) to attempt them.` | ``… what the engines can decide; the budget follows `<setting>`: raise it to attempt them.`` |
  | `detail` of the escalation note `memory-budget-exceeded` | the same sentence | ``…; the ceiling follows `MUNUNU_MAX_PROCESS_MEMORY_BYTES`: raise it, or set it to `0` for no ceiling, to attempt them.`` |
  | reason text of a `budget-expired` row | names `MUNUNU_PROPERTY_BUDGET_MS` and `MUNUNU_RESCUE_SLICE_MS` | also names `MUNUNU_VERIFY_BUDGET_MS`, for the whole run |
  | the l2s note of `btor2 verify-justice` / `btor2 verify-liveness --justice` (CLI and API) when no timeout was given | ``… undecided at the members' default budgets (60 s per subprocess member; `--timeout-ms` raises them)`` | ``… undecided at the members' default budgets (60 s per subprocess member); this limit is the member's default: a limit given with `--timeout-ms` replaces it`` (the API names the request's `timeout_ms`) |

- **Match on the list and the member, not on the clause.** The clause differs by verb, by surface and by which budget was in effect; that is the point of the change. A consumer that needs to know a stop was a budget stop reads which list the member is in (`abstained` after a clock stop, `undecided` for the interpolation member's own limits), the row's `bottom_reason` (`budget-expired`) or the note's `kind`.

## Per consumer

| consumer | what to do |
|---|---|
| rosf | Nothing unless a script matches one of the "before" texts above; then match the list, `bottom_reason` or note `kind` instead. A CI gate that reports a budget stop can now quote the clause as the thing to change. |
| monono | The same. The texts of mununu#1186's briefing (`2026-11-solver-query-limit-one-wording.md`) that end in "the member's budget follows the run budget (…)" no longer occur: the fact before the semicolon is unchanged, the clause after it is the one above. |
| mununu-ui | No UI affordance; the reason is rendered as text. |

Earlier briefings quote wordings this one replaces: `2026-11-member-clock-stop-progress.md` and `2026-11-portfolio-wall-deadline.md` (the wall's and the budget's texts), `2026-11-solver-query-limit-one-wording.md` (the interpolation member's clause), `2026-11-aig-build-stop.md` (the model build's two endings), `2026-11-portfolio-member-progress.md` (the `--owned-only` line). They are kept as written; this briefing is the current text.

## Docker rebuild

| image | rebuild? |
|---|---|
| production `mununu/docker/Dockerfile`, the on-prem bundle `docker/Dockerfile.bundle`, `mununu/docker/Dockerfile.sva` | Yes, to get the new texts: they ship the binary |
| `mununu/docker/Dockerfile.dev`, `Dockerfile.profile`, `Dockerfile.extract*` | No |
| rosf `docker/Dockerfile*`, monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | Only those that pin a `mununu` image |

## Provenance

- **Found by:** mununu#1197 (the remedy audit of mununu#1121: three budget-stop remedies on `sv verify-auto` named "the run budget"), and the sweep for other budget remedies with no setting that the issue asked for.
- **Tests:** over every setter: `x1197_every_setter_names_its_own_setting_or_says_there_is_none`, `x1197_a_child_reports_the_setter_of_the_deadline_that_binds`, `x1197_an_unbounded_budget_keeps_the_setting_that_would_bound_it`, `x1197_a_limit_origin_names_its_setter_or_says_there_is_none`. What each verb's run reads: `x1197_the_full_portfolio_reads_each_limit_from_the_budget_and_the_wall`, `x1197_the_owned_only_driver_names_the_setter_of_the_wall_that_binds`. The seam between the interpolation search and the member lists: `x1197_the_search_deadline_is_a_typed_limit_stop_that_names_no_setting`, `x1197_an_interp_limit_stop_closes_with_the_origin_of_the_limit_it_hit`. The justice ladder's l2s note, over every setter: `the_l2s_abstention_note_names_the_default_cap_and_every_member_reason`. `policy::timeout_setter_named_on_its_surface_1197` keeps the flag's name in the CLI and the request field's name in the API handlers, and holds the core lines that spell the flag to an exact table.
- **Class:** `budget-remedy-knob` in [`docs/defect-classes.md`](../defect-classes.md).

## Not covered here

- **Seven reports of a time stop still state the fact and name no setting** (mununu#1243): a subprocess member with no verdict within its budget, native BMC's wall-clock stop, the SAT BMC and owned IC3 members at their own deadline (the model build at a member's deadline among them), the witness search that did not run, the justice ladder's rung notes under a timeout (and the k-liveness rung's note without one), and `btor2 verify --bmc-only`. None of them names a setting that does not exist; they name none.
- **Native BMC's stop at its deterministic query budget still names `--timeout-ms` on the API** (mununu#1243). Its text offers `MUNUNU_NATIVE_QUERY_RLIMIT` on any verb and the flag "on the `btor2` verbs that take it"; on the HTTP routes that setting is the request's `timeout_ms`. Unchanged here.
- **No limit changed.** Every default, every derivation of a member's limit from the wall or the budget, and the 60000 ms threshold are as before; only what the report says about them changed.
- **The predicate-cube lift's, the symbolic refinement's and the CEGAR loop's budget errors** already named `MUNUNU_PROPERTY_BUDGET_MS` and `MUNUNU_VERIFY_BUDGET_MS` and are unchanged.
