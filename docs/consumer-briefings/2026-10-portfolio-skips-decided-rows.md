# Consumer briefing — a later portfolio member no longer re-runs a row an earlier one decided (2026-10)

> Source of truth: [`decided_rows`](../../crates/mununu-core/src/planner/mod.rs) + [`VerifyAutoOptions::skip_rows`](../../crates/mununu-core/src/adapter/slang/verify_auto.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto` under the default sequential portfolio

**Audience:** monono, rosf and anyone timing `sv verify-auto` runs or reading the `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` breadcrumb. Plan 4, PR D3 (mununu#733); follows #732 and #735.

## TL;DR

- **No verdict changes; shorter runs.** The sequential portfolio merges after each member and keeps the first definite verdict, so a later member's work on a decided row could never change it. Later members now record such rows as `unknown` with `bottom_reason: not-attempted` and spend nothing on them. Measured: FILL.
- **The breadcrumb ends with the verdict that ships.** `MUNUNU_VERIFY_AUTO_PARTIAL_JSON` now gets one `phase: "merged"` record per property after the merge and the ⊥ re-plan. A member's own records (`main`, `escalated`) precede it, and a later member writes `unknown` for every row it left to an earlier one — so "take the last record per property" is right again only because the `merged` record is last. Readers that stop at the last `main` record will read `unknown` for decided rows.
- **A new Info note kind `portfolio-skip`** counts the rows each later member left alone. The `portfolio` note's detail no longer says "a ⊥ means every engine left it undecided".
- **Trade-off, stated:** the merge's runtime contradiction alarm (`portfolio-soundness-alarm`) cannot fire in sequential mode on a row a member skipped, because the member did not run it. The parallel mode (`--portfolio parallel`) and the census remain the differential check.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **monono** | lane walls drop where a design's rows are decided by the first member; `[portfolio-skip]` lines appear in the transcript | none |
| **rosf** | `properties[].bottom_reason` of a merged report never carries `not-attempted` (the merge keeps the earlier definite); the breadcrumb gains `merged` records | none |
| **mununu-ui** | a new Info note kind | optional |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` | yes | planner lives in the binary |
| `mununu-sva-pono` | yes | same |
| `mununu-ui` | no | JSON unchanged |

## Measured

FILL (census `keymgr_ctrl` wall; `e2e_opentitan_ip_edn_full` wall; verdict identity of the census and the full-IP e2e against #735).

## Not covered here

- Which engine decides the nine duplicated-counter fault-flag chains (Plan 4 step 0 / PR D2).
