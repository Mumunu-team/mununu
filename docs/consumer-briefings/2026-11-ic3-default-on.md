# Consumer briefing — the owned IC3 member (`ic3`) is on by default (N5.5)

> **Audience:** maintainers of the repositories that consume `mununu btor2 verify`, `--owned-only`, `sv verify-auto`'s reach rescue, or `POST /api/v1/btor2/verify` (rosf, monono, mununu-ui). Not a release note.
>
> Source of truth: [`ic3_default`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) · [`ReachMembers::shipped_for`](../../crates/mununu-core/src/adapter/reach_portfolio.rs) — surface: CLI+API

## TL;DR

The owned bit-level IC3 member (`ic3`, in-process CaDiCaL IC3/PDR, Track N N5.1) now runs without `MUNUNU_REACH_IC3=1` in two places: under `--owned-only`, and in the full reach portfolio on a host where no rIC3 is invokable. Beside an invokable rIC3 (every `mununu` / `mununu-sva` image: oss-cad-suite bundles it) the full portfolio is unchanged. `MUNUNU_REACH_IC3=0` removes the member from every decider set, `=1` adds it to every one; the N5.4 trace-carrier search (which derives a trace for an already-decided violation) keeps `ic3` whatever the value, as it did before this change. No field, flag or route is added or removed; no verdict value changes meaning. What changes for a consumer:

1. **`reachable_by` / `unreachable_by` can now contain `"ic3"`**, and on some designs it is the only entry. The strings were documented members since N5.1 (`docs/api-schemas/btor2-verify-response.schema.json` names `ic3` in `trace_by` and `trace_replay_failed[].member`); a consumer that whitelists member names must include it.
2. **More designs decide under `--owned-only`**, and the decider on a design that another member used to decide may now be `ic3` (first definite verdict wins; the finished set is timing-dependent, as before).
3. **`trace_by: "ic3"` with `--witness`**: the carried counterexample (N5.4 briefing) may come from `ic3`'s own replayed trace.
4. **CPU and wall:** where the member runs, one more in-process thread, including every reach-rescue slice of `sv verify-auto` on a host without rIC3. Measured under `--owned-only` on the HWMCC'20 M2 68 at 300 s, same box, with `ic3` (N5.1 run) against without (the N5.4 F0 run): 52 decided against 35, 0 disagreements on the 34 co-decided, and on those the wall is a **median 1.10× longer** with `ic3` (faster on 8 of 34; the slowest ratio 1.89× on `shift_register_top_w16_d8_e0`); one row decided without `ic3` is lost with it (`mul9`, at the deadline edge). On the OpenTitan edn sec_cm class the member decides nothing within 300 s and spends the slice on its initiation checks; the measured cost of that idle thread on a host without rIC3 is nil on the edn posture: 52 rows, no verdict changes, wall ratio median 1.00 and no row over 2× between `MUNUNU_REACH_IC3=0` and the default (provenance, criterion 4).
5. **What does not change:** the edn sec_cm rows that only rIC3's portfolio decides today stay rIC3-only, and undecided under `--owned-only`; the gate measured `ic3` at 0 / 14 monitors there (the row list is in the provenance). Beside an invokable rIC3 the default member set is the one main ships today.

Every `ic3` verdict is checked before it ships: a `Safe` by a direct check of the inductive invariant in a fresh solver, a `Violated` by a replay of its trace on the concrete simulator. A failed check abstains with the reason (`abstained[]`), never a verdict.

## Per consumer

### rosf · monono (CLI and HTTP)

- `reachable_by` / `unreachable_by` / `trace_by`: `"ic3"` is a possible value. Shape unchanged.
- `MUNUNU_REACH_IC3=1` (the N5.1 opt-in) puts the member in every decider set; `=0` in none; unset is the scoped default above. No other knob.
- `sv verify-auto`: on a host without rIC3 the reach rescue's portfolio includes `ic3`; a row's `decided_by` may read `reach-rescue` as before with `ic3` the member underneath (the per-member line is in the `reach_portfolio=debug` log). In the images nothing changes.

### mununu-ui

No CTXDSL surface; no change required.

## Docker rebuild

| image | rebuild | why |
|---|---|---|
| `mununu` (product) | yes | the default member set |
| `mununu-sva` (dev / e2e) | yes | same; the gate's e2e rows run here |
| consumers' own images | no, unless they pin a member whitelist | no schema change; `docs/api-schemas/` untouched |

## Provenance

N5.5 gate (Track N), measured in `mununu-sva` on 2026-11-16 at `3dc3101f` (the build under test; this PR changes only the default). Criteria and results: (1) the eight edn sec_cm rows only rIC3 decides today — **fails**, the owned `ic3` decides 0 of the 14 FpvSecCm monitors at 300 s under `--owned-only` (rIC3 `-e ic3` standalone decides 11; the posture's nine `holds` are all the `ric3` member); the measured cause is the initiation check, 100,280 SAT calls taking 299 of the 300 s on a monitor rIC3 proves in 0.7 s, and the fix (a syntactic check for constant-init registers) is the next lane item, not this PR; (2) HWMCC'20 ≤ 60 KB subset under `--owned-only` ≥ 35 / 68 — **passes**, 52 / 68 at 300 s with 0 disagreements, and the four rows the interpolation member alone used to decide (gen12 / 14 / 39, am2910) are decided by `ic3`; (3) wall-class matrix with the member off and on — **passes**, both arms `3 passed; 0 failed` (516 s and 502 s) and every verdict cell identical between them; (4) the edn sec_cm posture on this build with rIC3 hidden from the locator (btormc and pono present), `MUNUNU_REACH_IC3=0` against the default — **passes**, 52 rows with no verdict change, wall ratio median 1.00 and maximum 1.64 (pre-registered bound 2×), the `ic3` member running as a decider in 41 rescue slices in the default arm and in none in the `=0` arm. Private evidence: `mununu-private/experiments/competitiveness-2026-11/n55-default-on-gate/`. The N5.1 owned-only row (52 / 68 on the M2 set at 300 s, 0 disagreements) and the all-136 arm C (101 / 136 with `ic3` credited on 12) are the member's prior rows. No scoreboard row changes: the owned-only row already ran with `ic3`; the full-portfolio rows ran beside rIC3, where the default is unchanged.

## Not covered here

The k-liveness rungs (`ReachMembers::PROOF_DIRECTED`, justice and the response verbs' fallback) do not include `ic3` yet; that set has its own measurement (Track V). The three per-row items N5.2 named for the only-rIC3 rows (a latch/CNF reduction pass, `ric3-bmc --bmc-kissat`, internal-signal lemmas) are not in this change.
