# Consumer briefing — the k-liveness rung for justice properties (Track V, 2026-11)

> Source of truth: [`kliveness_rung`](../../crates/mununu-core/src/adapter/justice.rs#L353), [`emit_kliveness_monitor`](../../crates/mununu-core/src/adapter/btor2/kliveness_monitor.rs#L49), [`ReachMembers`](../../crates/mununu-core/src/adapter/reach_portfolio.rs#L490) — surface: CLI+API (`btor2 verify-liveness --justice --rung kliveness|both`, `btor2 verify-justice`, the justice form of `POST /api/v1/btor2/verify-liveness` with `rung`).

> **Audience:** anyone running the justice verbs (HWMCC-style `justice` / `fair` lines, AIGER 1.9) on the CLI or the API; lanes driving `scripts/hwmcc_live_study.sh`.
>
> **TL;DR — more proofs, same verdict vocabulary, no JSON shape change.** A third rung, **k-liveness**, joins the exact fair-cycle fixpoint and l2s. Under the default `--rung both` it runs after l2s (sequentially: exact 30 s → l2s → k-liveness → exact remainder; under `--parallel-rungs` all three race). It proves a justice property by bounding the number of fair rounds and proving the bound as safety with the IC3 / k-induction members; it **never reports VIOLATED**. `decided_by` gains the label `kliveness` followed by `k=<bound>` and the deciding member. `--rung kliveness` / `rung: "kliveness"` runs it alone. Verdicts can only move `unknown → holds`; no verdict can flip.

## What changed

| surface | item | meaning |
|---|---|---|
| CLI + API | `--rung kliveness` / `rung: "kliveness"` | the new rung alone (proofs only) |
| CLI + API | `--rung both` (default) | sequential order is now exact ≤ 30 s → l2s (½ of the remainder) → k-liveness (½ of what l2s leaves) → exact remainder; `--parallel-rungs` races three rungs |
| CLI + API | `decided_by` | `["kliveness", "k=<2^w−1>", "<member>", …]` on a k-liveness proof |
| CLI + API | `notes[]` on a k-liveness abstention | `kliveness: no fair-round bound proved within a <ms> ms budget (k=1 reachable, k=3 reachable, …)` plus every member's refusal / abstention per bound |
| engine | `ReachMembers` on the reach portfolio | internal: the rung starts only rIC3 (its own portfolio mode), Pono and native k-induction — no exact (uninterruptible, mununu#816), no BMC, no interpolation. The shipped portfolio (`ReachMembers::ALL`) is unchanged for every other caller |
| engine | rIC3 portfolio mode | `ric3::PORTFOLIO_ENGINE` (no `-e`): the rung's rIC3 runs rIC3's own multi-configuration portfolio; the reach portfolio's member keeps `-e ic3` |

**Budget effect under `--rung both`:** l2s now gets half of what the exact slice leaves (unchanged since #976) and k-liveness half of what l2s leaves; the exact remainder is correspondingly shorter. A row the exact remainder used to decide in that last quarter may now be `unknown` under a tight budget — the measured LIVE data (every exact decision ≤ 7 s, in the slice) says this does not happen on the known sets, and the gate below checks it.

## Per consumer

| consumer | what this means for you | action |
|---|---|---|
| **rosf** / **monono** | You do not call the justice verbs. | None. |
| **mununu-ui** | No justice UI. | None. |
| benchmark lanes (HWMCC LIVE) | More `holds` rows, with `kliveness` in `decided_by`; adjudicate against `live.csv` as before. | Re-run rather than merge with earlier rows. |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu-sva` / the release images / the on-prem bundle | Yes, to get the rung. | Engine behaviour. |
| rosf `docker/Dockerfile*`, monono `Dockerfile.dev` / `Dockerfile.vexriscv` | No. | Not callers. |

## Provenance

Track V (`mununu-private` roadmap, 2026-11-10), from the competitiveness measurements M2/M3/M3b: the incumbents (abclive, iimc, nuxmvlive) prove HWMCC'17 LIVE instances in 1–355 s that the exact rung abstains on (node budget) and l2s leaves open at 3,600 s; they run k-liveness / FAIR. V0 (the falsifier before the code): the hand-built round-counter monitor proved by rIC3 standalone — `cuabq4fro` at k = 15 in 1 s, `cutarb8ro` at k = 255 in 14 s — and the full portfolio did NOT return in 400 s on the same monitor (the exact member), which is why the rung uses the proof-directed member set. Tests at the seam: the monitor counts rounds, not simultaneous literals (`a_round_needs_every_literal_not_all_at_once`); the rung proves the stuck design and names the bound; it never reports VIOLATED on a fair cycle; the sequential default on the counter is unchanged.

## Not covered here

The response shape (`AG(a → AF b)`, `btor2 verify-liveness --request/--grant`, `sv verify-liveness`) and recoverability are Track V's V2 / V3, not in this change. The verify-auto planner is untouched (V4). rIC3's SIGABRT on the l2s monitor is mununu#980.
