# Consumer briefing — `e throughout b[->1]` translates; a plain `EF p` left ⊥ is decided on its dual (2026-10)

> Source of truth: [`is_goto_once`](../../crates/mununu-core/src/adapter/slang/translate.rs) + [`reduce_ef_target`](../../crates/mununu-core/src/adapter/reach_rescue.rs) — surface: (CLI+API) — `sv verify-auto` / `POST /api/v1/sv/verify-auto`

**Audience:** rosf, monono, mununu-ui and anyone whose gate pins `unsupported` counts or `unknown` rows of `sv verify-auto`. Plan 2, PR B (follows PR #729).

## TL;DR

- **One more SVA shape translates.** `e throughout b[->1]` — a boolean held until and including the first `b` (OpenTitan's `EdnDataStable_A`: `$stable(bus) throughout (req || !enable)[->1]`) — lowers to the weak-until `nu Z. (e && (b || [] Z))`: `e` at every cycle up to the first `b`, `e` forever if `b` never comes (the LRM's weak reading of an unmarked consequent sequence; not vacuous — `e` constrains every cycle). Rows that were `unsupported` with the reason "`GoTo` repetition … Tier-3b" now get a verdict (or an honest ⊥). A bare `b[->1]` consequent, `[->n]` with n ≥ 2 and `[=n]` stay refused.
- **A plain `EF p` the cube left ⊥ now decides.** The ⊥ re-plan runs it as its dual `AG !p` through the reach portfolio: `bad := p` reachable ⇒ `EF p` **holds**; proved unreachable ⇒ `EF p` **violated**, with the target atoms in `counterexample.unreachable_target` (the existing field). Gated by the same `rescue_bottom_safety` option; `decided_by: "reach-rescue"`; a new provenance note kind `reachability-rescue` (and `reachability-rescue-declined` when the dual's monitor cannot be emitted).
- No JSON shape change. No verdict flips: a row moves only from `unsupported` → decided/⊥ or from `unknown` → decided.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | `--expect-count` pins move where a `throughout b[->1]` row joins the property list; `EF` twins written as `@mununu_guarantee` may leave `unknown` | rerun; re-pin counts |
| **monono** | same; an `unsupported` row with the `GoTo` reason on a goto-once-under-throughout shape disappears | rerun the gate |
| **mununu-ui** | two new note kinds (`reachability-rescue`, `reachability-rescue-declined`) render like `portfolio-rescue` | none required |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` | yes | translator + re-plan live in the binary |
| `mununu-sva-pono` | yes | same |
| `mununu-ui` | no | JSON unchanged |

## Measured

OpenTitan `edn` (`examples/verify/ip_opentitan_edn`, shipped defaults): 67 properties, **35 holds / 8 violated / 24 ⊥ / 0 unsupported** (before: 58 translated + 8 unsupported, 31 holds). `EdnDataStable_A` ×8 leave `unsupported` and land ⊥ (a 32-bit bus-stability cone — the honest answer); the four `EF` twins HOLD on their dual (SymbiYosys `cover` reaches each within six cycles); the e2e takes 1 342 s (937 s before: the eight new rows each spend a rescue slice per portfolio member). Census: `e2e_opentitan_sva_census` PASS in 1 870 s; the unsupported set is unchanged (`AssertionInstance` ×4, the comparison form ×2, a `Binary` antecedent ×2 — the census corpus has no goto rows), no verdict flips; two panics caught on a portfolio member thread during `prim_esc_receiver` (`symbolic_bitblast.rs:4903`, `.not().unwrap()` on a fallible OxiDD op — mununu#730, pre-existing, read as an abstention).

## Not covered here

- `[->n]` for n ≥ 2 and `[=n]` (a sequence automaton — Tier-3b); `AG EF` recoverability rows stay ⊥ where the cube certifies no MUST path.
- A HOLDS `EF` carries no path in the JSON (there is no witness field for a holding property; the trace exists inside the rescue and is a follow-up).
