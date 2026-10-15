# `v8_opentitan_fifo_recoverability` — relational recoverability of a real OpenTitan FIFO

> **Status: PASS.** Asks whether OpenTitan's `prim_fifo_sync_cnt` can always drain
> back to **empty** — a recoverability (`AG EF`) question — but over a datapath
> **relation** (`wptr == rptr`), not a control-FSM state. It decides a definite
> `Holds` on both the small FIFO (exact) **and** a 2²¹-deep FIFO (via the ranking
> certificate), where exact BDD walls and the predicate cube abstains — a real
> OpenTitan datapath-branching property decided at scale. §Phase 8 V-track
> recoverability showcase (the relational-target companion to
> [`../v7_csrng_recoverability`](../v7_csrng_recoverability), which targets a
> control state).

> Source of truth: [`verify_recoverability`](../../../crates/mununu-core/src/adapter/recoverability.rs) — surface: CLI (`mununu btor2 verify-recoverability --target 'wptr == rptr'`)

> **Claims integrity.** `prim_fifo_sync_cnt.sv` and `prim_count_pkg.sv` are real
> OpenTitan RTL (Apache-2.0), vendored + pinned under [`source/`](source/) at the
> commit in [`source/UPSTREAM_COMMIT.txt`](source/UPSTREAM_COMMIT.txt). This is a
> **demonstration of a property class on real silicon, not a vulnerability finding**:
> the ranking boundary it surfaces is intrinsic to the property (draining is
> unbounded progress), not a design defect.

## The question

A FIFO that must not wedge raises a branching-time question: from any fill level it
can reach, can it always get back to **empty**? That is recoverability, `AG EF empty`.
But "empty" is not a control-FSM state — in `prim_fifo_sync_cnt.sv` it is a **relation
over two datapath registers**:

```systemverilog
assign empty_o = wptr_wrap_cnt_q == rptr_wrap_cnt_q;   // line 63
```

So the recoverability target is **relational** (`REG == REG`), decided by mununu's
compound-good machinery rather than a `REG == VALUE` control atom:

```
recoverable        = mu X. (empty || <> X)            # EF empty
always_recoverable = nu Y. (recoverable && [] Y)      # AG EF empty
        with  empty = (wptr_wrap_cnt_q == rptr_wrap_cnt_q)
```

The `<>` (some-successor) inside the `[]` (all-successors) is the branching content a
linear formalism (LTL / SVA) cannot state.

## What decides it — the ranking certificate

Reset is tied **inactive** in both runs, so recoverability rests on the *datapath* drain
(reads catching the write pointer), not a reset escape.

| Setup | `always_recoverable` | How |
|---|---|---|
| **Small FIFO** (`Depth=16`) | **`Holds`** | exact engine (`engine: exact`) |
| **Wide FIFO** (`Depth=2^21`, 22-bit pointers) | **`Holds`** | exact engine (`engine: exact`, measured 2026-10-14); the ranking certificate was the decider when the cap was 40 bits |

> **Re-measured 2026-10-14** on the release binary (`engine` field, mununu#682): both rows are decided by the **exact engine** directly — the cap has auto-raised to fit the cone up to 192 bits since mununu#402, so these widths no longer leave it. The ranking certificate described below remains the engine of record for a cone the exact engine abstains on; this example no longer exercises it. See `docs/verifying-rtl.md` → "What decides at what width".


`empty` (`wptr == rptr`) is **not invariant** — the two wrap counters advance
**independently** (writes vs. reads), so `wptr == rptr` is *achieved* by draining, a
well-founded descent. No bounded predicate set captures a 2²¹-step descent, so the
predicate cube alone abstains. mununu's **ranking certificate** decides it directly over
the exact transition (Podelski–Rybalchenko): for the ranking δ = `wptr − rptr`, a single
SMT query proves that from every non-empty state *some* input (a read) strictly decreases
δ; with δ bounded below, that forces a descent to empty — so `AG EF empty` **Holds**, in
~0.1 s at 2²¹ depth.

The `∃`-input form is what fits a FIFO: only *some* input drains it, so `AG EF empty`
holds but `AG AF empty` does **not** (the environment may write forever). The all-path
variant of the certificate correctly *fails* here and is reserved for deterministic
descents (down-counters, timers).

This is the value: mununu can *state and soundly decide* a relational branching-time
property — one SVA cannot express — on real RTL **at a scale where bit-level engines and
predicate abstraction both give out**, without over-claiming (it still soundly abstains
where the property genuinely needs a ranking argument no certificate finds).

## Run it

```bash
LIBRARY_PATH=/usr/local/opt/z3/lib cargo build -p mununu-cli
./examples/verify/v8_opentitan_fifo_recoverability/validate.sh
```

Requires `sv2v`, `yosys`, and `python3` on `PATH` (the standard plain-SV → BTOR2 flow,
same as `v7`). The two wrap-pointer counters are the only state; `validate.sh` names
the two anonymous state lines `cnta` / `cntb` (the `empty` relation is symmetric) and
targets `cnta == cntb`.
