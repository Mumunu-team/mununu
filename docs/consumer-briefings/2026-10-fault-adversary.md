# Consumer briefing — the k-fault adversary: `btor2 verify --fault … --target`, `btor2 game --fault`; the game's strategy agrees with its verdict on free initial values (2026-10)

> Source of truth: [`verify_fault_reach`](../../crates/mununu-core/src/adapter/fault_adversary.rs) + [`btor2_game_handler_impl`](../../crates/mununu-core/src/api/handlers.rs) — surface: (CLI+API) — `btor2 verify` / `POST /api/v1/btor2/verify`, `btor2 game` / `POST /api/v1/btor2/game`

**Audience:** rosf, monono, mununu-ui and anyone who calls `btor2 game` (CLI or API) or gates on its verdict. PR C of the 2026-10-22 fault-injection plan.

## TL;DR

- **New question, opt-in:** `btor2 verify FILE --fault REG[:k] --target ATOM` — can at most `k` injections (after `--fault-warmup`, default 10) reach `ATOM`, with the attacker also driving every input. `violated` lists the attack (`attack.faults[]`: cycle, site, value). API: `target` beside `faults` on `/btor2/verify`; response gains `fault`, `target`, `attack[]` (absent/empty otherwise).
- **New game, opt-in:** `btor2 game … --fault REG[:k]` adds the fault inputs to the controllable side: `realizable` = an attack wins against every environment behaviour. **Under `--fault` the CI verdict flips: realizable → `violated`.** JSON gains `fault` and `attack_exists`. API: `faults` / `fault_warmup` on `/btor2/game`.
- **Behaviour change without any new flag — the game's strategy.** `btor2 game` / `/btor2/game` used to return a CONTROLLER strategy whenever SOME initial state was winning, while `realizable` (the verdict) requires EVERY initial state to be winning. On a design with a free initial value the two disagreed: `realizable: false` beside a `controller_strategy`. Now the strategy agrees with the verdict (an `environment_counterstrategy` in that case). Same for the recurrence (Büchi) strategy. Designs whose initial state is fixed (every reset-initialized design) are unaffected.
- **`strategy_error`:** when the strategy cannot be extracted (a goal over no state register, the cap), the reason is now reported (`strategy_error`, string) instead of the field silently missing.
- Schemas: `btor2-verify-request` / `-response` regenerated (game types are not schema-pinned).

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | nothing unless you call `btor2 game` on designs with a free initial value — then `strategy.kind` may change from `controller_strategy` to `environment_counterstrategy` (matching `realizable`) | if you read `strategy` without `realizable`, read `realizable` first |
| **monono** | same | none required |
| **mununu-ui** | additive response fields | none required (RTL verb) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the adversary and the strategy fix live in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the keymgr rows below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | the strategy agreement fix |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured

| run | result |
|---|---|
| known-answer tests (`fault_adversary` module + `tests/fault_adversary.rs`) | trap escape needs one fault; redundant copies hold against one fault and fall to two in the same cycle; a vetoable attack is unrealizable as a game while it exists for the strongest attacker; the edn boot FSM: unrealizable at k=0, realizable at k=1 |
| `the_strategy_and_the_verdict_agree_when_only_some_initial_states_win` | fails on the parent (controller strategy beside `realizable: false`), passes here |
| keymgr K1–K3 (block-level dump, 120 registers) | K1 `--target state_q == 680`: violated in 53 s, but with no injection — `StCtrlDisabled` is reachable without a fault on the block model, so the target does not calibrate the rewrite; K2 (undetected jump to `StCtrlOwnerKey`): undecided at the 30-min limit; K3 game: realizable in 18 s, the attack injects `state_q := StCtrlDisabled` — a corruption can force the safe state, the denial of service the design accepts. No OpenTitan finding is claimed. |

## Verification steps for every consumer

```bash
mununu btor2 verify design.btor2 --fault state_q:1 --target "state_q == 3" | jq '{verdict, attack}'
mununu btor2 game design.btor2 --good "done == 1" --fault st:1 | jq '{realizable, attack_exists, strategy_error}'
```

## Provenance

- Fix commit: pending merge — branch `feat/fault-adversary`.
- Plan: fault-injection posture, linear-FSM properties and the k-fault adversary (2026-10-22), PR C.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- A strategy indexed by more than the goal's register (a state-dependent forced input can be left out of an entry), a concrete value per move.
- `sv game` (the path is `MUNUNU_SHADOW_BTOR2_DUMP` → `btor2 game --fault`); the KMTS two-player cube as a decider.
- The recoverability template `AG(¬T → EF R)` (C3, deferred).
