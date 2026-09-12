# Consumer briefing — 2026-09 an engine budget abstention is `unknown`, not `skipped` (and an OOM no longer panics)

> **Audience:** monono (reported it — ask 25), ROSF, and any consumer that reads `sv verify-auto` outcomes or gates on its exit code.
>
> **Provenance:** [mununu#542](https://github.com/Mumunu-team/mununu/issues/542). Extends [mununu#504](https://github.com/Mumunu-team/mununu/issues/504)'s wall-clock rule to the engine's other budgets. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## ⚠ TL;DR — this can turn a GREEN gate RED, and that is the fix

Two changes, one root:

1. **A bit-blaster OOM used to PANIC** — exit 101, no report at all. It now abstains.
2. **Every engine budget abstention used to arrive as `skipped`.** It is now `unknown`.

`ci_exit_code` fails on `unknown` and never on `skipped`. So a strict `--fail-on unknown` gate that was **passing** on a property the engine had explicitly abstained on will now **fail**. That is correct: the property was never decided. If a gate goes red on this upgrade, the property was not being verified before either — you are seeing it for the first time, not breaking it.

## What changed

### 1. The OOM panic

```
thread 'main' panicked at crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs:1094:68:
called `Result::unwrap()` on an `Err` value: OutOfMemory
```

Exit 101, **no report**. monono's report: the design verifies under the same pins; a twin adds one register and the cone stops fitting.

The site was the `MustSemantics::ForallExists` must-edge `∃i` existential in `abstract_relation_impl`. Every cone-sized BDD operation in that function now propagates through the existing `oom` helper instead of `unwrap`-ing. The only `unwrap`s left there allocate a single fresh variable each — O(1), at function entry — and carry a comment saying so.

**Why it panicked there specifically, and not elsewhere:** the engine's node-budget guard is a *start-of-op* check inside `eval_op`, and the arena is deliberately sized **above** the budget so the guard fires first and returns cleanly. The must-relation construction calls OxiDD directly (`exists` / `not` / `or` / `forall`) and never passes through `eval_op`, so it had no guard and no propagation.

**Why a panic here was so damaging:** a panic while an OxiDD manager is exhausted aborts on the unwind — `catch_unwind` cannot save it. So this could surface as exit 101 *or* exit 134.

### 2. The misclassification (the wider half)

`symbolic_engine::ir_err` stamped `IrConsistencyError` on **every** engine error, including the deliberate abstentions:

- `abstained on the NODE budget`
- `abstained on the ITERATION budget`
- `abstained on the WALL-CLOCK budget`
- `abstained on the BIT CAP`

`verify_auto`'s generic error arm maps any non-budget error to `Skipped`. So the engine said "I abstained" and the report said "not attempted" — and no CI gate fails on `skipped`.

`AdapterErrorKind::ResourceBudgetExceeded` and its `→ Unknown` mapping already existed (#504) — **nothing on the symbolic path ever produced that kind.** A new `classify_engine_error` closes it. The special case stays narrow: a genuine defect (an unsupported operator, a failed intern) keeps its kind and still maps to `skipped` with its message.

## What to update, per consumer

### monono

- **Expect new `unknown`s.** Any property whose engine abstained on a budget was reported `skipped` and is now `unknown`. Re-run and re-pin: `--expect NAME=unknown` is the sound way to record one.
- **The six dead twins.** Your `expect_violated` reported `"1 violated as required"` on a run that produced no verdict lines. After this change that run yields a parseable report with `unknown`, so the comparison has something to iterate over. **The harness lesson stands independently:** a run producing zero verdict lines should fail your gate regardless of exit code — belt and braces.
- **`MUNUNU_VERIFY_AUTO_PARTIAL_JSON` already does what you need — verified, no change required.** It flushes at the **top** of each property iteration, before that property's (possibly fatal) work. A death during property *N* leaves records for `0..N-1` on disk. Read the **last** record per property name.
- **Your exit-134s are tracked separately** at [mununu#543](https://github.com/Mumunu-team/mununu/issues/543) and are **not** closed by this change. Your hypothesis that they share a root is plausible and now cheaper to test: if raising `MUNUNU_BDD_ARENA_NODES` removes them, they were this bug; if raising `RUST_MIN_STACK` removes them, they are genuine recursion. One knob at a time, ≥3 runs each given the reported flakiness.

### ROSF

- The `mununu` lane maps `holds | violated | unknown | skipped` 1:1, so **no code change** — but the *distribution* shifts: fewer `skipped`, more `unknown`. Any dashboard or threshold keyed on `skipped` counts will move.
- rosf already treats a timeout as `unknown` rather than `skipped` for exactly this reason, so the semantics now agree end to end.

### Report-parsing impact

No shape change. `PropertyVerdict` fields, the JSON schema, and the outcome vocabulary are all unchanged — only which of the four a budget abstention lands on. No schema regeneration was needed and the drift detector is untouched.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | verdict values: budget abstention `skipped` → `unknown`; OOM no longer exits 101 | **Yes** |
| `mununu-dev` | test/lint image; carries the new unit tests | **Yes** |
| `mununu-sva` | extends `mununu-dev`; same verdict change on the SVA path | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu; rebuild to pick up the verdict change | **Yes** |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. A budget abstention is now `unknown`, and the classification is locked in both directions.
cargo test -p mununu-core --lib -- adapter::tests::an_engine_budget \
  adapter::tests::a_real_engine adapter::btor2::symbolic_engine::tests::ir_err

# 2. Re-run any corpus that previously reported `skipped`, and diff the outcome column.
#    A property moving skipped -> unknown is this change. A property moving either of those to
#    holds/violated is NOT, and is worth reporting.
```

## Not covered here

- **[#543](https://github.com/Mumunu-team/mununu/issues/543)** — the exit-134 triage. Open.
- **[#544](https://github.com/Mumunu-team/mununu/issues/544)** — naming a property by its SVA label instead of its index. Open; ships separately and *will* change the report shape.
- **[#545](https://github.com/Mumunu-team/mununu/issues/545)** — a cutpoint that frees an array read's value but keeps its timing. Open; design work.
- **Forcing a real arena exhaustion from a unit test.** The arena is deliberately sized above the node budget so the guard fires first, which makes a genuine OxiDD `OutOfMemory` unreachable from a unit test. The propagation is therefore verified by inspection — no cone-sized `unwrap` remains in the function, and the two O(1) exceptions are annotated — plus the two classification tests above. **Stated because it matters: the no-panic property itself is not covered by an automated test.** The end-to-end evidence is monono's original report and the absence of the `unwrap`.
