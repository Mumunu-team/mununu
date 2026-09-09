# Consumer briefing — atoms over a memory are refused instead of silently mis-bound

> **Audience:** anyone naming signals in a hand-written `.mununu.json` sidecar's
> `compound_predicates`, in `--predicate`, or in the API's `predicate` field. **ROSF, monono.**
> Narrow scope — see below.

## TL;DR

An atom naming a **memory** (a BTOR2 array state, e.g. a register file's `mem`) used to be
admitted as a bit-vector cube dimension for which no bit-vector exists. It is now refused with a
new `array-atom-unsupported` verification note that names the memory.

**Not reachable from SVA**, so most consumers see no change at all.

## What was wrong

`verify_auto` built its set of BV state cells with no sort filter — every `Node::State`, including
arrays. A BTOR2 array state *is* a `Node::State` and carries a symbol, so a memory landed there
beside a scalar register and the "does this resolve to state?" test answered **true** for it.

But arrays are deliberately excluded from the BV signal vector: they are encoded as SMT `Array`
constants resolved by name through a separate map, and `bv_width` returning `None` is the
BV/array discriminator used everywhere else. So the atom was admitted as a cube dimension with
nothing to range over.

The fix filters state cells to BV-sorted states, matching that discriminator.

### Reproduced, not just reasoned about

The issue recorded the downstream consequence as *"inferred, not reproduced."* It is now
reproduced: with the sort filter removed, the classifier returns `bv={"cnt", "mem"}` — the memory
admitted alongside the scalar register. Two regression tests fail on that mutant and pass with the
filter, one on a hand-written mixed design and one on real yosys output (the lifted ibex register
file, `16 state 14 mem`).

## Scope — who is affected

Only a property whose atoms name a memory, which today can only arrive via:

- a hand-written `.mununu.json` sidecar naming a memory in `compound_predicates`;
- `--predicate` / the API `predicate` field.

**Not from SVA** — an array atom is refused at translation (mununu#514). This was filed and fixed
now because it is latent: any future SVA array support would make it live, and a silent mis-bind
produces a confident wrong answer rather than an error.

## The new note

```
kind:    array-atom-unsupported
level:   ScopeCaveat
items:   ["mem"]
summary: `<property>`: the atom names `mem`, which is a MEMORY (a BTOR2 array state), not a
         scalar register; memory atoms are not supported as cube dimensions and the property
         cannot be decided from them
```

Consumers keying on `verification_notes[i].kind` can add it; the memory's name is in `items`.

## Per-consumer

### ROSF / monono

- **What to update:** nothing, unless you name memories in a sidecar or `--predicate`. If you do,
  expect the note and a property that does not bind that atom, where previously it bound to
  something meaningless.
- **What to expect:** no change to any SVA-derived property.

### mununu-ui

- **What to update:** nothing required. The new note kind renders through the existing note list;
  add an icon/label for it if note kinds are enumerated explicitly.

## Docker rebuild disposition

| Image | Impact | Rebuild required? |
|---|---|---|
| `mununu-dev` | Rust only; no tool pins touched | No |
| `mununu-sva` | Inherits `mununu-dev`; tool pins unchanged | No |
| `mununu-sva-pono` | Inherits `mununu-sva`; tool pins unchanged | No |
| `hw-verif` | Not involved | No |

## Provenance

- Issue: [mununu#518](https://github.com/Mumunu-team/mununu/issues/518), found while fixing #514.
- Fix: `verify_auto::partition_state_cells_by_sort` +
  `verify_auto::formula_atoms_naming_memories`.
- Policy: [`docs/policies/cross-repo-impact.md`](../policies/cross-repo-impact.md).

## Not covered here

- **Memory atoms are refused, not supported.** Routing them to the `Select` path (`mem[i] == v` as
  a real cube dimension) is a feature, not this fix; the issue called refusing "the smaller, safer
  step" and that is what shipped.
- The note fires per property per memory named. A property naming two memories produces two notes.
