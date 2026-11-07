# 4-bit DMR Commit Gate — Fault-Tolerance Contract Example

> Status: LTS verdict reproduced on this branch. The verdict has not yet
> been reproduced under simulation in `hw-verif:latest`. Per
> [CLAUDE.md §Claims Integrity rule 8](../../../CLAUDE.md), the SAT /
> UNSAT pair below is "LTS witness only — not reproduced in simulation."

This directory illustrates the assume/guarantee contract pattern from
[`docs/archive/design/black-box-modules.md`](../../../docs/archive/design/black-box-modules.md)
applied to a fault-tolerance question: an adversary may flip exactly one
bit of a functional unit's output per cycle; the design must not
broadcast a corrupted result.

The example is intentionally tiny — a 4-bit dual-modular-redundancy
(DMR) commit gate — because that is the scope at which the
SystemVerilog → Kripke pipeline produces a tractable state space (see
"What this example does NOT prove" below).

## Files

- [`dmr_top.sv`](dmr_top.sv) — intact SV source. Two identical 4-bit
  replicas, a one-hot adversarial XOR mask on replica A, a comparator, a
  registered commit stage, and an observer register that latches if a
  commit ever broadcasts a value disagreeing with the pipelined golden
  reference.
- [`dmr_top_broken.sv`](dmr_top_broken.sv) — negative control. Same
  structure but `commit_valid_r <= 1'b1` (commits unconditionally,
  ignoring the comparator). Demonstrates the formal verdict catches the
  missing mitigation.
- [`dmr_top.mununu.json`](dmr_top.mununu.json) and
  [`dmr_top_broken.mununu.json`](dmr_top_broken.mununu.json) — signal
  abstractions + the safety property formulas.
- [`contracts.json`](contracts.json) — the contract set: one environment
  assumption (`A_env`), three guarantees / one invariant
  (`G_compare`, `G_commit`, `G_top`), and the linear discharge chain.

## Reproducing the verdicts

Run from the repo root:

```bash
# 1. Discharge graph check — proves the *structure* of the assume/guarantee
#    proof obligation is sound (no circular reasoning, every assumption has
#    a guarantor).
mununu contract validate examples/hw/dmr_commit_4bit/contracts.json
# expected: discharge: acyclic
#   topological order: G_top, G_commit, G_compare, A_env

# 2. Safety check on the intact design.
mununu context eval examples/hw/dmr_commit_4bit/dmr_top.sv \
  --adapter systemverilog \
  --formula no_corrupt_broadcast \
  --automaton dmr_top
# expected: States satisfying: 81/81
#           Initial states satisfying: 1/1

# 3. Deadlock-freedom baseline on the intact design.
mununu context eval examples/hw/dmr_commit_4bit/dmr_top.sv \
  --adapter systemverilog \
  --formula no_deadlock \
  --automaton dmr_top
# expected: States satisfying: 81/81

# 4. Negative control on the broken variant.
mununu context eval examples/hw/dmr_commit_4bit/dmr_top_broken.sv \
  --adapter systemverilog \
  --formula no_corrupt_broadcast \
  --automaton dmr_top_broken
# expected: States satisfying: 0/161
#           Initial states satisfying: 0/1
#   ↑ the safety property fails: removing the commit gate produces a
#     trace where the observer latches.
```

Step 1 and steps 2–4 are independent checks. Step 1 catches dishonest
contract structure at the IR level — an assumption with no real
guarantor, the Meta/Google 2021 SDC failure mode reframed as a contract
graph. Steps 2–4 catch an SV body that does not actually implement the
contract.

## Fault model

Precise single-bit-flip per cycle, encoded structurally:

```systemverilog
y_a_post = flip_en ? (y_a ^ (4'b0001 << flip_idx)) : y_a;
```

- The mask `4'b0001 << flip_idx` is **one-hot by construction** for any
  `flip_idx` ∈ {0,1,2,3}. MBU is excluded by encoding, not by
  assumption alone.
- When `flip_en=0` the value passes through unchanged.
- `flip_idx` and `flip_en` are environment-controlled inputs, so the
  model explores all 4×2 = 8 fault scenarios per cycle for every operand
  `x` ∈ {0..15}.

XOR support landed on this branch via `BinOp::BitXor` in
[crates/mununu-core/src/adapter/systemverilog/ast.rs](../../../crates/mununu-core/src/adapter/systemverilog/ast.rs),
[parser.rs:parse_bitxor_expr](../../../crates/mununu-core/src/adapter/systemverilog/parser.rs),
and [kripke.rs](../../../crates/mununu-core/src/adapter/systemverilog/kripke.rs).
Three parser tests confirm precedence (`& > ^ > |`) and the DMR
fault-injection pattern (`x ^ (4'b0001 << idx)`) parses correctly.

## What this example does NOT prove

Per [CLAUDE.md §Claims Integrity](../../../CLAUDE.md), the load-bearing
honesty caveats:

1. **Datapath correctness is unverified.** Both replicas are modelled as
   `y = x`. In a real functional unit they would compute the same
   nontrivial function on the same input, and the contract only proves
   "if both agree, commit." A bug present in *both* replicas (correlated
   failure) is not detected. This is exactly the failure mode documented
   in Dixit et al., *Silent Data Corruptions at Scale* (Meta, 2021,
   arXiv:2102.11245) and Hochschild et al., *Cores that don't count*
   (Google, HotOS 2021): real SDCs are input-pattern-dependent and hit
   both DMR replicas identically.

2. **Single fault per cycle only.** Multi-bit upsets are excluded by
   the one-hot mask. Real radiation environments at advanced
   semiconductor nodes see MBU (Baumann, IEEE TDMR 2005). Persistent
   faults (a stuck bit) are also excluded; `flip_en` is
   non-deterministic per cycle.

3. **Only `y_a` is faulted.** A symmetric model would add a
   `fault_b_en` and the assumption would become "at most one of the two
   replicas is faulted in any cycle." That requires a mutual-exclusion
   environment assumption this example does not bake in.

4. **The comparator, commit register, and observer are in the trusted
   compute base.** A bit-flip *inside* the comparator's combinational
   logic, or in `commit_valid_r` / `broadcast_data_r` /
   `corrupted_broadcast_r` themselves, bypasses the proof. In real
   RAD-hard designs the comparator output is itself triplicated (the
   LEON3-FT errata showed a registered voter output as the actual
   failure site); this example does not model that nesting.

5. **No liveness.** The contract proves "no corrupted broadcast ever
   occurs," not "every valid result eventually broadcasts." Liveness
   would need a fairness assumption on `flip_en` that the chaotic fault
   model does not admit.

6. **Replicas modelled as identity, not as a real FU.** A faithful 4-bit
   ALU could be substituted with no change to the contract or the
   property; the identity choice keeps the state space minimal and the
   focus on the gating layer.

## Why this scope, not more

The SystemVerilog adapter at [`crates/mununu-core/src/adapter/systemverilog/kripke.rs`](../../../crates/mununu-core/src/adapter/systemverilog/kripke.rs)
has a hard state cap of 2^18 ([kripke.rs:207](../../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L207))
and auto-abstracts signal widths > 4 bits ([kripke.rs:918](../../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L918)).
A full 32-bit FMA datapath does not fit; a 4-bit commit gate fits
comfortably (81 reachable states on the intact design, 161 on the
broken).

The contract framework is independent of this scope limit. The same A/G
discharge check would apply to a 32-bit design verified by a different
backend; only the mu-calculus verdict in steps 2–4 is bounded by
mununu's SV adapter.

## Adapter-safety patterns used

These are documented in CLAUDE.md §"Adapter / Emitter Capability Use"
and apply to any SV file fed to the Kripke pipeline:

- **Output ports are pure shadows of internal registers.** The SV Kripke
  builder treats `output logic` as combinational and only picks up
  internal `logic` declarations as state. The example uses internal
  `<name>_r` registers plus `assign output = <name>_r;` continuous
  assigns.
- **XOR (`^`)** is the natural primitive for the fault mask; supported
  on this branch.
- **No `concat()`** — sidesteps the LSB-truncation in [kripke.rs:1551](../../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L1551).
- **Every register has an explicit reset value** — sidesteps the
  fall-back-to-false unsoundness at [kripke.rs:1405](../../../crates/mununu-core/src/adapter/systemverilog/kripke.rs#L1405).
- **All datapath widths ≤ 4 bits** — within auto-abstract scope.

## Provenance and honesty

The contract clauses in [`contracts.json`](contracts.json) are
hand-authored for this example. They are not derived from any real
vendor IP datasheet, and the example does not claim to find a
vulnerability in any commercial GPU functional unit. The contribution
is the *pattern* — explicit fault assumption + explicit gating
guarantee + machine-checked discharge — not a finding about any
specific real-world chip.
