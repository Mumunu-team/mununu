# Consumer briefing — 2026-09 a timed-out may-enumeration could produce a false `holds`

> **Audience:** anyone running `mununu sv verify-auto` or `btor2 cegar` on the default (explicit / portfolio) path. monono especially — this sits on the same code path as the hang you reported.
>
> **Related:** [mununu#504](https://github.com/Mumunu-team/mununu/issues/504).
>
> **⚠ TL;DR — this is a soundness fix.** When a z3 query in the may-edge enumeration returned **inconclusive**, the partial result was stored as if complete. That under-approximates the `may` relation, which can make a `[]`/`AG`-shaped property report **`holds` when it does not**. Verdicts change in one direction: **`holds` → `unknown`**. If you have acted on a `holds` from a large or wide design, it is worth re-running.

## What was wrong

`compute_all_may_edges_smt_postimage` enumerated next-state valuations with:

```rust
while matches!(solver.check(), z3::SatResult::Sat) { … }
```

`Unknown` and `Unsat` were treated identically — both exited the loop. But only **`Unsat` is a proof that the enumeration is complete**. On `Unknown` (a z3 timeout, or a resource-limit hit) the partial target list was stored as the finished answer.

**Why that is unsound.** `may` is an *over*-approximation: correctness requires may ⊇ concrete. A truncated may is an *under*-approximation. `[]φ` is True when **all** may-successors satisfy φ — so dropping successors makes a box property **easier** to hold, and can manufacture a definite `holds` that the design does not have.

This was reachable in normal operation: the path carries a 5-second per-query timeout, and a wide combinational cone can exhaust it partway through an enumeration.

## What changed

The three results are now distinguished:

| z3 result | Before | After |
|---|---|---|
| `Sat` | record target, continue | unchanged |
| `Unsat` | stop | stop — enumeration provably complete |
| **`Unknown`** | **stop, keep partial** ❌ | **saturate this cube: every target is a may-successor** ✅ |

Saturation is the sound direction — denser `may` ⇒ more ⊥, never a wrong verdict — and it is **local**, so one starved cube costs precision only there.

## Direction of change

- **`holds` → `unknown`** where a truncated enumeration had been propping up a box property.
- `violated` is unaffected: a definite `[]φ = False` needs a *must*-successor, and must-edges are not touched.
- Designs where no query ever returned `Unknown` are **bit-identical**.

## How likely was this to fire? — stated honestly

**I have not measured it.** What is established:

- the truncation is real and reachable on the **default** path (`may_postimage` is set unconditionally, and the post-image is used for `|P| ≥ 2`);
- it is pinned by a regression that **fails on the pre-fix code**.

What is **not** established is how often a z3 query actually returned `Unknown` on real designs. It requires a query to exhaust its budget mid-enumeration, which is a wide-cone / large-design phenomenon — so a small design almost certainly never hit it, and a large one may have. If a `holds` moves to `unknown` for you, that property was in this class.

## Also in this change

`MUNUNU_CUBE_SMT_RLIMIT` now applies to the post-image path too. It was already applied by every check in the must-edge module but was missing from this one — which is the single place in the lift that builds its own solver parameters, and the measured suspect behind the six-hour hang in #504.

**Unset by default ⇒ no behaviour change.** Set it, and a grinding enumeration becomes a fast deterministic result instead of a hang:

```bash
MUNUNU_CUBE_SMT_RLIMIT=2000000 mununu sv verify-auto …
```

This is usable today as a mitigation while the rest of the #504 budget work lands. It is only safe *because* of the saturation fix above — without it, an rlimit (whose purpose is to make queries return `Unknown`) would have turned a rare silent truncation into a routine one.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | soundness fix; verdicts can move `holds` → `unknown` | **Yes** |
| mununu `Dockerfile.dev` | binary bump | **Yes** |
| mununu `Dockerfile.sva` | binary bump | **Yes** |
| mununu `Dockerfile.extract`, `.extract-*` | no lift path | No |
| rosf | consumes verdicts | **Yes** |
| monono Docker | same path as the reported hang; pins verdicts | **Yes** |
| mununu-ui | no type change | No |

## Verification

```bash
cargo test -p mununu-core --lib -- postimage_saturates
```

Forces every query inconclusive with `rlimit = 1` — deterministic, machine-independent, ~100 ms, no slow input needed — and asserts the full grid comes back. It carries a control assertion that the unstarved relation is strictly sparser, without which the test would prove nothing.

## Not covered here (follow-ups)

- **The rest of #504.** There is still no wall-clock budget at any granularity, and output remains all-or-nothing, so a killed run still emits zero bytes. That is the remaining ladder.
- **The `targets.len() > 2^n` safety break** in the same loop is also a truncation, but it is unreachable in principle (each valuation is blocked, so at most 2^n iterations occur). Left as-is, not audited further.
