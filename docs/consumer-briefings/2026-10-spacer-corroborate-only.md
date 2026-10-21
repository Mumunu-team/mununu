# Consumer briefing — SPACER corroborates, never decides alone (2026-10)

> Source of truth: [`collect`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) (the reach portfolio's merge) + [`run_spacer`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: (CLI+API) — `btor2 verify` / `POST /api/v1/btor2/verify`, and every `sv verify-auto` row the reach portfolio decides (the `reach-rescue` rows)

**Audience:** rosf, monono, mununu-ui and anyone pinning `btor2 verify` verdicts or `unreachable_by` / `reachable_by` attributions. PR B of the 2026-10-20 open-issue round (mununu#740, #738); follows PR A (#745) and PR C.

## TL;DR

- **A lone SPACER verdict decides nothing, in either direction** (mununu#740). The `reachable` direction was already distrusted (July 2026: a spurious counterexample on `vcegar_arrays_itc99_b12_p2`); the `unreachable` direction is now too: libz3 4.13.3's SPACER — the build `mununu-sva` and `mununu-sva-pono` link — proved HWMCC'20 `vis_arrays_am2901` **safe**, and `btor2 verify` shipped `holds` with `unreachable_by: ["spacer"]`. The property is violated at depth 16 (six competition solvers; rIC3 prints the 17-step witness in 0.2 s). mununu's CHC encoding was cleared (the native BMC on the same encoded view, with the inputs pinned to the witness, finds the violation in 0 s; z3 4.16 answers `unknown` on the same CHC), so it is a wrong `unsat` from that z3 build. SPACER still corroborates: a verdict another member shares keeps both names; a SPACER-vs-other disagreement is still a contradiction alarm.
- **Verdicts that change.** A design only SPACER could prove now reports `unknown`. Measured below on the HWMCC'20 bv adjudication: exactly **one** row, `h_RCU` (ground-truth safe), moves from `unreachable` to `unknown`; `gen31`, `gen35` and `gen44` — SPACER-alone the day before — stay decided as proofs shared with rIC3, because a SPACER answer no longer cancels the members whose corroboration it needs; `vis_arrays_am2901` had already moved from the wrong `holds` to rIC3's `reachable` on PR A's run. On the OpenTitan `edn` rescue rows SPACER was the lone decider on none: **0 verdict changes** there.
- **SPACER cancels nothing and never blocks a cancellable portfolio.** The member used to set the portfolio's `decided` flag on a `safe`; it no longer does (it decides nothing alone, so it must not cancel the members whose corroboration it needs). With no isolated child (`MUNUNU_SELF_EXE` unset — unit tests and Rust-API embeddings; the CLI and the server always set it), the uncancellable in-process query is skipped instead of holding every rescued row for its 10-s floor.
- **The HWMCC adjudication harness survives one bad instance** (mununu#738): `MUNUNU_HWMCC_SKIP=<substr>[,…]` skips named files (`SKIPPED` in the table) and `MUNUNU_HWMCC_TIMEOUT_MS` (default 300 s) runs every instance under a run budget. The probe that named the hang: on `vis_arrays_bufferAlloc` the only member that crashes is z3's Fixedpoint in-process (the isolated child already contains it); the unbudgeted full portfolio never returns, and neither does a budgeted one — the exact engine's bit-blast runs on the calling thread with no clock — so the skip list is the containment for that instance and #738 stays open. The harness is a test, not a shipped verb.
- **Re-promotion criterion**, recorded here so it is not forgotten: a libz3 ≥ 4.16 pinned in `docker/Dockerfile.sva` (and `-pono`) **and** a clean re-adjudication of the 67 HWMCC'20 bv instances under 60 KB plus the 33 OpenTitan rescue rows.
- No JSON shape change. No CLI flag or route change.

## Per consumer

| consumer | what changes for you | action |
|---|---|---|
| **rosf** (`--profile industrial`, `sv verify-auto` subprocess) | a row that only SPACER proved reports `unknown` (`bottom_reason` names the abstention); no rosf row was of that shape on the last lane | rerun; move a pin only where a row flips to `unknown` |
| **monono** (direct `sv verify-auto` CLI) | same | rerun the lane at the next pin |
| **mununu-ui** | `unreachable_by` may no longer list `spacer` alone | none |
| anyone running `btor2 verify` on HWMCC / hand-written designs | `unreachable_by: ["spacer"]` alone no longer appears; such designs read `unknown` unless rIC3 / btormc / pono / native / exact decide them | re-check pinned verdicts |

## Docker rebuild

| image | rebuild? | why |
|---|---|---|
| `mununu/docker/Dockerfile` (production) | yes if consumers pin a version tag | the merge rule lives in the binary |
| `mununu/docker/Dockerfile.dev` | only if the dev workflow requires the new binary | — |
| `mununu/docker/Dockerfile.sva` (`mununu-sva`) | yes — mandatory | the gates below ran here |
| `mununu/docker/Dockerfile.profile` | no | profiling image, unaffected |
| `mununu/docker/Dockerfile.extract`, `.extract-circt`, `.extract-llvm` | no | extraction images do not run the engines |
| rosf `docker/Dockerfile`, `Dockerfile.dev`, `Dockerfile.hw`, `Dockerfile.oss-cad` | behaviour updates on next binary pull | rows that only SPACER proved (none known) read `unknown` |
| monono `docker/Dockerfile.dev`, `Dockerfile.vexriscv` | behaviour updates on next binary pull | same |

## Measured

| run | before | after |
|---|---|---|
| HWMCC'20 bv adjudication (`hwmcc_adjudication_over_user_dir`, the 67 instances under 60 KB, user-downloaded corpus, `MUNUNU_HWMCC_TIMEOUT_MS=120000`, `MUNUNU_HWMCC_SKIP=vis_arrays_bufferAlloc`) | 42 instances before the mununu#738 hang: 7 reachable / 25 unreachable / 10 unknown (PR A's run); `gen31`, `gen35`, `gen44`, `h_RCU` decided by SPACER alone | **the table completes: 66 adjudicated + 1 skipped, 49 decided (25 reachable / 24 unreachable) / 17 unknown in 55 min**; every verdict agrees with the official per-solver results (0 label mismatches, 0 contradictions); **no lone-SPACER attribution anywhere**; SPACER corroborates 14 proofs; on the first 42 the only change is `h_RCU` → `unknown` (the measured cost) and `krebs.3` → `reachable` by btormc (correct); `gen31` / `gen35` / `gen44` stay decided, shared with rIC3 |
| `vis_arrays_bufferAlloc` member by member (the mununu#738 probe) | the unbudgeted harness never returned on it | only the in-process z3 Fixedpoint crashes (SIGSEGV 4 s — contained by the isolated child); a budgeted full portfolio does not return either (the exact engine's bit-blast on the calling thread, no clock) — the skip list is the containment |
| `e2e_opentitan_ip_edn_full` (OpenTitan `edn` + its 15 system properties, shipped defaults; in-process SPACER now skipped under a cancellation flag) | 56 holds / 8 violated / 3 ⊥ / 0 unsupported in 1 161 s (PR A's run) and 1 431 s (PR D's) | **PASS, pins unchanged (56 / 8 / 3 / 0), 922 s** — every rescued row loses the 10-s uncancellable floor; 0 verdict changes |

## Verification steps for every consumer

```bash
# a design SPACER alone proves (a 300-bit stall counter): expect unknown on a host without rIC3 / btormc / pono,
# unreachable_by ["ric3"] (or btormc / pono) in mununu-sva — never ["spacer"] alone
printf '1 sort bitvec 300\n2 zero 1\n3 one 1\n4 state 1 c\n5 init 1 4 2\n6 sort bitvec 1\n7 eq 6 4 2\n8 add 1 4 3\n9 ite 1 7 2 8\n10 next 1 4 9\n11 ones 1\n12 eq 6 4 11\n13 bad 12\n' > stall300.btor2
mununu btor2 verify stall300.btor2
```

## Provenance

- Fix commit: pending merge — branch `fix/spacer-corroborate-only-740`.
- Issues: [mununu#740](https://github.com/vscorza/mununu/issues/740), [#738](https://github.com/vscorza/mununu/issues/738).
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md); reproducers per [`docs/policies/reproducers-before-fixes.md`](../policies/reproducers-before-fixes.md).

## Not covered here

- Pinning a newer libz3 in the images (the re-promotion precondition).
- mununu#744 (the native BMC's shallow-witness search) — deferred with its variants on the issue.
- mununu#723 — PR D.
