# phase10_caliptra_mbox_blackbox — Caliptra mailbox external-SRAM black-box contract

> **Status: stage 5 COMPLETE (contract-authoring track).** This fixture
> models the Caliptra `mbox.sv` mailbox's **external SRAM** as a
> black-box module and drives it through mununu's contract subsystem
> end-to-end: chaotic-stub baseline → authored read-after-write +
> address-stability clauses → gap downgrade → HITL review → acyclic
> discharge validation. The verify-*verdict* (contract → composable
> automaton) remains task A6, an explicit follow-up.

## What this is — and is NOT (per CLAUDE.md §Claims Integrity)

**This is** a *contract-authoring demonstration* on a real industrial
memory interface: the Caliptra mailbox payload buffer is an external
SRAM (the `mbox_sram_req_*` / `mbox_sram_resp_*` interface in
chipsalliance/caliptra-rtl's `src/soc_ifc/rtl/mbox.sv`), which Yosys
cuts at the module boundary into I/O ports rather than an internal
memory array. mununu models it as a **black-box module with an
authored memory contract** and validates the contract through the
`mununu contract` pipeline.

**This is NOT** a "mununu verified Caliptra's mailbox" claim. The
contract-clause authoring + discharge validation is what ships here.
Turning an authored contract into a *composable verifying automaton*
(and thus an actual read-after-write verdict) is **task A6**
(chaotic-CLTS synthesis from contracts), an explicit follow-up — not
this milestone. See
`Phase10-stage5-caliptra-mbox-blackbox-2026-06-12.md` (a planning note, not tracked)
for the full scoping.

## Why the black-box path (not the array encoder)

The §Phase 10 stage-3.c array encoder (`btor2_encode.rs`, Z3 array
theory) handles **internal** inferred memory — a BTOR2 array-sorted
`state` cell. Caliptra mbox's SRAM is **external** (a separate IP block
behind the `mbox_sram_*` handshake), so there is no internal array
cell to lift. The black-box contract subsystem is the correct tool for
an external memory interface. The natural fixture for the *array
encoder* is ibex `ibex_register_file_ff` (internal `$mem`), addressed
separately by the uf-memory Option-4 track
([`docs/design/uf-memory-end-to-end.md`](../../../docs/design/uf-memory-end-to-end.md)).

## Files

| File | What it is |
|---|---|
| `mbox_sram.interface.json` | The bare `BlackBoxInterface` — 8 ports (clk + 5 request inputs + 2 response outputs) of the Caliptra mailbox external SRAM. No clauses. |
| `baseline_discover.json` | `contract discover` on the bare interface. Chaotic-stub default: inputs `Uncontrollable`, outputs `Controllable`, `output_sequencing` gap on the two response outputs. |
| `mbox_sram.contracted.interface.json` | The same interface + 3 authored annotations: the read-after-write `@mununu_guarantee` and two `@mununu_assume` clauses (address-stability + no concurrent read/write to the same address). |
| `contracted_discover.json` | `contract discover` on the contracted interface. The gap **downgrades** to `latency_bound` ("1 guarantee clause(s) found") — the observable proof the authored contract was recognized. |
| `contracted_review.json` | `contract review` on the contracted interface — the HITL stage surfacing all 3 proposed clauses with `source_comment` provenance + soundness notes ("Reviewer must verify against the silicon or the spec"). |
| `mbox_sram.contract_set.json` | A hand-authored `ContractSet`: the 3 clauses + the two assumptions declared as `environment_assumptions` (the outside world must provide address-stability + no-concurrent-RW for the guarantee to hold). |
| `contract_validate.json` | `contract validate` on the contract set — discharge verdict **`acyclic`** (exit 0) with the topological order + the `unmet_environment` list (the two env assumptions awaiting an external discharger). |

## The chaotic-stub baseline (stage 5.1)

```bash
mununu contract discover mbox_sram.interface.json --json   # → baseline_discover.json
```

With no authored clauses, phase-1 discovery emits one
`output_sequencing` gap covering `mbox_sram_resp_rdata` +
`mbox_sram_resp_ecc`. Per `gap.rs`, this gap's soundness contract is:
**safety verdicts hold; liveness verdicts depending on these labels
are unsound (no progress assumption).** The SRAM's read data is treated
as fully nondeterministic — a sound over-approximation, but it admits
the read-after-write violation (a read can return anything), so it
does not yet verify the memory's defining property.

The strict-contracts gate fails on this baseline (exit 1), proving the
gate fires when an output is left chaotic:

```bash
mununu contract discover mbox_sram.interface.json --strict-contracts ; echo $?   # → 1
```

## The full pipeline (stages 5.1 → 5.2 → 5.3)

| Step | Input | Observable result |
|---|---|---|
| **5.1 baseline** | `mbox_sram.interface.json` | `output_sequencing` gap on the response outputs; `--strict-contracts` exits 1. Read data is fully nondeterministic — sound for safety, but admits the read-after-write violation. |
| **5.2 authored clauses** | `mbox_sram.contracted.interface.json` | Gap **downgrades** `output_sequencing` → `latency_bound`. The read-after-write guarantee + address-stability assumptions are recognized by phase-2 discovery. |
| **5.3a review** | `contract review` | All 3 clauses surfaced for HITL approval with `source_comment` provenance + per-clause soundness notes. |
| **5.3b discharge** | `mbox_sram.contract_set.json` | `contract validate` → **`acyclic`**; topological order computed; the two env assumptions listed as `unmet_environment` (correct — they require an external discharger). |

**The gap downgrade is the substance.** Even before task A6 (which would
turn the authored contract into a verifying automaton), the contract
machinery *responds measurably* to the authored read-after-write
guarantee: the chaotic `output_sequencing` gap becomes a `latency_bound`
gap, the clauses pass discharge validation acyclically, and the HITL
review surfaces them with the right provenance + soundness caveats.
That is a real demonstration of the contract-authoring path on a real
industrial memory interface — not a config file with no consequence.

## Soundness note (per the gap kinds)

- `output_sequencing` (baseline): *safety verdicts hold; liveness
  verdicts on these labels are unsound (no progress assumption).*
- `latency_bound` (contracted): the user has authored *some* sequencing
  behaviour; the remaining gap is the unauthored latency bound. The
  read-after-write *value* relation is now contracted; what's still
  open is the *timing* (how many cycles until `resp_rdata` is valid).
- The two assumptions are `environment_assumptions` — sound only if the
  deployed environment (the mbox FSM's access protocol) actually
  provides address-stability + no-concurrent-RW. The review's soundness
  note flags this for the reviewer.

## Reproduce

```bash
cd examples/verify/phase10_caliptra_mbox_blackbox
# 5.1 — chaotic baseline
mununu contract discover mbox_sram.interface.json --json
mununu contract discover mbox_sram.interface.json --strict-contracts ; echo "strict exit: $?"   # 1
# 5.2 — authored clauses → gap downgrade
mununu contract discover mbox_sram.contracted.interface.json --json   # latency_bound
# 5.3a — HITL review
mununu contract review mbox_sram.contracted.interface.json --json
# 5.3b — discharge validation
mununu contract validate mbox_sram.contract_set.json --json   # acyclic
```
