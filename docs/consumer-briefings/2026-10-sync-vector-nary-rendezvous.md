# Consumer briefing — 2026-10 a synchronisation vector split across two partner automata now fires whatever the members' order or names

> **Audience:** anyone composing CTXDSL automata — `mununu verify` (`verify.toml` compositions), `mununu context eval` with a `composition { … }` block, and the API peers — where a multi-label transition (`on {label a, label b}`) has its labels carried by **two or more other members**. Shinro's framework/LeKiwi models are the known case; single-partner vectors and single-label models are untouched.
>
> **Provenance:** [mununu#589](https://github.com/Mumunu-team/mununu/issues/589); fix in `crates/mununu-core/src/composition/mod.rs` (`CompositionOptions::sync_vectors`, the rendezvous rule in `compose`) and `context_dsl/realize.rs` (the collection before the fold). Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## TL;DR

A synchronisation vector `{a, b}` on automaton `A`, with `a` on `J` and `b` on `L`, is a three-party rendezvous. The product is folded pairwise, and until now the vector fired **only if `A` was folded before `J` and `L`** — on the `verify` path, only if `A`'s name sorted first. Otherwise the vector's target state was silently absent from the product: `reachable(target)` reported `VIOLATED`, and every safety property over the missing states held vacuously. Now the composition collects every member's vectors before the fold and lets two partners take their joint step; the result no longer depends on order or names.

**What moves:** on an affected model, states that were unreachable become reachable. A `reachable` that was `VIOLATED` becomes `SATISFIED`; a safety property that held *vacuously* over the missing region is now evaluated over it and may become `VIOLATED` — that is the sound reading the model always meant. Unaffected models (no vector whose labels sit on two other members) produce byte-identical products.

## What to update, per consumer

### Anyone with a multi-partner vector

- Re-run the lane. Expect `reachable` targets behind the vector to flip to `SATISFIED`; expect any safety property that was green over the vacuous region to be re-decided. The issue's reproducer (three sources, `members = ["a","j","l"]`, owner renamed `A` → `Z`) is the test `x589_a_vector_split_across_two_partners_fires_whatever_the_fold_order`.
- The workaround — naming the owner to sort first — is no longer needed and can be removed; it did nothing wrong, it just made the result order-dependent.
- The `[mununu#570]` dead-compound-action warning is unchanged: a vector no partner can ever match still warns.

### monono / ROSF

- No RTL path is involved; `sv verify-auto` and the BTOR2 verbs do not compose CTXDSL automata.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu` (prod) | composition results on multi-partner vectors | **Yes** |
| `mununu-dev` | test image; carries the new tests | **Yes** |
| `mununu-sva` | extends `mununu-dev` | **Yes** |
| `mununu-sva-pono` | extends the above | **Yes** |
| `rosf` (runtime) | bundles its own mununu; no CTXDSL composition in its lanes | No |
| `rosf-dev` | no mununu inside | No |
| `hw-verif` | Verilator only, no mununu | No |

## Test the transition

```bash
# 1. The mechanism, owner first / last / named to sort last, and the control (no owner ⇒ no joint step).
cargo test -p mununu-core --lib -- x589_

# 2. Your own model: the product's state count with and without the owner renamed must now agree.
mununu verify --quiet verify.toml --print-alphabet
```

## Not covered here

- Modality-guard label SETS (`[labels = {a, c}] φ`) are all-of (a vector match), not any-of — a separate issue (mununu#590) with its own change.
- Vectors inside a *synchronous* composition were never affected (every step is joint there).
