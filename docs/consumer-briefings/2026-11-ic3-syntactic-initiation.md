# Consumer briefing — the owned IC3 decides the constant-init class it used to time out on (N5.6)

> **Audience:** maintainers of the repositories that consume `mununu btor2 verify --owned-only`, `sv verify-auto`'s reach rescue on a host without rIC3, or `POST /api/v1/btor2/verify` (rosf, monono, mununu-ui). Not a release note.
>
> Source of truth: [`Ic3::initial_syntactic`](../../crates/mununu-core/src/adapter/btor2/ic3.rs#L668) · [`Ic3::initial`](../../crates/mununu-core/src/adapter/btor2/ic3.rs#L639) — surface: CLI+API

## TL;DR

The owned bit-level IC3 member (`ic3`, Track N) now decides its initiation checks syntactically on registers whose `init` is a constant, and asks the SAT solver only for a cube that touches a register without `init` or with an expression init. No field, flag, route or default changes; no verdict value changes meaning. What changes for a consumer:

1. **Designs that were `unknown` under `--owned-only` can now be `holds`.** The class is "every register has a constant init and the proof needs a wide, deep IC3 search": the OpenTitan edn `FpvSecCm*` monitors are the measured case. Before, `ic3` spent the whole 300 s slice on its initiation checks and proved 0 of 14; now the member alone proves all 14 in 3.2–4.1 s each (provenance). A consumer that gates on those rows being `unknown` will see them flip to `holds` by `reach-rescue` / `unreachable_by: ["ic3"]`.
2. **The same verdicts elsewhere.** Where `ic3` already decided, it decides the same way; where another member decided first, nothing moves. The HWMCC'20 owned-only arm on this build is in the provenance (no verdict regression, initiation time within the pre-registered bound).
3. **The `ic3 search` debug line** (`RUST_LOG=mununu_core::adapter::btor2::ic3=debug`) gains `init_syntactic=<n>` beside `init_queries=<n>`: how many initiation checks the syntactic path answered. A log scraper keyed on that line's fields must allow the new one.

Every `ic3` verdict is still checked before it ships: a `Safe` by the direct check of its invariant in a fresh solver, a `Violated` by a replay on the concrete simulator. The syntactic check is exact with respect to the solver's answer by construction (a literal against a constant init is UNSAT; a cube `Init` implies is SAT iff `Init ∧ constraints` has a state, asked once), and a 3,000-cube differential test pins it on a design with constant, absent and expression inits.

## Per consumer

### rosf · monono (CLI and HTTP)

- `--owned-only` / `owned_only: true`: more `holds` rows on the constant-init class; `unreachable_by` reads `"ic3"` there. Shape unchanged.
- `sv verify-auto` on a host without rIC3: the reach rescue's `ic3` member decides the sec_cm monitors inside its slice; `decided_by` reads `reach-rescue` as before. In the images (rIC3 present) the full portfolio's member set is unchanged since N5.5, so the rows those images already decided by `ric3` keep their decider.
- Full portfolio beside rIC3: no change (the member is not in that set by default).

### mununu-ui

No CTXDSL surface; no change required.

## Docker rebuild

| image | rebuild | why |
|---|---|---|
| `mununu` (product) | yes | the owned member's reach under `--owned-only` |
| `mununu-sva` (dev / e2e) | yes | same; the gate's rows run here |
| consumers' own images | no | no schema change; `docs/api-schemas/` untouched |

## Provenance

N5.6 (Track N), measured in `mununu-sva` on 2026-11-17 on this branch (`144bb81a`, then `13a39940` on main `3d1c235a`; the second commit dropped a one-literal shortcut in the initiation repair that the first had added — it bought nothing, so the repair keeps its original add-back walk, now free on constant-init registers). Falsifier pre-registered in the N5.5 provenance: on the AckFsm ep1 monitor `init_ms` under 1 s **and** at least 8 of the 14 edn sec_cm monitors proved within 300 s; the HWMCC'20 M2 68 under `--owned-only` with no verdict regression and `init_ms` within ~1.2× of the N5.1 run.

- **Member alone, per-phase profile, 300 s each:** 14 / 14 monitors `safe` (certified), 3,178–4,096 ms each, `init_ms` 1–3, `init_syntactic = init_queries`, `repair_queries` 0. On ep1 the N5.5 build had reached frame 13 after 300 s with 100,280 initiation calls taking 299.2 s. The three monitors rIC3 `-e ic3` standalone left unknown at 300 s (AckFsm ep6, ep7, ResCmd WPtr) are among the 14.
- **Shipped `--owned-only` driver on the 14 (`13a39940`), 300 s each:** 14 / 14 `holds`, `unreachable_by: ["ic3"]`, 7.9–11.2 s wall per row (the driver's other members run beside it).
- **M2 68 `--owned-only` (`13a39940`) against the N5.1 arm (52 / 68):** **51 / 68** (28 violated / 23 holds), **0 disagreements, 0 gained, 1 lost: `blocks.4`** (N5.1 and main `f5cfa52d`: `violated` by `ic3` in ~30 s, a deterministic 96,751-query search; here `unknown` at 300 s). Wall on the 51 co-decided rows: median 0.99×, max 1.5×, none over 2×; `init_ms` 0 on every row. The loss is this change's own and is not a wrong answer: every SAT/UNSAT answer the search gets is the one the solver would have given (the differential test), but the solver no longer sees the ~100k initiation queries, so its learned-clause and phase state differs and the search on `blocks.4` takes a trajectory that does not reach the counterexample in the slice — the seed-class sensitivity N5.2 measured on this engine. There is no way to keep the old trajectory without paying the queries it came from; the row is named here as N5.1 named `mul9`. A consumer that gates on `blocks.4` under `--owned-only` sees it move from `violated` to `unknown`.

Private evidence: `mununu-private/experiments/competitiveness-2026-11/n56-syntactic-initiation/` (`arm-a-shortcut/`, `arm-b-final/`, `bisect/`, `blocks4.log`).

## Not covered here

The full edn posture (`run.sh --json --fault-model sec-cm`) was not re-run for this briefing; the member-alone and driver rows above are the measurement. The three N5.2 per-row items for the only-rIC3 HWMCC rows (latch / CNF reduction, `ric3-bmc --bmc-kissat`, internal-signal lemmas) are not in this change, nor is `ic3` in the k-liveness rungs' set.
