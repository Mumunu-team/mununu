# Consumer briefing — 2026-10 a modality guard's label set is any-of; a declared-only controllable label is in the alphabet; `ltl F` is documented as the diamond it is

> **Audience:** anyone writing mu-calculus formulas with `labels = { … }` guards of two or more labels, composing CTXDSL automata whose `controllable { }` block declares a label no transition uses, or reading verdicts of `ltl F` / `ltl G(p -> F q)` as "inevitable". Shinro's models are the known case for the first two; the third changes no verdict.
>
> **Provenance:** [mununu#590](https://github.com/Mumunu-team/mununu/issues/590), [#592](https://github.com/Mumunu-team/mununu/issues/592), [#593](https://github.com/Mumunu-team/mununu/issues/593). Code: `mu_calculus/mod.rs` (`guard_matches_labels_and_vars`), `mu_calculus/evaluator.rs` (the `[mununu#590]` warning), `composition/mod.rs` (the `[mununu#592]` warning), `clts/mod.rs` (`CltsBuilder::declare_in_alphabet`), `context_dsl/realize.rs`. Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR

Three places where the tool did one thing and the page said another. Two are fixed by changing the tool to what the page (and the textbook) say; one by changing the page to what the tool deliberately does.

| | before | after | verdicts move? |
|---|---|---|---|
| `[labels = {a, c}] phi` / `<labels = {a, c}> phi` | all-of: only an edge carrying **both** `a` and `c` (a vector match); vacuously true/false where no edge carries the whole set | **any-of**: every edge carrying `a` **or** `c`, as documented | yes, on multi-label guards (none in this repo's examples) |
| a label in `controllable { }` with no transition | not in the alphabet; a partner fired it freely | **in the alphabet**, so a partner that carries it is blocked (this automaton never offers it) | yes, on compositions with a declared-but-unused shared label |
| `ltl F phi` | documented as `mu X. (phi \|\| [] X)` "along every path" | documented as what it is: `mu X. (phi \|\| <> X)`, reachability (the Skolem reading); the inevitability spelling is given | no |

Two warnings arrive with the semantics: `[mununu#590]` when a guard names a label no transition of the model carries (the vacuity trap in its general form — a typo makes a box vacuously true), and `[mununu#592]` at composition time when a label controllable in one member is carried by another (it composes as uncontrollable, which flips realizability). Both are `tracing::warn!`; verdicts are unchanged by them.

## What to update, per consumer

### Formulas with a multi-label guard

- A guard written for the documented any-of meaning now evaluates that way; rewrites made to work around the all-of reading (`[labels={a}] phi && [labels={c}] phi`) are equivalent and can stay or be simplified.
- A guard that *relied* on the vector match (`labels = {a, b}` meaning "exactly the `{a, b}` edge") has no spelling now; a vector's enabledness is a transition property, not a modality. If you have one, say so on #590.

### Compositions with a declared-but-unused controllable label

- Re-run. A partner's step on such a label no longer fires; a `reachable` behind it flips to `VIOLATED`, and that is the model saying what the reference always said. If the label was declared by mistake, remove it; if the partner's move should be free, the label is not this automaton's to declare.
- A mutation that deletes an automaton's only edge on a shared label now *blocks* its partners (the mutation test's expectation) rather than freeing them.

### Anyone reading `ltl F` as inevitability

- Nothing changed in the tool. The LTL page now states the translation (`F`, `U` existential; `G`, `X` universal) with the box forms for CTL `AF` / `AG(p -> AF q)` to write when inevitability is meant. A property that must mean "on every path" should be rewritten to the box form; its verdict may then change — that is the stronger property, decided correctly.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | guard semantics, declared-label alphabet, two warnings | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu; no CTXDSL formulas in its lanes | No |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# the three mechanisms
cargo test -p mununu-core --lib -- x590_ x592_
# your formulas: any `[mununu#590]` line names a label the model does not carry
RUST_LOG=mununu_core=warn mununu context eval model.ctxdsl --formula <name> 2>&1 | grep 'mununu#590'
# your compositions: any `[mununu#592]` line names a controllable label a partner also carries
RUST_LOG=mununu_core=warn mununu verify verify.toml 2>&1 | grep 'mununu#592'
```

## Not covered here

- Environment assumptions (fairness) on the CTXDSL path — mununu#595, its own change.
- A declared-only label in `internal { }` is unchanged (internal actions are never shared).
