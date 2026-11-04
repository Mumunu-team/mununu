# R46-6 / GAP-2 — wide field concretized + escape-to-OOB, no spurious VIOLATED

> **Synthetic fixture (hand-written, NOT vendored RTL).** Like
> `r46_synth_two_cones`, this exists to regression-test a specific
> mechanism in isolation — here the GAP-2 composition of (a) the
> effective-bits cap accounting and (b) the realize numericity-gate
> soundness fix — with a design small enough to read.

## What it demonstrates

`source/wide7.sv` is a **24-bit** counter `cnt` with three combinational
outputs `atK_o = (cnt == K)` for K ∈ {1, 5, 7}.

Two mechanisms compose here:

1. **Effective-bits cap accounting (GAP-2).** At raw width the design has
   24 state bits — past the bit-blast cap (`MAX_STATE_BITS = 20`), so the
   *un-abstracted* design is rejected with `StateSpaceOverflow`. The
   sidecar (`source/wide7.mununu.json`) declares
   `cnt : bounded_counter bound=7`, concretizing it to the value set
   {0..7} = **3 effective bits**, which fits the cap. The cap check counts
   `ceil(log2(|value set|))` per concretized cell, not raw width — that is
   what lets a wide field be verified once a property only needs a small
   slice of its range.

2. **Realize numericity-gate fix (the soundness bug this fixture guards).**
   `cnt` escapes its declared set at 7 → 8, so the bit-blaster routes that
   transition to the absorbing **OOB sink**, which carries the marker
   valuation `{__mununu_oob__: "true"}`. Before the fix, that single
   non-numeric marker tripped `clts_valuations_are_numeric`, which gates
   whether per-state numeric valuations are wired into the evaluator's
   `abstract_states` channel. With the gate closed, formula atoms fell
   through to the false-everywhere path and the reachability properties
   reported a **spurious VIOLATED** — even though `cnt == 1` and `cnt == 7`
   are genuinely reached on the path 0 → 1 → … → 7 (all in-set). The fix
   exempts the OOB marker key from the gate; the sink stays masked by the
   evaluator while the real states bind normally.

## Expected result

```
reach_t1: SATISFIED   (cnt == 1 reached on 0 → 1)
reach_t7: SATISFIED   (cnt == 7 reached on 0 → … → 7, just before the escape)
```

Both are SATISFIED and non-vacuous. The OOB sink itself is masked, so the
satisfying-state count is the real-state count (the sink does not satisfy).

## Run it

```bash
cargo build -p mununu-cli
examples/verify/r46_gap2_wide_concretize/validate.sh
```

`validate.sh` requires `yosys` (with `read_slang`) on `PATH` (the sv-yosys
pipeline). It checks all three contract points: the raw design is rejected
at the cap, the concretized design lifts, and both reachable targets come
back SATISFIED.

## CI-level regression

The validate.sh script needs `yosys` and is a manual reproduction.
The fix's CI-level guards are the unit tests in
`crates/mununu-core/src/context_dsl/realize.rs`:
`oob_sink_marker_does_not_trip_numericity_gate` (positive) and
`non_numeric_real_valuation_still_trips_numericity_gate` (negative).
