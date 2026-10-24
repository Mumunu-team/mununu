# Consumer briefing — `check-fsm --liveness`: the FSM liveness inventory (2026-10)

> Source of truth: [`fsm_liveness_scan`](../../crates/mununu-core/src/adapter/fsm_scan.rs) — surface: (CLI+API) — `btor2 check-fsm` / `sv check-fsm` `--liveness [--terminal REG=VALUE]`, `liveness` / `terminal` on `POST /api/v1/{btor2,sv}/check-fsm`

**Audience:** rosf, monono, mununu-ui.

## TL;DR

- **New, opt-in:** `--liveness` adds, per scanned FSM register (out of reset, resets pinned inactive): legal encodings never reached (`unreachable`, reach portfolio), reachable encodings it can never leave (`absorbing`, exact engine on `AG(reg == v → EF reg ≠ v)`), whether it always returns to its reset value (`returns_to_reset`, `AG EF`), and the undecided values. `--terminal REG=VALUE` lists an intended absorbing state under `expected_terminal`.
- **Report-only:** the exit code is still the encoding scan's.
- **Shape:** a `liveness` array beside `registers` in the CLI JSON and `Btor2CheckFsmResponse` (absent unless requested); request fields `liveness`, `terminal`. Schemas regenerated.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** | the cheatsheet's `check-fsm` call is unchanged unless it passes `--liveness` | optional |
| **monono** | none unless adopted | optional |
| **mununu-ui** | new optional fields | none required (RTL verb is CLI+API) |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes, to use `--liveness` | new flag |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes | SV verbs run here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | no | opt-in |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | no | opt-in |

## Measured

| run | result |
|---|---|
| `the_liveness_inventory_finds_a_dead_value_an_absorbing_state_and_no_return` | a hand-written BTOR2 FSM: 5 unreachable, 2 absorbing, `returns_to_reset` violated; `--terminal st=2` moves 2 to expected |
| `sv check-fsm --liveness` on an enum FSM with an absorbing `FAULT` (mununu-sva) | `absorbing: [4]`, `returns_to_reset: violated`; with `--terminal state_q=4`, `expected_terminal: [4]` |

## Provenance

- Commit: pending merge — branch `feat/check-fsm-liveness`.
- Docs: [`wiki/CI-and-Agent-Integration.md`](../../wiki/CI-and-Agent-Integration.md) §"The zero-input FSM auto-scan".
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- One-hot FSMs split across cells, coupled FSMs, enum member names in the report (values only).
