# Consumer briefing — 2026-09 a `⊥` now says WHY, and `0` means DISABLED on every budget knob

> **Audience:** monono, ROSF, anyone parsing `sv verify-auto` output or setting `MUNUNU_BDD_*` budgets.
>
> **Related:** monono **ask 26**. Mirror of [mununu#548](https://github.com/Mumunu-team/mununu/issues/548); complements [mununu#536](https://github.com/Mumunu-team/mununu/issues/536), which gave a `⊥` its *cell count*. Design record: [`docs/design/bdd-variable-ordering.md`](../design/bdd-variable-ordering.md).
>
> **TL;DR:** two changes, both additive. **(1)** Every `⊥` property now carries a structured **`bottom_reason`** — on the CLI and in the API — and for a resource abstain it is the **engine's own message, naming the budget and the knob to raise**. **(2)** `0` now means **DISABLED** on every `MUNUNU_BDD_*` budget; it previously meant three different things across four knobs, and on `MUNUNU_BDD_ITER_BUDGET` it meant *zero iterations — abstain immediately*. **No verdict changes.**

## ⚠️ Action item: check whether you set `MUNUNU_BDD_ITER_BUDGET=0`

If you do, its meaning has changed — for the better, but it *has* changed:

| | before | after |
|---|---|---|
| `MUNUNU_BDD_ITER_BUDGET=0` | **zero iterations — abstain on step one** | **no iteration bound** |
| `MUNUNU_BDD_FIXPOINT_ITERS=0` | the default (5,000), while its own doc said `0` disabled it | **no latency bound** (matches the doc) |
| `MUNUNU_BDD_FIXPOINT_NODES=0` | disabled | unchanged |
| `MUNUNU_BDD_TIME_BUDGET_MS=0` | disabled (and the default) | unchanged |

`.unwrap_or(default)` fired only on a *parse failure*, so `"0"` parsed fine and became the budget. A consumer set it expecting "disabled", got "zero iterations", and their failure count went **2 → 4 of 16** with new bottoms at 32 and 2 cells.

**"Disabled" is never unbounded in practice.** The arena-safety net still applies — exceeding the arena is a crash, not a budget question — so a run with every budget disabled can still abstain on `ARENA-SAFETY`. Raise `MUNUNU_BDD_ARENA_NODES` for that one.

**An unparseable value now WARNS** and says it is a config error rather than being absorbed into the default. `1_000_000`, `1e6`, `10MB` and `-1` were all previously silent.

## What `bottom_reason` gives you

**CLI** — on the property's own lines, so a harness that filters *notes* still sees it:

```
  [assert] video_timing_sva_1: unknown
        formula: AG (...)
        bottom-reason [engine-did-not-complete]: engine `exact-symbolic` did not
          complete on this design: symbolic bit-blaster: abstained on the ITERATION
          budget (1048577 > 1048576) — raise MUNUNU_BDD_ITER_BUDGET
```

**API** — `properties[].bottom_reason`, as `BottomReasonView`:

| field | meaning |
|---|---|
| `kind` | stable kebab tag — **branch on this** |
| `detail` | human-readable; for a resource abstain it is the engine's verbatim message, naming the budget and its numbers |
| `engine` | portfolio label, for `engine-did-not-complete` only |

### 🔴 Gate on `kind`, not on the outcome

```
safety-shape-not-reducible   the property's SHAPE is outside the rescue lane.
                             A BIGGER BUDGET CANNOT HELP. Needs a reshape or a
                             new reducer. Retrying with raised budgets loops forever.
no-state-model-non-safety    the design has zero state registers — a modelling
                             issue, not an engine cap.
engine-did-not-complete      a resource abstain. `detail` names the knob. Retry
                             with that knob raised.
unclassified-bottom          cause not established. Do NOT treat as
                             distinct-from-abstain without further evidence.
```

A gate that retries every `unknown` with bigger budgets will spin indefinitely on the first two. That is the whole reason `kind` is a separate machine-readable field rather than prose.

### Read the attribution precisely

`engine` and `detail` describe a **whole-run** failure of that engine over the design. They do **not** assert that this property caused it. What is claimed: *this property is ⊥, and the engine most likely to have decided it stopped for the stated reason.*

Conflating those is [mununu#548](https://github.com/Mumunu-team/mununu/issues/548) — a note describing one engine inherited by properties another engine decided — and this change deliberately does not repeat it. A property another engine **decided** carries no `bottom_reason`, however loudly a higher-precision engine failed.

## Why a field and not a note

A `bottom-reason` **note** has existed since mununu#492. It never reached the reader it was written for, because monono's formal lane discarded notes wholesale — while the engine's own abstention string was being dropped at the portfolio merge under the comment *"Errors contribute nothing."*

**A consumer that does not know about a field cannot silently filter it out. A consumer that does not know about a note routinely does.** Routing the budget name into the existing note machinery would have looked fixed upstream, changed nothing downstream, and closed the ticket — a two-repository silent failure neither side's tests would have caught. (monono-8d identified that trap before it was built.)

One consequence: the `unclassified-bottom` note used to advise *"check for a sibling `abstained on the …` note on the same property."* That advice was **unactionable** — the note it pointed at did not exist, because the reason had just been discarded. It resolves now.

## Report-parsing impact

**Additive only.** No verdict value changes, nothing removed, no CLI flag added or removed. New: one optional field on each property (CLI lines + API `bottom_reason`), and two new stderr warnings (unparseable budget, unrecognised variable order).

## Verification

- `a_failing_engines_budget_name_survives_the_merge` — the budget name, the knob, and the engine all survive the portfolio merge.
- `a_decided_property_never_inherits_a_failing_engines_reason` — the #548 direction, held for the field.
- `zero_means_disabled_on_every_budget_knob` + an unset-value control + an unparseable-value case.
- `the_api_view_carries_the_bottom_reason_and_its_gateable_tag` — the drift detector pins the field's *shape*; this pins that the *conversion* fills it.
- JSON schema regenerated; 17 drift tests pass.

## Docker rebuild table

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | new report field + budget semantics | **Yes** |
| mununu `Dockerfile.dev` | binary bump | **Yes** |
| mununu `Dockerfile.sva` | binary bump; e2e runs here | **Yes** |
| mununu `Dockerfile.extract`, `.extract-*` | no verify-auto path | No |
| rosf | consumes verdicts; verdicts unchanged | **No** (rebuild to read the new field) |
| monono Docker | formal lane parses this output | **Yes** — and check for `ITER_BUDGET=0` |
| mununu-ui | wire format grew an optional field; no type change required | No |

## For monono

Ask 26 is closed at the emitting end. Three things:

1. **The reason is a field on each property**, so you do not need the transcript capture you built this afternoon in order to see it — though keep it, since it is the thing that makes an *unmatched* note recoverable, which this change does not address.
2. **Branch your lane on `kind`.** `safety-shape-not-reducible` will never yield to a bigger budget.
3. **Check `MUNUNU_BDD_ITER_BUDGET`.** If any lane sets it to `0`, that used to abstain immediately.

For `video_timing sva_1`, the direct diagnostic still works and does not need this build: `mununu btor2 verify` calls the engine with no portfolio merge, so the abstention string was always visible there. With this change it is visible through `sv verify-auto` too.

## Not covered here (follow-ups)

- **A note the consumer's pattern does not match is still only recoverable from a transcript.** This change adds a field; it does not make arbitrary notes structured.
- **No per-engine timing.** You cannot attribute a run's wall time to a particular portfolio engine, which is how "170 s means the exact engine did work" became an unfounded assumption on both sides of one conversation.
- **`approx_num_inner_nodes` still counts allocated-including-dead nodes**, so at GC equilibrium it reports the arena rather than the diagram. Do not size anything off it.
- **`PropertyVerdict` has no `Default`**, so a new construction site defaults to *no reason given* by omission rather than by choice. 38 sites currently pass `None` explicitly.
