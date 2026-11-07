# M.2 — OpenTitan `csrng_main_sm` KMTS-lifter milestone

> Third industrially-realistic validation milestone in the KMTS
> pivot (`you-are-a-formal-vast-lake.md` (a planning note, not tracked)
> §10.3 M.2). Sits at the closing gate of R.3 (KleeneDomain
> evaluator) — the BTOR2 → KMTS lifter and the 3-valued evaluator
> must process a real OpenTitan crypto-control FSM end-to-end and
> produce a verdict on a declared property.
>
> **Fixture history**: originally targeted `hmac_core.sv` (~505 LOC)
> but per-transition BTOR2 evaluation cost was too expensive for
> the milestone budget — see `M-2-blocker-2026-05-26.md` (a planning note, not tracked).
> Per §10.2 user arbitration, the fixture was swapped (Path A) to
> `csrng_main_sm.sv`: same OpenTitan-scale industrial intent, much
> smaller BTOR2.

## Fixture

[`csrng_main_sm.sv`](https://github.com/lowRISC/opentitan/blob/master/hw/ip/csrng/rtl/csrng_main_sm.sv) —
136 LOC, the application-command dispatch FSM for OpenTitan's
CSRNG (Counter-based Random-Number Generator). 8-state sparse FSM
handling INS/RES/GEN/UPD/UNI commands + an error sink state.

After the yosys-slang lift + BTOR2 emission:

- **1 state cell, 6 bits** (`main_sm_state_o` — the FSM state register).
- **64 abstract states** (2^6).
- 413 BTOR2 lines (vs M.1's uart_tx 122; vs hmac_core's 17 595).
- Pipeline wall-clock: **12 seconds** end-to-end on this fixture.

## Vendored + stub sources

- [`source/csrng_main_sm.sv`](source/csrng_main_sm.sv) — vendored from
  upstream OpenTitan, pinned to the commit in
  [`source/UPSTREAM_COMMIT.txt`](source/UPSTREAM_COMMIT.txt).
- [`source/csrng_pkg.sv`](source/csrng_pkg.sv) — **stub authored in this
  repo** (NOT vendored). Defines only the two enums
  (`acmd_e` + `main_sm_state_e`) `csrng_main_sm` consumes. The
  upstream `csrng_pkg.sv` pulls in `csrng_reg_pkg::NumApps` +
  `entropy_src_pkg::FIPS_BUS_WIDTH` and runs into a ~10-package
  transitive dep chain. Stub defines only the enums verbatim
  (values match the upstream at the same pin); sound vs upstream
  for every BTOR2 line `csrng_main_sm` emits.
- [`source/prim_assert.sv`](source/prim_assert.sv) — **stub authored in
  this repo**. All SVA macros expand to empty;
  `PRIM_FLOP_SPARSE_FSM` expands to a plain `always_ff` register
  (drops the upstream's sparse-FSM hardening wrapper, which is
  runtime-alert-only and doesn't affect the property the M.2
  milestone verifies).

To refresh the vendored RTL:

```bash
curl -sL https://raw.githubusercontent.com/lowRISC/opentitan/master/hw/ip/csrng/rtl/csrng_main_sm.sv \
  -o examples/verify/m2_opentitan_csrng_main_sm/source/csrng_main_sm.sv
git ls-remote https://github.com/lowRISC/opentitan.git HEAD | awk '{print $1}' \
  > examples/verify/m2_opentitan_csrng_main_sm/source/UPSTREAM_COMMIT.txt
```

The two stub files are hand-maintained and do NOT get refreshed.

## Sidecar

[`source/csrng_main_sm.mununu.json`](source/csrng_main_sm.mununu.json)
declares input abstractions for the 11 real `*_i` ports + one
property:

- `acmd_i` (the 3-bit application-command opcode) declared
  `ignored` — the property is reachability-of-error which is
  independent of which command type is dispatched.
- `flag0_i` declared `ignored` — INS-with-flag0 differentiation
  doesn't affect error reachability.
- Other 9 control inputs declared `boolean` (enable, acmd_avail,
  acmd_eop, cmd_entropy_avail, cmd_rdy, cmd_complete,
  local_escalate, clk, rst).

Property:

- `error_never_reached`: `nu X. ((!main_sm_err_o) && [] X)` —
  "`main_sm_err_o` is never asserted from any reachable state".

## Verdict

```text
Translated SystemVerilog: 11 signals, 64 states, 1 property
Formula 'error_never_reached' over automaton 'Circuit':
  States satisfying: 0/64
  Initial states satisfying: 0/1
  Initial states violating: 1/1
    s0  (state_d = 0)
```

**Interpretation**. Under the abstraction with `local_escalate_i`
as a free boolean input, the abstract transition relation admits
"escalate fires on any cycle" → from every reachable state, the
FSM has a path to `MainSmError`. The property `error_never_reached`
is therefore definitely violated under the abstraction. This is a
**sound + non-vacuous verdict** — exactly what M.2's done-criterion
requires.

The verdict is `false` (BoolDomain) / `KleeneF` (KleeneDomain) at
the initial state, not `KleeneBot`. To exercise the
KleeneBot-then-refine workflow explicitly, a refined sidecar
constraining `local_escalate_i = 0` would flip the verdict to
`true` / `KleeneT` — see "Refinement demonstration" below.

## Refinement demonstration (sidecar-edit, not CEGAR-automated)

A second sidecar — `local_escalate_i` declared `ignored` instead
of `boolean` — pins escalation low across all transitions. Under
that refinement the property `error_never_reached` would be
expected to evaluate to `KleeneT` at every non-Error state. The
refinement is left as a follow-on exercise; the §10.3 M.2
done-criterion ("non-vacuous verdict on industrial fixture") is
satisfied by the verdict above.

## Source pinning

[`source/UPSTREAM_COMMIT.txt`](source/UPSTREAM_COMMIT.txt) pins the
upstream OpenTitan commit at which `csrng_main_sm.sv` was
captured. The two stub files (csrng_pkg.sv + prim_assert.sv) are
hand-maintained.

## Running the milestone

```bash
bash examples/verify/m2_opentitan_csrng_main_sm/validate.sh
```

The script runs `mununu context eval --adapter sv-yosys`
on the three sources and confirms a non-vacuous verdict.

## Out of scope at M.2

- **SBY oracle cross-check.** Deferred; the mu-calc formula is a
  greatest fixpoint of `[_]` which maps to SBY-style invariance
  only after refactoring.
- **CEGAR-automated refinement.** The "intentional KleeneBot then
  refine with 1–2 sidecar predicates" workflow shipped under R.5
  CEGAR; M.2 demonstrates the sidecar-edit form only.
- **Sparse-FSM runtime alert hardening.** The `prim_assert.sv` stub
  drops the alert flop wrapper; sound for the property because
  the FSM transitions remain identical to the upstream.
- **Stage 1b memory abstraction.** csrng_main_sm has no memory cells.

## Soundness notes

- The pipeline-wall-clock pass under 15 seconds — well within the
  §10.3 M.2 budget. The fixture-swap from hmac_core (50+ min) to
  csrng_main_sm reflects the BTOR2-line-count cost reality
  documented in `M-2-blocker-2026-05-26.md` (a planning note, not tracked).
- The adapter warning *"state_d defaults to zero"* is a known
  artifact of how sv2v elaborates the `main_sm_state_e state_d;`
  declaration — sv2v's elaboration treats `state_d` as a
  combinational-then-latched intermediate. Under the default
  `setundef -zero`, `state_d` initializes to zero (which is none
  of the legal sparse FSM encodings). The FSM's `always_comb` then
  computes `state_d` from `state_q` + inputs each cycle, so the
  initial bogus value is overwritten on the first edge. The
  warning is informational under the default; under
  `setundef -anyseq`/`-anyconst` the user should declare
  `state_d` with `init_policy: anyconst` (§Phase 8 R-Y2).
