# Consumer briefing — 2026-10 environment assumptions on the `verify` path: `[[assumptions]]`, the fairness templates, and conditional verdicts with a non-vacuity gate

> **Audience:** anyone verifying hand-written CTXDSL compositions through `verify.toml` (`mununu verify`, `POST /api/v1/verify`, the UI's verify flow) — the Shinro-style protocol models first, and the `verify` consumers that parse `PropertyVerdict`.
>
> **Related:** closes [mununu#595](https://github.com/Mumunu-team/mununu/issues/595). Builds on the `F` = diamond clarification of [#593](https://github.com/Mumunu-team/mununu/issues/593) (briefing [`2026-10-guard-label-sets-and-declared-alphabet.md`](2026-10-guard-label-sets-and-declared-alphabet.md)). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).
>
> **TL;DR:** **new capability + additive wire-shape change.** `verify.toml` gains `[[assumptions]]` (named environment fairness constraints: `state` / `edge` / `edges` / `weak`) and `[[properties]].assumptions`; two templates take them — `fair_response(TRIGGER, RESPONSE)` and `fair_always_eventually(TARGET)` — and expand to the fair-`EG` encoding the issue shipped as a reference. A verdict under assumptions is **conditional**: `PropertyVerdict` carries `assumptions` and `fair_path_exists` (the gate `E_C G true`), the CLI prints `SATISFIED under {…}` / `VIOLATED under {…}` / **`VACUOUS under {…}`**, and `--strict` fails on vacuous. Nothing changes for a property without assumptions.

## What you can write now

```toml
[[assumptions]]
name = "env_progress"
kind = "edge"              # "state" | "edge" | "edges" | "weak"
atom = "ack"               # the label (edge, weak) or the state predicate (state); `labels = [...]` for edges

[[properties]]
name = "request_served"
template = "fair_response"
args = { TRIGGER = "Requesting", RESPONSE = "Served" }
assumptions = ["env_progress"]
```

| `kind` | means | encoding (one conjunct of `E_C G q = νZ.(q ∧ ⋀C)`) |
|---|---|---|
| `state` | `GF P` | `◇ μY.((Z ∧ P) ∨ (q ∧ ◇Y))` |
| `edge` | `GF ⟨l⟩` | `μY.(⟨l⟩Z ∨ ◇(q ∧ Y))` |
| `edges` | `GF ⟨l₁⟩ ∨ … ∨ ⟨lₙ⟩` | `μY.(⟨l₁⟩Z ∨ … ∨ ◇(q ∧ Y))` |
| `weak` | weak fairness of `l` | `μY.(⟨l⟩Z ∨ ◇(Z ∧ ¬⟨l⟩true) ∨ ◇(q ∧ Y))` |

`fair_response` = `AG(TRIGGER → A_C F RESPONSE)`, `fair_always_eventually` = `AG(A_C F TARGET)`, with `A_C F p = ¬E_C G ¬p`. The reference encoder's twelve known-answer cases (the issue's `sanity_fair.ctxdsl`, both vacuity traps included) are the in-tree test of `verify::fairness`.

## What the verdict says

| CLI | `satisfied` | `fair_path_exists` | read it as |
|---|---|---|---|
| `SATISFIED under {a, b}` | `true` | `true` | holds on every path satisfying `a` and `b`; such paths exist |
| `VIOLATED under {a, b}` | `false` | `true` | a fair path refutes it (the genuine violation) |
| **`VACUOUS under {a, b}`** | `true` | **`false`** | the assumptions admit **no** fair path from the initial state(s); the formula is true for nothing. **Not a pass**: `--strict` fails, and a gate should too. |
| `SATISFIED` / `VIOLATED` | — | absent | an unconditional property; unchanged |

The gate is `E_C G true` at the initial states, evaluated alongside the property on the same target. It catches the trap the issue named: `GF ⟨go⟩` on a model where `go` fires at most once is unsatisfiable, and `A_C F Goal` holds there for nothing.

**Rules the validator enforces.** Assumption names are unique and declared; only the two fairness templates take `assumptions`, and they require at least one (with none they are `response` / `always_eventually` — use those). An inline `formula` encodes its own fairness (the conjuncts above are plain mu-calculus).

**A quantifier worth noticing.** `always_eventually` is `AG EF` (the existential reading this tool gives `F`); `fair_always_eventually` is `AG A_C F` — universal over the fair paths — because "under a fair environment the system keeps recovering" is a statement about every fair path, not one.

**`ltl (GF a) -> (GF b)` is still not the path implication.** The translator is state-wise; the LTL page now says so and points here.

## What to update, per consumer

### `verify` report parsers (CLI JSON, `POST /api/v1/verify`)

- Two new optional keys on each `property_verdicts[]` entry: `assumptions` (array of names; absent when empty) and `fair_path_exists` (bool; absent when unconditional). Additive; a parser that ignores unknown keys is unaffected.
- A gate that reads `satisfied` alone will count a VACUOUS verdict as a pass. Read `fair_path_exists == false` as a failure.

### `verify.toml` authors

- Nothing changes without `[[assumptions]]`. A config that already used `assumptions` as a property key for something else (none in-tree) would now be validated.

### mununu-ui

- `VerifyAssumption`, `VerifyProperty.assumptions`, `VerifyPropertyVerdict.assumptions` / `fair_path_exists` in `types.ts`; the verdict table renders `under {…}` and VACUOUS. Shipped in the companion mununu-ui PR.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | new config surface, new report fields | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The encoder's known answers and the verify path end to end:
cargo test -p mununu-core --lib --features api -- verify::fairness x595_

# 2. A model of yours: declare the environment's progress and compare the two readings.
mununu verify --strict verify.toml      # exits non-zero on VIOLATED and on VACUOUS
```

## Not covered here

- A CTXDSL `assume { }` block (assumptions inside the model file) — the surface here is `verify.toml`; `context eval` users encode the conjuncts by hand or move to `verify`.
- Strong fairness (compassion, `GF p → GF q` pairs) and Streett conditions — not expressible with these kinds; a `weak` entry per label and `edges` cover the common environment-progress cases.
- The BTOR2 verbs' fairness (`verify-liveness-under-fairness`, the `--fairness` latch, GR(1) on the game path) are unchanged and separate.
