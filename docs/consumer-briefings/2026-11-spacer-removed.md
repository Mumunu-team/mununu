# Consumer briefing — z3-SPACER leaves the reachability portfolio (2026-11)

> Source of truth: [`decide_reach_portfolio_cancellable`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) + [`collect`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API — `reachable_by` / `unreachable_by` of `btor2 verify`, the `portfolio-rescue` notes and `decided_by` of `sv verify-auto`, and every rescue lane that calls the reach portfolio (`POST /api/v1/btor2/verify`, `POST /api/v1/sv/verify-auto`; no UI affordance).

**Audience:** rosf, monono and anyone who reads the reach portfolio's member names.

## TL;DR

- **The member is gone.** The `spacer` member (z3's SPACER over mununu's btor2→CHC encoding) is removed from the reach portfolio, with its isolated child (`btor2 spacer-check`, a hidden subcommand) and the `MUNUNU_SELF_EXE` variable that pointed at it. This is removal R2 of the commercial-gap plan, Track N step N2.
- **No decided verdict can change from losing it.** Since mununu#740 SPACER was **corroborate-only in both directions**: a SPACER answer that no other member shared was already dropped as `unknown`. So no row on `main` was decided by SPACER alone, and none can move to `unknown` now.
- **What changes:**
  - **The member name.** `"spacer"` no longer appears in `reachable_by` / `unreachable_by`, in a `portfolio-rescue` note's engine list, or in `refused`. A row that read `unreachable_by: ["spacer", "ric3"]` now reads `["ric3"]`.
  - **One cross-check fewer, and the one verdict change.** A SPACER-vs-other disagreement used to raise `Contradiction` (`bottom-reason [engine-contradiction]`, ⊥). Such a row now gets the other member's verdict, so it moves from ⊥ to decided. Every such disagreement on record was a wrong SPACER answer; see Provenance.
  - **Wall time.** The isolated child no longer runs, so no SPACER timeout reaches a row's wall.
- **No JSON shape change.** The fields are lists of member names; one name stops appearing.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **monono** | `spacer` no longer appears in member lists. Ask 45 (`rtl/apu/pcm_voices` witness `EF st_q == 11`: `reachable@[ric3] vs unreachable@[spacer]`, ⊥ `engine-contradiction`) loses its wrong member. The row is **expected** to decide HOLDS via rIC3; this has **not been measured** on monono's lane | re-run `rtl/apu/pcm_voices/verify.sh` with the ⊥ escalations on and confirm before un-pinning the witness; `docs/upstream.md` Ask 45 can then be closed as resolved upstream |
| **rosf** | `spacer` no longer appears in member lists; `docs/cheatsheet.md` lists SPACER among the reach-portfolio members and describes the SPACER-only downgrade | update the cheatsheet's portfolio row; no code reads the name |
| **mununu-ui** | none: no UI affordance; one doc comment in `src/api/endpoints.ts` lists `spacer` among the members | optional comment fix |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag and read member names | the portfolio lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow reads member names | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes if you run `btor2 verify` / `sv verify-auto` in the image | the binary the volume carries changes; the image's toolchain does not (z3 stays linked for the other engines) |
| `mununu/docker/Dockerfile.profile` | no | profiling image |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not verify |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | yes if rosf pins a mununu version and reads member names | picks up the change |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | yes if monono pins a mununu version; required to pick up the Ask 45 change | picks up the change |

## Provenance

- **#740.** SPACER's record: a spurious `reachable` over the btor2→CHC encoding (`vcegar_arrays_itc99_b12_p2`, ground truth safe), and a wrong `unsat` from libz3 4.13.3 on HWMCC'20 `vis_arrays_am2901` (violated at depth 16). That made it corroborate-only in both directions.
- **Lone reach.** On the 2026-10 adjudications its lone reach was 2 of 42 HWMCC rows (am2901 wrong, `h_RCU` right, already `unknown` since #740) and 0 of 33 edn + csrng rescue rows.
- **monono Ask 45 (2026-10-23).** SPACER said `unreachable` on a state the testbench reaches every tick.
- **Ledger.** The verdict ledger (`measurements/ledger/verdicts.csv`, #861) has no row attributed to SPACER.
- **Tests.** The portfolio's unit tests are updated, and the two corroborate-only guard tests go with the member.

## Not covered here

- The emergent-K research track's possible use of a CHC encoder: the encoder is deleted, and the plan keeps it only on a written request.
- z3 stays linked: native BMC / k-induction, the term backend and the cube lift use it.
- cvc5's removal is a separate step, after the owned IC3 lane is measured.
- The historical briefings and measurement notes that name SPACER stay as the record.
