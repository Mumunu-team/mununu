# Consumer briefing — a ⊥ the reach portfolio could not even start on names its cause (2026-10)

> Source of truth: [`attribute_portfolio_abstention`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) + [`ReachOutcome::refused`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) + [`RESCUE_MONITOR_DUMP_ENV`](../../crates/mununu-core/src/adapter/reach_rescue.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** rosf, monono and anyone reading `bottom_reason` or the verification notes of an `sv verify-auto` run. Plan 4, PR D1 (mununu#734); follows #732.

## TL;DR

- **An instant "unknown" from the ⊥ re-plan now says why.** When the reach portfolio decides nothing on a row and its budget is intact, the row's `bottom_reason` becomes `engine-did-not-complete` with `engine: "reach-portfolio"` and the members' own words: `refused by native: … ConstantOutOfRange; pono: binary not found` (an encode error, an absent binary, a child that exited without a verdict), or `every portfolio member returned unknown within its budget` when nobody refused. Before, such rows read `unclassified` or `safety-shape-not-reducible` — on OpenTitan csrng the cause (a 64-bit constant limit) took a debug run and a probe on a dumped model to find.
- **Two sound members disagreeing is an alarm, not an abstention.** A portfolio `Contradiction` on the rescue's monitor used to be folded into "inconclusive"; it is now `bottom_reason: engine-contradiction` with a `portfolio-rescue-contradiction` SoundnessCaveat note, like the merge's own alarm.
- **Two new note kinds** (`portfolio-refused`, ScopeCaveat, `items` = one `member: reason` per refusal; `portfolio-rescue-contradiction`, SoundnessCaveat). No JSON shape change; no verdict changes.
- **`MUNUNU_RESCUE_MONITOR_DUMP=<dir>`** writes the `bad`-monitor BTOR2 the re-plan hands the portfolio, one file per rescued row, so an external engine can be probed on exactly what the portfolio saw.
- A row whose rescue slice or property budget ran out still reads `budget-expired` — unchanged.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `unexplained_abstentions()` counts ⊥ rows without a `bottom_reason`; rows that were unexplained because the portfolio refused the design now carry `engine-did-not-complete` — the count can only drop | none required; re-capture when adopting |
| **monono** | the transcript shows `⚠ [portfolio-refused]` / `⚠ [portfolio-rescue-contradiction]` lines where a rescued row stays ⊥ for these reasons; the lane does not act on notes | none |
| **mununu-ui** | two new note kinds to render like `portfolio-rescue` | optional |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` | yes | attribution lives in the binary |
| `mununu-sva-pono` | yes | same |
| `mununu-ui` | no | JSON unchanged |

## Measured

Unit level: a design with an array sort makes every in-process member refuse (`ArraySortUnsupportedInBvOnly`) and the subprocess members are absent on the host — the row's reason names `native` and the array sort (`escalate_bottom_attributes_an_instant_inconclusive_to_the_refusing_members`). Image gates recorded on the PR: OpenTitan `edn` / `csrng` verdicts identical to #732 (the nine fault-flag rows keep `budget-expired`; no other ⊥ row exists on those IPs), census identical.

## Not covered here

- Which engines decide the nine duplicated-counter fault-flag chains — PR D2 (measured first with `MUNUNU_RESCUE_MONITOR_DUMP`).
- Members that run rows an earlier portfolio member already decided (mununu#733) — PR D3.
