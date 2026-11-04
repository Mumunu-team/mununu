# M.3 — Clustered cone-of-influence on real OpenTitan RTL

> Status: milestone fixture (§Phase 11 slot 7, R4W-4 / R4W-5). Validates
> R.4 **property-clustered COI** end-to-end on real RTL through the
> automated `sv-yosys` verify pipeline.

## What this demonstrates

Two **independent** instances of the real OpenTitan
[`csrng_main_sm`](source/csrng_main_sm.sv) sparse FSM are wired by a thin
harness ([`csrng_main_sm_pair.sv`](source/csrng_main_sm_pair.sv)) so each
instance has its own primary inputs and the two share only `clk`/`rst`.
The single-module `sv-yosys` route flattens the harness into one BTOR2
whose dependency graph has **two disjoint state-register cones**.

A property over instance 0's error output (`u0_main_sm_err_o`) has a cone
disjoint from a property over instance 1's, so partitioning the two
properties by Jaccard similarity yields **two clusters**, each bit-blasting
a cone strictly smaller than the naive joint cone-of-influence over both:

```
clustered-COI: joint cone 5 signals, 2 cluster(s), max cluster cone 3 signals
               (reduces binding cone by 2 vs joint COI)
verdict u0_err_reachable: SAT 4096/4096
verdict u1_err_reachable: SAT 4096/4096
```

The verdicts are non-vacuous (each `μ`-reachability property is evaluated
over the full bit-blast state space and reaches a definite verdict). The
headline is the **measured cone reduction** (5 → 3) — the M.3
done-criterion: clustered cones reduce state space vs the naive joint COI
on a real fixture.

## How to run

```bash
cargo build -p mununu-cli          # build the binary
./validate.sh                      # runs the full pipeline + checks the criteria
```

Requires `yosys` (with `read_slang`) on `PATH` (the KMTS frontend), and
`LIBRARY_PATH=/usr/local/opt/z3/lib` for the z3 link (handled by
`validate.sh`).

## Fixture provenance (M-milestone pattern)

| File | Origin |
|---|---|
| [`source/csrng_main_sm.sv`](source/csrng_main_sm.sv) | **Vendored** from lowRISC/opentitan, pinned by [`source/UPSTREAM_COMMIT.txt`](source/UPSTREAM_COMMIT.txt). The real sparse-FSM being verified. |
| [`source/csrng_main_sm_pair.sv`](source/csrng_main_sm_pair.sv) | **NOT vendored** — hand-written test harness instantiating two real `csrng_main_sm` instances with independent I/O. Ties off every wide / non-essential input internally, leaving 2 free 1-bit inputs so the explicit bit-blast stays tiny. |
| [`source/csrng_pkg.sv`](source/csrng_pkg.sv) | **NOT vendored** — minimal stub package (the `acmd_e` + `main_sm_state_e` enums + widths the FSM consumes), shared with the M.2 fixture. |
| [`source/prim_assert.sv`](source/prim_assert.sv) | **NOT vendored** — empty-SVA-macro stub + `PRIM_FLOP_SPARSE_FSM` expanded as a plain `always_ff`, shared with the M.2 fixture. |

The harness is a test wrapper, not a hand-authored model: the automated
pipeline still extracts the real `csrng_main_sm` FSM logic from upstream
RTL; the harness only fixes the I/O boundary (exactly as the M.0–M.2
stub packages do).

## Why a two-instance harness

`csrng_main_sm` alone is a single FSM whose cone is connected — it cannot
exhibit a clustered-COI *reduction* (there is only one cone). Clustered
COI is about **independent properties over independent subsystems**; two
instances of a real module with independent inputs is the minimal faithful
realization (the same shape as OpenTitan `pattgen`'s two channels). The
richer multi-detector fixtures (`pattgen`, `sysrst_ctrl`) are deferred —
see [`docs/design/native-sv-abstraction.md`](../../../docs/design/native-sv-abstraction.md)
§10.3 M.3 and the R4W breakdown.

## Soundness

The properties are reachability (`μX. (err || ◇X)`); the verdicts are
definite over the bit-blast model. Clustered COI is a *partitioning of
the verification work*, not an abstraction of behaviour — it does not
change which signals each property's verdict depends on, only how many
are bit-blasted together. The per-cluster cones are exact sub-cones of
the joint COI.
