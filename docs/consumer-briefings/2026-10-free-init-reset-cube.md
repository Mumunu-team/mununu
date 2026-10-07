# Consumer briefing — 2026-10 the cube paths no longer pin a free-init register to zero in their reset cube

> **Audience:** anyone running `mununu btor2 verify-recoverability` or `btor2 verify-safety` (and the
> API peers) on a BTOR2 file in which some `state` has **no `init` line** — hand-written designs,
> btor2tools / HWMCC inputs, non-mununu frontends. **`sv verify-auto` with reset gating (the default)
> is not affected**: it injects an `init` for every state cell before any engine runs.
>
> **Related:** the exact-engine half of the same defect shipped in September
> ([2026-09-free-init-state-semantics](2026-09-free-init-state-semantics.md), mununu#498/#502/#505).
> This is the predicate-cube half. Found 2026-10-06 while calibrating profiling cases.
>
> **TL;DR:** when the exact engine abstains (cone over the cap, or an OxiDD arena exhaustion) and
> the recoverability or safety *cube* path takes over, that path built its "reset cube" by pinning
> every un-`init`ed register to **0**. It then reported a definite verdict for one initial state
> out of many. On a design where `AG EF (done == 1)` is VIOLATED from a free `a ≠ b` start, it
> reported **HOLDS**. A register with no `init` is now treated as **free at cycle 0**: the
> predicate bits that depend on it are enumerated, every flavour is a real initial state, and the
> verdict is HOLDS only if every flavour holds, VIOLATED if any flavour is violated, ⊥ otherwise.
> **No definite verdict on a fully-initialised design moves.**

## What was wrong

`concrete_oracle::init_valuation` returned a value for *every* state, `unwrap_or(0)` for a state
without an `init` line ("the `setundef -zero` power-up"). Both cube paths in
`adapter/recoverability.rs` read it to evaluate their predicates at "the" reset state and then
reported `final_verdict.verdict_at(init_cube)` as the property's verdict. That is one initial
state of the design's many. The paths even carried a guard for the case — "abstain if any final
predicate's register lacks a pinned reset value" — but the guard could never fire, because the
defaulted map had an entry for every register.

Reproducer (in-repo, `recoverability::tests::free_init_registers_are_enumerated_not_pinned_in_the_recoverability_reset_cube`):
two held 6-bit registers with no `init`, `done` latched on `a == b`, `AG EF (done == 1)`.

| path | before | after | correct |
|---|---|---|---|
| exact engine (`exact_symbolic_verdict`) | VIOLATED | VIOLATED | VIOLATED |
| scalable ladder (`verify_recoverability_scalable`) | **HOLDS** | VIOLATED | VIOLATED |
| safety cube (`verify_safety_scalable`, `bad = (x == 3)`, `x` free) | **HOLDS** | VIOLATED | VIOLATED |

How it surfaced: on a wider instance the exact engine ran out of OxiDD arena (an `Err` the
recoverability driver swallowed without a trace), the ladder took over, and `verify-recoverability`
printed `holds`. The arena was a trigger, not the cause; forcing the exact engine to skip on the bit
cap reproduced the HOLDS with no arena involved.

## What changed

- `init_valuation` now returns **only** registers that have an `init` line. A new
  `init_valuation_defaulted` keeps the total valuation for the concrete simulation seed and flags
  that it defaulted; a bounded reachability from such a seed is marked `bounded` and can no longer
  conclude `Holds` (that oracle is test-only today).
- Both cube paths collect the predicate bits whose registers are free at reset and enumerate them
  through one helper, `reset_cube_verdict`: free-init flavours are trusted in **both** directions
  (each is a real initial state); array-content (`select`) flavours keep their existing
  HOLDS-only unanimity rule (the free content over-approximates the initial set). More than four
  free bits abstain.
- The exact engine's abstention reason is now logged (`tracing::warn!`, target
  `mununu::recoverability`) before the ladder runs, so a report that came from the ladder can be
  traced to why.

## Who is affected

| Path | Effect |
|---|---|
| `sv verify-auto`, reset-gated (default) | **None** — every state cell gets an `init` before the model is built |
| `sv verify-auto --no-gate-reset` on RTL whose lift leaves states un-`init`ed | verdicts may move; see direction |
| `btor2 verify-recoverability` / `verify-safety` / API peers on files with free-init states | verdicts may move; previously a HOLDS could be fabricated, now it needs every initial flavour |
| Fully-initialised BTOR2 | **None** — identical model and verdicts |

**Direction of change:** `Holds → Violated` or `Holds → Unknown` only. No `Violated` becomes
`Holds`; no definite verdict on a fully-initialised design moves. A `Holds` that moves was never
established.

## Test the transition

```bash
# a free-init design: the ladder must agree with the exact oracle
MUNUNU_BDD_MAX_BITS=1 mununu --quiet btor2 verify-recoverability design.btor2 --target "done == 1"
#   before: "holds"   after: "violated" (or "unknown" if more than four predicate bits are free)
cargo test -p mununu-core --lib -- free_init reset_cube_verdict_trusts
```

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | none — no toolchain or dependency change | No |
| `mununu-sva` | carries the binary; rebuild to pick up the fix | Yes, on next adoption |
| `mununu-profile` (new, optional) | derives from `mununu-dev` | No |

## Not covered here

- The exact engine's OxiDD arena exhaustion on representation-bound cones (`a == b` under the
  cell-major order fills 83% of the arena at 11 bits) is a performance wall, not a verdict defect;
  it is tracked in the engine performance roadmap.
- Enumerating more than four free reset bits (abstains today).
- `exact_symbolic_verdict`'s `Err` still carries no structured cause through `verify-recoverability`'s
  JSON; only the log line was added.

Provenance: fix on `main` (uncommitted at the time of writing; trailer `Repro: in-repo
free_init_registers_are_enumerated_not_pinned_in_the_recoverability_reset_cube`), policy
[`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
