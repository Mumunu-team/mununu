# `v9_opentitan_count_recoverability` — counter recoverability of a real OpenTitan counter (both directions)

> **Status: PASS.** Asks, in **both directions**, whether OpenTitan's `prim_count` can
> always get back to a known state: down to **zero** (`AG EF cnt==0`, a datapath
> **descent**) *and* up to **max credit** (`AG EF cnt==MAX`, a datapath **ascent**). It
> decides `Holds` for both properties on a small counter (exact) and a 48-bit counter
> (via the **ranking certificate**), where exact BDD walls and the predicate cube
> abstains. A single-register ranking in two directions on a real hardened primitive — the
> companion shape to
> [`../v8_opentitan_fifo_recoverability`](../v8_opentitan_fifo_recoverability)'s relational
> FIFO drain. §Phase 9 V-track recoverability showcase.

> Source of truth: [`verify_recoverability`](../../../crates/mununu-core/src/adapter/recoverability.rs) — surface: CLI (`mununu btor2 verify-recoverability --target 'cnt == 0'` / `--target 'cnt == MAX'`)

> **Claims integrity.** `prim_count.sv` and `prim_count_pkg.sv` are real OpenTitan RTL
> (Apache-2.0), pinned under [`source/`](source/) at the commit in
> [`source/UPSTREAM_COMMIT.txt`](source/UPSTREAM_COMMIT.txt). `prim_flop.sv` is a minimal
> register matching OpenTitan's abstract-prim flop interface (identical to
> `prim_generic_flop`); `count_top.sv` is a thin down-counter harness (the usage
> configuration — `step = 1`, no clear/set). A property-class demonstration on real
> silicon, not a vulnerability finding.

## The question — both directions

A counter that must not wedge raises a branching-time question: from any value it can
hold, can it always get back to a known state? `prim_count` answers **yes in both
directions** — it can always drain back to **zero** *and* always fill up to **max
credit** (`MAX = 2^Width − 1`). Both are recoverability, `AG EF (cnt == GOAL)`:

```
recoverable        = mu X. (cnt == GOAL || <> X)      # EF cnt==GOAL — goal reachable from here
always_recoverable = nu Y. (recoverable && [] Y)      # AG EF cnt==GOAL — ...from every value
```

with `GOAL ∈ {0, MAX}`. The `<>` inside the `[]` is the branching content a linear
formalism (LTL / SVA) cannot state.

## What decides it — the ranking certificate (both measure directions)

Reaching either goal is a **well-founded descent of a measure** — but the two goals need
measures pointing opposite ways:

- **descent to 0** — measure δ = `cnt` (each decrement lowers it, bounded below by 0);
- **ascent to MAX** — measure δ = `MAX − cnt` (each increment lowers it, bounded below by 0).

No bounded predicate set captures a 2^W-step descent, so the predicate cube abstains; the exact
BDD engine, whose cap auto-raises to 192 bits, decides both widths here directly (measured
2026-10-14, `engine: exact`). Beyond the exact engine's reach mununu's **ranking certificate** decides
each directly over the exact transition: from every off-goal state *some* input strictly
decreases the relevant δ, forcing a descent to the goal. The certificate **tries both
measure directions**, so the same extraction decides both properties — each `Holds` at
Width = 48.

| Setup | `AG EF cnt==0` (descent) | `AG EF cnt==MAX` (ascent) | How |
|---|---|---|---|
| **Small** (`Width=8`) | **`Holds`** | **`Holds`** | exact engine (`engine: exact`) |
| **Wide** (`Width=48`) | **`Holds`** | **`Holds`** | exact engine (`engine: exact`, measured 2026-10-14); the ranking certificate (∃-input variant, δ = `cnt` / δ = `MAX−cnt`) is the decider past the exact engine's reach |

> **Re-measured 2026-10-14** on the release binary (`engine` field, mununu#682): both rows are decided by the **exact engine** directly — the cap has auto-raised to fit the cone up to 192 bits since mununu#402, so these widths no longer leave it. The ranking certificate described below remains the engine of record for a cone the exact engine abstains on; this example no longer exercises it. See `docs/verifying-rtl.md` → "What decides at what width".


## A real-hardware wrinkle (and why the certificate is robust to it)

`prim_count` is **hardened**: it keeps a redundant *secondary* counter (for fault
detection) and an FPV backdoor. Lifted to BTOR2, those leave wide signals that never
touch the primary count's evolution. The ranking certificate enumerates only inputs in
the **counted register's own next-state cone**, so the secondary/backdoor signals are
left free (sound — they cannot affect the ranking) and do not swamp the search. This is
the kind of messiness real RTL brings that a synthetic counter never would.

## Run it

```bash
LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli
./examples/verify/v9_opentitan_count_recoverability/validate.sh
```

Requires `sv2v`, `yosys`, and `python3` on `PATH` (the standard plain-SV → BTOR2 flow,
same as v7/v8). `validate.sh` names the primary counter state (the one in `cnt_o`'s cone)
`cnt` and targets both `cnt == 0` (descent) and `cnt == MAX` (ascent) on one extraction.
