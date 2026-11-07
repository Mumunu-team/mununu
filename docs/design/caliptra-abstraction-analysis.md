# Caliptra-RTL #150 — Abstraction Analysis

> **Status: superseded by Phase A.3 (2026-05-19).** The runtime-performance
> blocker this doc analyses was cleared by the auto-partition + predicate-
> binding shipment documented in
> [`proof-by-fire-findings.md`](proof-by-fire-findings.md#phase-a3-update--auto-partition--predicate-binding-ship-2026-05-19).
> The runnable end-to-end fixture lives at
> [`examples/verify/sv_yosys_caliptra_rtl_150/`](../../examples/verify/sv_yosys_caliptra_rtl_150/).
> The §2.2 signal × abstraction matrix and §2.3 property catalog below
> remain the canonical reference for *which* signals to keep / drop
> and which mu-calculus properties to evaluate; only the §3 "Recommended
> actions" list is now historical.
>
> Original status: analysis doc, Phase 1.7 of the
> pipeline-blocker plan (`create-a-plan-to-enumerated-patterson.md`, an untracked note).
> Read-only audit; the actions section recommends concrete next steps but
> does not perform them. Companion to
> [`proof-by-fire-findings.md`](proof-by-fire-findings.md) — this doc
> grounds the "next concrete step" decision after Phase 1.6's runtime-
> performance finding.

## Why this doc exists

Phase 1.6 cleared the input-bit cap (16 → effective 6 via sidecar pruning)
on Caliptra-RTL's boot FSM. The bit-blaster then ran for >20 minutes on
a release build, accumulated 4.6 GB RSS, and was killed before producing
a verdict. The 524 K × 64 = 33.5 M transition enumeration is not
inherently impossible; the question this doc answers is *what existing
mununu primitives bring the enumeration down to a tractable scale
without an engine rewrite*.

Three deliverables, all in this doc:

- **§1** — Catalog of every abstraction mechanism the custom SV adapter
  uses (including SMT discovery), with a reusability matrix showing
  which the BTOR2 path already honours.
- **§2** — Signal × abstraction matrix for `soc_ifc_boot_fsm_pre_fix.sv`,
  plus the four mu-calculus properties evaluated on the hand-modelled
  staging variant.
- **§3** — Recommended actions, ranked by impact × feasibility.

---

## §1 — SV adapter abstraction primitives and their reusability by the BTOR2 path

### 1.1 — Primitives

Source of truth: [`crates/mununu-core/src/adapter/domain.rs:20-140`](../../crates/mununu-core/src/adapter/domain.rs#L20). `AbstractionType` is the variant enum; `FieldDomain` wraps it with bounds, variants, and an optional `initial` value.

| Primitive | What it preserves | What it drops | Cardinality | File:line |
|---|---|---|---|---|
| `Boolean` | true / false distinction | All values not in `{0, 1}` (treated as their boolean truthification) | 2 | [`domain.rs:24`](../../crates/mununu-core/src/adapter/domain.rs#L24) |
| `Presence` | "value is present" vs "absent" | The actual value | 2 | [`domain.rs:26`](../../crates/mununu-core/src/adapter/domain.rs#L26) |
| `BoundedCounter` | Concrete value in `[lower_bound .. bound]` | Values past the bound (saturated → out-of-bounds sink) | `bound − lower + 1` | [`domain.rs:28`](../../crates/mununu-core/src/adapter/domain.rs#L28) |
| `EnumValues` | A named variant set, optionally with concrete value map | Anything not matched → `catch_all` variant | `variants.len()` | [`domain.rs:30`](../../crates/mununu-core/src/adapter/domain.rs#L30) |
| `Ignored` | Nothing (signal pinned to 0) | Everything; signal contributes 0 effective bits | 1 | [`domain.rs:32`](../../crates/mununu-core/src/adapter/domain.rs#L32) |

The sidecar's `SignalAbstraction::Discover` is **not a runtime primitive** — it is a *directive* for SMT discovery. The resolver at
[`sidecar/mod.rs:130-154`](../../crates/mununu-core/src/adapter/sidecar/mod.rs#L130) translates a `Discover`-marked signal *plus* a populated `discovered_values` entry into an `EnumValues` domain.

### 1.2 — Enumeration: abstract cross-product vs concrete bit-blast

The custom SV adapter's Kripke builder ([`adapter/systemverilog/kripke.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs)) enumerates **the cross product of per-signal `FieldDomain` value sets** ([`adapter/state_enum.rs:14`](../../crates/mununu-core/src/adapter/state_enum.rs#L14)), not the cross product of raw bit vectors. For each abstract `(state, input)` pair it runs `compute_next_state()` over the parsed AST.

The BTOR2 bit-blaster ([`adapter/btor2/bit_blast.rs`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs)) — after Phases 1.5 + 1.6 — *also* uses cross-product-over-per-cell-value-sets (`CellEnumeration` for states, `InputCellEnumeration` for inputs), driven by the same `FieldDomain` types resolved from the same sidecar shape. **The difference between the two enumerators is the front-end (parsed SV AST vs Yosys-emitted BTOR2), not the abstraction algebra.**

### 1.3 — SMT-based discovery (`mununu sv discover`)

Source of truth: [`adapter/systemverilog/kripke_smt.rs:19-155`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs#L19). Requires `--features smt` (Z3 bitvector theory). CLI handler at [`crates/mununu-cli/src/main.rs`](../../crates/mununu-cli/src/main.rs) (search for `sv_discover`).

Algorithm:

1. For each signal marked `"abstraction": "discover"`, find guard expressions in `always_ff` / `always_comb` that mention it.
2. For each such guard, ask Z3 to enumerate satisfying assignments (capped at 32 per signal). The signal's RHS in `signal == constant` comparisons is the typical hit.
3. Merge with syntactic constants from `case` labels (no SMT needed).
4. Write the result into the sidecar's `discovered_values: { <signal>: { values: [...], catch_all: "OTHER" } }` field. Schema at [`adapter/systemverilog/annotation.rs:192-216`](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs#L192).

Cross-module variant `sv_discover_multi` covers connected signals across module instances.

### 1.4 — Reusability matrix (custom SV adapter ↔ BTOR2 bit-blaster)

| Mechanism | Custom SV adapter | BTOR2 bit-blaster | Reusable today? | Evidence |
|---|---|---|---|---|
| `FieldDomain` cross-product enumeration | ✓ via `kripke::build_kripke_with_config` | ✓ via `CellEnumeration` (state) + `InputCellEnumeration` (input) | **Yes** — already wired Phase 1.5 + 1.6 | [`bit_blast.rs` state side](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs); input side added 2026-05-18 |
| `Boolean` / `BoundedCounter` / `EnumValues` / `Ignored` from sidecar | ✓ via `resolve_to_field_domain` | ✓ via `build_field_domains_for_btor2` + `build_input_field_domains` | **Yes** | [`sidecar/btor2_resolver.rs`](../../crates/mununu-core/src/adapter/sidecar/btor2_resolver.rs) |
| `discover` → `EnumValues` conversion (when `discovered_values` is populated) | ✓ via [`sidecar/mod.rs:130-154`](../../crates/mununu-core/src/adapter/sidecar/mod.rs#L130) | **✓ — same resolver** (format-agnostic) | **Yes, latent.** Resolver fires for any caller; BTOR2 reader was wired in Phase 1.5/1.6 but never exercised on a `discover`-mode design. | The resolver function is shared; the path is reachable but unused in any shipped example |
| SMT discovery of significant values (`mununu sv discover`) | ✓ via Z3 on the parsed SV AST | **✗ — discovery requires the custom SV parser** | **Partially.** The discovery step needs the SV adapter's AST. The *output* (`discovered_values` in sidecar) is consumable by the BTOR2 path via 1.3 above. | [`kripke_smt.rs:86`](../../crates/mununu-core/src/adapter/systemverilog/kripke_smt.rs#L86) requires `&Module` (SV AST) |
| Cross-module discovery | ✓ for SV multi-module composition | ✗ N/A | n/a | Multi-module BTOR2 is a separate Yosys flow |
| State-space ceiling check | ✓ at [`kripke.rs`](../../crates/mununu-core/src/adapter/systemverilog/kripke.rs) line ~207 | ✓ `MAX_STATE_BITS` / `MAX_INPUT_BITS` caps in `bit_blast.rs` | **Yes, but value differs.** Custom SV checks abstract-state product against a soft ceiling; BTOR2 checks raw + effective bit width. | — |

**Key reusability finding.** The BTOR2 path **already** honours every `FieldDomain` shape the SV adapter produces, including the `discover → EnumValues` derivation. *If* the user runs `mununu sv discover` and the resulting `discovered_values` are written into the same `.mununu.json` the BTOR2 reader auto-loads, those discovered values flow through to BTOR2 state / input enumeration with zero new code.

**Two caveats.** (a) `mununu sv discover` requires the custom SV parser to accept the source. Caliptra's upstream SV uses constructs the custom parser does not handle (the same SV2009/2012 constructs that blocked Yosys before Phase 1's sv2v integration). Whether the *sv2v-preprocessed* Verilog-2005 output is digestible by the custom SV parser is **an empirical question this doc does not answer**. (b) `discover` mode requires `--features smt` (Z3 dependency) — not in the default build.

### 1.5 — `discovered_values` schema (for reference)

From [`annotation.rs:192-216`](../../crates/mununu-core/src/adapter/systemverilog/annotation.rs#L192):

```jsonc
{
  "discovered_values": {
    "<signal_name>": {
      "values": [
        { "value": 0, "name": "VAL_0", "from": "SMT: guard (cmd == 0) at line 0" },
        { "value": 1, "name": "VAL_1", "from": "SMT: guard (cmd == 1) at line 0" }
      ],
      "catch_all": "OTHER"
    }
  }
}
```

A hand-populated `discovered_values` entry has the same downstream effect as one produced by SMT — the resolver does not check provenance.

---

## §2 — Caliptra signal × abstraction matrix and property catalog

Source: [`examples/verify/sv_yosys_caliptra_rtl_150/source/soc_ifc_boot_fsm_pre_fix.sv`](../../examples/verify/sv_yosys_caliptra_rtl_150/source/soc_ifc_boot_fsm_pre_fix.sv) at commit `b436906f0b16ae` (chipsalliance/caliptra-rtl).

### 2.1 — Signal inventory

**Inputs** (16 raw bits, 9 distinct signals):

| Signal | Width | Role | Drives `boot_fsm_ps`? |
|---|---|---|---|
| `clk` | 1 | Clock (implicit in BTOR2; pinned to 1) | No — clock |
| `cptra_pwrgood` | 1 | Power good / async reset | Yes — async reset to `BOOT_IDLE` |
| `cptra_rst_b` | 1 | System reset | Yes — gates `BOOT_IDLE → BOOT_FUSE` |
| `fw_update_rst` | 1 | FW-update reset request | Yes — gates `BOOT_DONE → BOOT_FW_RST` |
| `fw_update_rst_wait_cycles[7:0]` | 8 | Wait-counter load value | **No** — load value of `wait_count`; the FSM transitions `BOOT_WAIT → BOOT_DONE` when `wait_count == 0`, regardless of the load value |
| `BootFSM_BrkPoint` | 1 | Debug breakpoint | Yes (encoded into `boot_brk_continue`) |
| `BootFSM_Continue` | 1 | Debug continue | Yes (encoded into `boot_brk_continue`) |
| `fuse_done` | 1 | Fuse handshake | Yes — gates `BOOT_FUSE → BOOT_DONE` |
| `fuse_wr_done_observed` | 1 | Fuse write observed | Yes — `AND`-ed with `fuse_done` |

**State registers** (10 raw bits):

| Signal | Width | Role | Drives the bug? |
|---|---|---|---|
| `boot_fsm_ps[2:0]` | 3 | Present state (enum: 5 defined + 3 UNDEF encodings) | **Yes — the bug-bearing register** |
| `boot_fsm_ns[2:0]` | 3 | Combinational next state | **Yes** (couples to `boot_fsm_ps`) |
| `wait_count[7:0]` | 8 | Reset-window timer | **No** (for safety property; relevant for timing-related liveness) |
| `cptra_rst_window_*` chain | 3 | Reset-window detect | **No** (orthogonal RDC logic) |
| `synch_noncore_rst_b`, `synch_uc_rst_b` | 2 | 2FF reset synchronizers | **No** (feed outputs only) |
| `fsm_iccm_unlock` | 1 | ICCM unlock flag | **No** (output-only) |

Total raw observable state: ~27 bits. Hand-modelled CWE-1245 verification reduces this to ~10 bits ([`examples/verify/sv_yosys_caliptra_rtl_150/source/soc_ifc_boot_fsm_pre_fix.sv`](../../examples/verify/sv_yosys_caliptra_rtl_150/source/soc_ifc_boot_fsm_pre_fix.sv)).

### 2.2 — Signal × abstraction matrix

For the **CWE-1245 safety property** (`no_undef_reachable` — "no path reaches `boot_fsm_ps ∈ {3'b101, 3'b110, 3'b111}`"):

| Signal | Bug relevance | Proposed sidecar abstraction | Raw bits | Effective bits | Justification |
|---|---|---|---|---|---|
| `boot_fsm_ps` | Drives the bug | `enum_values { BOOT_IDLE, BOOT_FUSE, BOOT_FW_RST, BOOT_WAIT, BOOT_DONE, UNDEF }` | 3 | log₂(6) ≈ 3 | The bug is precisely the reachability of the `UNDEF` equivalence class. The enum collapses the 3 unmatched encodings into a single sink variant. |
| `boot_fsm_ns` | Drives the bug | same as `boot_fsm_ps` | 3 | log₂(6) ≈ 3 | Combinational; same enum tracks the next-state value |
| `cptra_rst_b` | Drives the bug | `boolean` | 1 | 1 | Gates the cold-reset arc |
| `cptra_pwrgood` | Drives the bug | `boolean` | 1 | 1 | Async reset; defines reachable-state boundary |
| `fw_update_rst` | Drives the bug | `boolean` | 1 | 1 | Gates `BOOT_DONE → BOOT_FW_RST` |
| `fuse_done` ∧ `fuse_wr_done_observed` | Drives the bug | each `boolean`, OR collapse to one composite `fuse_done_observed` | 2 | 1 (composite) or 2 (individual) | Hand model collapses both into the AND |
| `BootFSM_BrkPoint`, `BootFSM_Continue` | Drives the bug | each `boolean`, OR collapse to one composite `boot_brk_continue` | 2 | 1 (composite) or 2 (individual) | Hand model collapses via `BootFSM_BrkPoint & ~BootFSM_Continue` |
| `fw_update_rst_wait_cycles[7:0]` | **Reachable-state-irrelevant** | `ignored` | 8 | 0 | Only affects *when* `wait_count` reaches 0, not *whether* `BOOT_WAIT → BOOT_DONE` fires |
| `wait_count[7:0]` | **Reachable-state-irrelevant** | `bounded_counter(bound=0)` or `enum_values { ZERO, NONZERO }` | 8 | log₂(2) = 1 | Property only checks `wait_count == 0` — distinguishing zero from nonzero is enough |
| `cptra_rst_window` chain | Pure environment | `ignored` | 3 | 0 | RDC clock-gating; orthogonal to FSM logic |
| `synch_noncore_rst_b`, `synch_uc_rst_b` | Pure output coupling | `ignored` | 2 | 0 | Feed outputs only; no FSM feedback |
| `fsm_iccm_unlock` | Pure output | `ignored` | 1 | 0 | Output-only |
| `clk` | Clock | (pinned; not in input space) | — | 0 | Implicit posedge |

**Bit budget under the proposed matrix:**

- State bits: 3 (`boot_fsm_ps`) + 3 (`boot_fsm_ns`) + 1 (`wait_count` collapsed) = **7 effective** (was 19).
- Input bits: 1 + 1 + 1 + 1 + 1 + 1 = **6 effective** (was 16; Phase 1.6 already brought this to 6 by pinning `wait_cycles`, `BrkPoint`, `Continue`).
- Total transition enumeration: 2⁷ × 2⁶ = 128 × 64 = **8 192 transitions** (was 33.5 M).

This is a 4 000× reduction in enumeration scale. The current bit-blaster should complete in seconds, not hours, under this matrix.

**The dominant saving versus Phase 1.6's sidecar:** the `wait_count` state register. The Phase 1.6 sidecar pinned only inputs; the state cell `wait_count` retained its full 8-bit width. Adding a state-cell entry `{ "name": "wait_count", "abstraction": "bounded_counter", "bound": 0 }` (or `"abstraction": "enum_values", "variants": ["ZERO", "NONZERO"]`) is the single change with the most impact.

### 2.3 — Property catalog

Verbatim from [`examples/verify/sv_yosys_caliptra_rtl_150/source/soc_ifc_boot_fsm_pre_fix.mununu.json`](../../examples/verify/sv_yosys_caliptra_rtl_150/source/soc_ifc_boot_fsm_pre_fix.mununu.json) (formulas as they appear in the hand-modelled sidecar):

| Name | Formula (mu-calculus) | Expected: bug | Expected: fix |
|---|---|---|---|
| `no_undef_reachable` | `nu X. (!boot_fsm_ps_UNDEF && [] X)` | **FAILS** (0/11 states satisfy) — `UNDEF` is reachable via the unmatched-case path | **HOLDS** under defensive fix (`default: boot_fsm_ns = BOOT_IDLE;`); still fails under the upstream fix that uses `default: boot_fsm_ns = boot_fsm_ps;` |
| `recoverable_to_idle` | `mu X. (boot_fsm_ps_BOOT_IDLE \|\| <> X)` | **PARTIALLY FAILS** (1/11; only IDLE itself) — UNDEF is absorbing, cannot recover | **HOLDS** under defensive fix |
| `always_recoverable` | `nu Y. ((mu X. (boot_fsm_ps_BOOT_IDLE \|\| <> X)) && [] Y)` | **FAILS** (0/11) — combined safety + liveness; no path back to IDLE from UNDEF | **HOLDS** under defensive fix |
| `safety_all_states_have_successors` | `nu X. ([] X)` | **HOLDS** (11/11; vacuous smoke test) | **HOLDS** |

For the matrix in §2.2 to be sound on these properties, the state predicate `boot_fsm_ps_UNDEF` must remain expressible after abstraction. With the proposed `enum_values { …, UNDEF }` abstraction on `boot_fsm_ps`, the predicate maps to the `UNDEF` variant — directly supported.

---

## §3 — Recommended actions, ranked

### Action A — Refine the Caliptra sidecar to mirror the hand model

**Impact: high. Feasibility: hours.**

The single change with the largest effect: add `wait_count` as a `bounded_counter(bound=0)` or `enum_values` state-cell entry to the Caliptra sidecar. Optionally add `enum_values` for `boot_fsm_ps` / `boot_fsm_ns` to make the `UNDEF` predicate first-class. Expected effect: enumeration drops from 33.5 M transitions to ~8 K, completes in seconds even on the debug build.

Concretely, extend the Phase 1.6 sidecar at `/tmp/caliptra_retry/soc_ifc_boot_fsm_pre_fix.mununu.json`:

```jsonc
{
  "$schema": "mununu_sv_annotation_v1",
  "module": "soc_ifc_boot_fsm",
  "signals": [
    {
      "name": "wait_count",
      "abstraction": "bounded_counter",
      "bound": 0,
      "note": "Property only depends on wait_count == 0; collapse to ZERO / NONZERO."
    },
    {
      "name": "boot_fsm_ps",
      "abstraction": "enum_values",
      "variants": ["BOOT_IDLE", "BOOT_FUSE", "BOOT_FW_RST", "BOOT_WAIT", "BOOT_DONE", "UNDEF"],
      "value_map": [
        { "name": "BOOT_IDLE",   "value": 0 },
        { "name": "BOOT_FUSE",   "value": 1 },
        { "name": "BOOT_FW_RST", "value": 2 },
        { "name": "BOOT_WAIT",   "value": 3 },
        { "name": "BOOT_DONE",   "value": 4 }
      ],
      "note": "Catch-all UNDEF variant captures the 3 unmatched encodings (3'b101 / 110 / 111) the CWE-1245 bug admits."
    }
  ],
  "inputs": [
    { "name": "fw_update_rst_wait_cycles", "abstraction": "ignored" },
    { "name": "BootFSM_BrkPoint",          "abstraction": "ignored" },
    { "name": "BootFSM_Continue",          "abstraction": "ignored" }
  ]
}
```

If this completes and the property fails on `pre_fix.sv` / holds on `post_fix.sv`, it becomes the first auto-extracted PoC against a real public upstream bug — the proof-by-fire effort's headline deliverable.

**Risk.** The `enum_values` mapping must round-trip correctly through Yosys's BTOR2 emission (specifically, the synthesized state cell's NID must match the symbol `boot_fsm_ps` after `flatten` + `dffunmap`). Two minor variants to try if the primary fails:

- Skip the `boot_fsm_ps` enum entry — keep `wait_count` collapsed but let the BTOR2 reader enumerate `boot_fsm_ps` over its raw 3-bit space (8 values, 5 reachable, 3 UNDEF). The property formula then refers to `boot_fsm_ps == 5 || boot_fsm_ps == 6 || boot_fsm_ps == 7`.
- Collapse `wait_count` more aggressively to `ignored` (pinned to 0). Sound for the safety properties; possibly unsound for the liveness `recoverable_to_idle` if it requires `wait_count` to reach 0 from a nonzero value.

### Action B — Test `mununu sv discover` on the sv2v-preprocessed Verilog

**Impact: medium (autonomy lever). Feasibility: empirical.**

The custom SV parser does not accept the upstream Caliptra source directly (same dialect blocker that Phase 1's sv2v solved for Yosys). Whether it accepts the *sv2v-preprocessed* Verilog-2005 output is an empirical question worth answering:

```bash
sv2v -I/tmp/caliptra_retry soc_ifc_pkg.sv soc_ifc_reg_pkg.sv soc_ifc_boot_fsm_pre_fix.sv > preprocessed.v
mununu sv init preprocessed.v
mununu sv discover --features smt preprocessed.mununu.json
```

If `sv init` parses and `sv discover` populates `discovered_values`, the BTOR2 path picks it up automatically (resolver path documented in §1.4). This becomes the autonomy lever for any future modern-SV target — no manual signal-by-signal classification.

If `sv init` rejects the preprocessed Verilog, Action B is moot and the autonomy gap is a separate, larger engineering task (outside this analysis's scope).

### Action C — Defer engine-runtime work to a separate phase

**Impact: high (long-term). Feasibility: weeks.**

Phase 1.6's runtime-performance finding observed that even with the input cap cleared, the 33.5 M-transition enumeration accumulated 4.6 GB RSS in 22 min on a release build. The cause is the per-transition `Env` HashMap allocation in `make_step_env` ([`bit_blast.rs`](../../crates/mununu-core/src/adapter/btor2/bit_blast.rs)).

Action A reduces enumeration by 4 000×; if it succeeds, the runtime-performance gap is no longer on the critical path for Caliptra. The general engine-runtime improvements (reusable per-step buffers, evaluated-node caching, compositional decomposition) remain valuable for designs that don't compress as well, but they are not gating Caliptra and should not be done as part of Phase 1.7.

If Action A fails despite the matrix's predictions, then the engine-runtime work becomes the next phase.

### Recommendation summary

| Order | Action | Impact | Cost | If it succeeds |
|---|---|---|---|---|
| 1 | **A** — refined sidecar with `wait_count` + `boot_fsm_ps` abstractions | high | hours | First end-to-end real-bug PoC; ship as `examples/verify/sv_yosys_caliptra_rtl_150/` |
| 2 | **B** — try `sv discover` on sv2v output | medium | empirical | Autonomy lever for future RTL targets |
| 3 | **C** — engine runtime work | high (long-term) | weeks | Necessary for designs that don't compress; not on Caliptra's critical path |

**Suggested first move.** Action A, gated on a small subsequent commit (Phase 1.7b in the active plan) that lands the refined sidecar + a `validate.sh` + a byte-deterministic transcript under `examples/verify/sv_yosys_caliptra_rtl_150/`, with full provenance metadata per the [claims-integrity policy](../policies/claims-integrity.md).

---

## See also

- [`docs/design/proof-by-fire-findings.md`](proof-by-fire-findings.md) — predecessor findings doc; Phase 1.6 runtime-performance note.
- [`docs/abstraction.md`](../abstraction.md) — memory soundness matrix; the standard reference for declaring abstraction postures in `verify.toml` and `.mununu.json`.
- [`docs/policies/claims-integrity.md`](../policies/claims-integrity.md) — applies if the Caliptra PoC ships as a public example.
- [`examples/verify/sv_yosys_caliptra_rtl_150/`](../../examples/verify/sv_yosys_caliptra_rtl_150/) — hand-modelled CWE-1245 staging with the property formulas this doc references.
