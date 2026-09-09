# Consumer briefing — 2026-09 self-imposed process-memory ceiling `MUNUNU_MAX_PROCESS_MEMORY_BYTES` (mununu#490)

> **⚠️ SUPERSEDED IN PART (mununu#504 C6) — see
> [`2026-09-memory-ceiling-auto-default.md`](2026-09-memory-ceiling-auto-default.md).**
> This briefing's *"Default unset ⇒ disabled — no behaviour change unless you opt in"* is
> **no longer true**: unset now resolves to 80% of a detected cgroup memory limit, so the
> ceiling is ON by default inside a memory-limited container. Everything else here — the
> mechanism, the `memory-budget-exceeded` note, the coarse-granularity caveat — still holds.

> **Audience:** monono (primary reporter and beneficiary — their 25-check `sv verify-auto` lane hits this today), ROSF (API consumer via `--profile industrial`), any orchestrator running `sv verify-auto` under a memory-constrained container / CI runner.
>
> **Related:** [mununu#490](https://github.com/Mumunu-team/mununu/issues/490) — the ticket. **Distinct from #462**: #462 fixed the BDD library's `Err(OutOfMemory)` handling (bit-blast library abstains cleanly). This ticket is one layer further out: the default allocator's `abort()` (exit 134) fires BEFORE any library-level budget check can, taking every property in the same invocation down with it.
>
> **TL;DR:** new `MUNUNU_MAX_PROCESS_MEMORY_BYTES` env var — a self-imposed process-RSS ceiling. When exceeded on `sv verify-auto`, remaining properties abstain (`unknown`) with a `memory-budget-exceeded` verification note; prior verdicts stay. **Default unset ⇒ disabled** — no behaviour change unless you opt in. Turns a crash-that-kills-the-lane into a graceful degradation.

## What changed

- **New dep** — `memory-stats = "1"` on mununu-core (cross-platform RSS reader, ~200 lines, no unsafe in mununu).
- **New module** — [`crates/mununu-core/src/adapter/memory_budget.rs`](../../crates/mununu-core/src/adapter/memory_budget.rs):
  - `MUNUNU_MAX_PROCESS_MEMORY_BYTES` const (the env var name).
  - `MemoryBudgetExceeded { current_rss_bytes, limit_bytes }` type + `Display` that names both numbers and the env var.
  - `check_memory_budget_bytes(current, limit)` pure helper (testable without env mutation).
  - `read_memory_budget_env()` (parses env; treats unset / zero / non-numeric as disabled).
  - `read_process_rss_bytes()` (wraps `memory_stats::memory_stats`).
  - `check_process_memory_budget()` composed helper — fail-open when env unset or RSS reader unavailable.
- **Check points** — two chokepoints on the SV verify-auto path in `crates/mununu-core/src/adapter/slang/verify_auto.rs`:
  - **Main per-property loop** — before each property. On hit: mark this + every remaining property as `Unknown { unknown_cells: 0 }`, push a `memory-budget-exceeded` note after the loop that names the ceiling + the observed RSS, preserve prior verdicts.
  - **`escalate_bottom` ⊥-escalation loop** — before each escalation attempt. On hit: break (leave the remaining Unknowns from the main loop as-is; don't attempt further escalation that may abort mid-BDD-blast).
- **Docs** — [`CLAUDE.md`](../../CLAUDE.md) Environment Variables table row; [`docs/verifying-rtl.md`](../verifying-rtl.md) new section on ceilings, sizing recommendation, complementarity with `--config-value`.

## What did NOT change

- **Wire format** — unchanged.
- **`PropertyVerdict` values** — same canonical vocabulary. `Unknown` is the only new outcome for the ceiling case (with `unknown_cells: 0` — engine-abstention-cell counts are meaningless when the engine never ran).
- **JSON schemas** — 17/17 drift-detector rows pass with zero regeneration; verification-note SHAPE is unchanged (only a new `kind` string appears).
- **CLI surface** — no new flag. Configuration is env-var-only, matching the existing `MUNUNU_BDD_*` budget vocabulary (bit cap, node budget, iteration budget, wall-clock budget).
- **Default behaviour** — unset ⇒ disabled ⇒ current behaviour. Consumers opt in explicitly.

## For monono, ROSF, and any lane consumer

**What to update:** nothing forced. To adopt the ceiling:

```bash
# Cap at 24 GiB (a container with 32 GiB total, leaving headroom):
export MUNUNU_MAX_PROCESS_MEMORY_BYTES=25769803776

# Then run the lane as before:
mununu sv verify-auto design_1.sv
mununu sv verify-auto design_2.sv
# ... etc — each invocation gets its own ceiling.
```

**Response parsing impact — additive-safe.** A consumer that inspects `verification_notes[i].kind` may now see `"memory-budget-exceeded"` on invocations that hit the ceiling. Properties past the hit will have `outcome: "unknown"` — same shape as any other unknown. To distinguish "hit the ceiling" from "engine abstained," check for the `memory-budget-exceeded` note kind.

**Soundness contract:**

- A ceiling hit is a SOUND abstention — never an over-approximation. Prior definite verdicts (`holds` / `violated`) stay.
- The check fires BETWEEN checkpoints. It does NOT catch a single allocation that fails within a BDD blast. The ceiling is a graceful-degradation lever for the multi-property lane, not an absolute crash guarantee.
- Recommended sizing: 70-80% of the process's real memory limit (`ulimit -m`, container `--memory`). Too tight ⇒ spurious abstentions. Too loose ⇒ process aborts before the ceiling fires.

**Complement to the existing `--config-value` workaround.** The ticket's reporter noted: pinning an additional free input via `--config-value SIG=V` shrinks the real cone (bit cap counts kept cone bits AFTER COI and AFTER pinning) and often keeps a property under the memory ceiling that would otherwise trip it. Predicate hints (`@mununu_predicate`) do NOT — they seed cube dimensions, and an alternating νμ formula is decided by the exact engine which does not use them. Use both together: the ceiling for the lane's crash-containment; `--config-value` for the individual over-budget property.

**Docker rebuild:** binary bump only; no subprocess tool changes.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | binary picks up the ceiling machinery; behavior unchanged unless the caller sets the env var | Yes if consumers pin a version tag; adoption is opt-in |
| mununu `Dockerfile.dev` | binary bump + 6 new memory_budget unit tests | Only if the dev workflow requires the new binary |
| mununu `Dockerfile.sva` | binary bump; no e2e slang-gated behaviour change | No — the ignored SVA e2e set is unaffected |
| mununu `Dockerfile.extract`, `.extract-*` | no impact | No |
| rosf `Dockerfile` / `Dockerfile.dev` / `.hw` | consumes mununu CLI/API; the ceiling helps their industrial-profile lane | No — behaviour updates on next binary pull; adopt the env var when the lane needs it |
| monono Docker (if any) | primary beneficiary — the 25-check lane can adopt the ceiling and stop losing whole runs to one property's OOM | No — pull the new binary; set `MUNUNU_MAX_PROCESS_MEMORY_BYTES` per the container's memory budget |
| mununu-ui deployment | no impact — env is a process-level knob | No |

## Verification steps

- `cargo test -p mununu-core --lib adapter::memory_budget::` — 6/6 unit tests pass (boundary cases, display format, env parsing, RSS reader).
- `cargo check --workspace --features mununu-core/api --tests` clean; no warnings.
- End-to-end (post-merge): set `MUNUNU_MAX_PROCESS_MEMORY_BYTES=<something-tiny>` on a real `sv verify-auto` invocation; expect the first property to abstain with the `memory-budget-exceeded` note in the report.

## Provenance

- Fix commit: (pending merge — branch `fix/490-process-memory-ceiling`).
- Ticket: [mununu#490](https://github.com/Mumunu-team/mununu/issues/490).
- Design record: `.claude/plans/490-process-memory-ceiling.md`.
- Policy: [`../policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
- Related shipped work: mununu#462 (BDD library OOM caught cleanly; this ticket is one layer further out).

## Not covered here (documented non-goals)

- **Sub-checkpoint allocation catch.** Requires fallible allocation at every allocation site in mununu + every transitive dep (`oxidd`, `z3-sys`, `boolector`, `serde`, `tokio`, `axum`, …). Not feasible without a custom allocator hook + rewriting every call. This ceiling is a coarse graceful-degradation lever, not an absolute crash guarantee.
- **Per-property subprocess isolation.** A cleaner containment shape (each property runs in its own subprocess so a crash is truly contained), but a real arch change: process spawn cost, IPC for verdict merge, engine-cache re-warm cost. Deferred as a separate track.
- **Wall-clock ceiling on the whole process.** `MUNUNU_BDD_TIME_BUDGET_MS` covers the BDD engine; a process-wide wall-clock ceiling could be a sibling env var later.
- **CLI-flag configuration.** Env-var-only stays consistent with the existing budget vocabulary. Consumers who need per-invocation control can `env MUNUNU_MAX_PROCESS_MEMORY_BYTES=… mununu …`.
