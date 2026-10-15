# `v10_opentitan_nested_count_recoverability` — nested-counter recoverability (lexicographic ranking)

> **Status: PASS.** A NESTED counter built from two real OpenTitan `prim_count`
> instances (the prescaler+counter pattern): an inner counter down-counts each tick and
> reloads at zero; when it reloads, the outer counter decrements. Asks whether the outer
> counter can always get back to zero — `AG EF (outer == 0)` — and decides `Holds` at
> 48 bits via the **lexicographic** ranking `(outer, inner)`, where a single-register
> ranking, the predicate cube, and the exact BDD engine all give out. Completes the
> real-RTL ranking trio: v8 (relational), v9 (single-register), v10 (lexicographic).
> §Phase 10 V-track recoverability showcase.

> Source of truth: [`verify_recoverability`](../../../crates/mununu-core/src/adapter/recoverability.rs) — surface: CLI (`mununu btor2 verify-recoverability --target 'outer == 0'`)

> **Claims integrity.** `prim_count.sv` and `prim_count_pkg.sv` are real OpenTitan RTL
> (Apache-2.0), pinned under [`source/`](source/) at the commit in
> [`source/UPSTREAM_COMMIT.txt`](source/UPSTREAM_COMMIT.txt). `prim_flop.sv` is a minimal
> register matching OpenTitan's abstract-prim flop interface; `count_nested_top.sv` is a
> nested-counter harness of two real `prim_count` instances (the prescaler+counter usage
> — a pattern real timers use). A property-class demonstration on real silicon.

## Why a single-register ranking is not enough

The outer counter decrements only when the inner counter reloads. So `outer` **holds**
for a whole inner cycle before it steps — it does not strictly decrease every tick, and
is therefore **not a valid ranking function** on its own. The property still holds (tick
long enough and the outer counter reaches zero), but proving it needs a measure that
decreases *every* tick.

## What decides it — the lexicographic ranking

mununu's ranking certificate generalizes past a single register difference to
**lexicographic tuples**. It pairs the outer distance with the inner counter as a
tiebreaker, `(outer, inner)`, which decreases on every tick:

- inner tick (inner > 0): `(outer, inner−1)` — same outer, smaller inner;
- inner reload (inner == 0): `(outer−1, INNER_MAX)` — smaller outer (dominates regardless of inner).

Lex order on `(outer, inner)` is well-founded, so the descent reaches `outer == 0` — a
*sound* `Holds`, decided over the exact transition in one SMT query per candidate tuple.

| Setup | `always_recoverable` | How |
|---|---|---|
| **Small** (`OW=8`) | **`Holds`** | exact engine (`engine: exact`) |
| **Wide** (`OW=48`) | **`Holds`** | exact engine (`engine: exact`, measured 2026-10-14); the **lexicographic** ranking certificate is the decider past the exact engine's reach |

> **Re-measured 2026-10-14** on the release binary (`engine` field, mununu#682): both rows are decided by the **exact engine** directly — the cap has auto-raised to fit the cone up to 192 bits since mununu#402, so these widths no longer leave it. The ranking certificate described below remains the engine of record for a cone the exact engine abstains on; this example no longer exercises it. See `docs/verifying-rtl.md` → "What decides at what width".


## Run it

```bash
LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli
./examples/verify/v10_opentitan_nested_count_recoverability/validate.sh
```

Requires `sv2v`, `yosys`, and `python3` on `PATH` (the plain-SV → BTOR2 flow, as v7–v9).
`validate.sh` names the outer counter (the state in `outer_o`'s cone) `outer` and targets
`outer == 0`.
