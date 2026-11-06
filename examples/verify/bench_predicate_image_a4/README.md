# Phase A.4 — Predicate-image discovery benchmark

> **Status: curating.** Step 4.0 of
> [`.claude/plans/phase-a4-predicate-image.md`](../../../.claude/plans/phase-a4-predicate-image.md).
> 10 fixtures listed in [`fixtures.toml`](fixtures.toml); the recall
> harness that consumes this manifest lands in step 4.3.

## What this directory contains

A manifest-driven benchmark suite that measures the
predicate-image discovery algorithm's *recall* against a curated
ground-truth baseline. Each fixture entry in
[`fixtures.toml`](fixtures.toml) declares:

- **`path`** — the source file the adapter ingests (SV or BTOR2).
- **`adapter`** — the mununu adapter to run.
- **`significant_values_expected.<signal>`** — the integer constants
  we **expect** the predicate-image to discover for each signal.

The recall harness (a `cargo test --test predicate_image_recall`
integration test landing in step 4.3) runs the algorithm on every
fixture and asserts

```
recall = |discovered ∩ expected| / |expected|  ≥  threshold
```

per fixture, where `threshold = 0.80` for non-adversarial entries
(0.95 on bug-bearing fixtures whose verdict closure depends on the
discovered set). Adversarial entries (`category = "adversarial"`)
additionally assert *soundness* — the discovered set must **not**
include unreachable values the syntactic seed extractor might
spuriously surface.

## Fixture groups

| Group | Count | Purpose |
|---|---|---|
| **A** — real upstream bugs | 2 | Caliptra `soc_ifc_boot_fsm` pre/post fix; headline proof-by-fire |
| **B** — synthetic CWEs | 3 | safety_demo + CWE-1245 + CWE-1260; in-tree, small |
| **C** — negative cases | 3 | handshake / traffic_light / fair_arbiter; property should hold |
| **D** — external benchmark | 1 | Pono `arbitrated_top_n2_w2_d2_e0.btor2` (fetched on demand) |
| **E** — adversarial | 2 | Hand-authored: cap_overflow + sparse_predicates |

Total: **10 fixtures**.

## Honest scope statement

- **The Caliptra discriminator pair (Group A)** is the headline proof-by-fire
  case. Phase A.4's done criterion is the initial-state verdict flip on
  `pre_fix.sv` `no_undef_reachable`. See the parent plan §"Proof-by-fire target".
- **The synthetic CWE fixtures (Group B)** demonstrate the algorithm against
  small, hand-controllable defect classes. They are **demos**, not findings,
  per [`docs/policies/claims-integrity.md`](../../../docs/policies/claims-integrity.md).
- **The negative cases (Group C)** are the smoke tests for the
  no-spurious-discovery property — a bug-free design should not produce
  alarming verdicts.
- **The external Pono benchmark (Group D)** is the only fixture that
  requires downloading. The `fetch_url` field in the manifest is the
  canonical source; the recall harness skips this fixture when the file
  is not present locally (CI does not download external content).
- **The adversarial fixtures (Group E)** are hand-authored to exercise
  the algorithm's failure modes:
  - `cap_overflow.btor` — saturating counter that drives `bad` at 200
    (well past typical 16-value caps; exercises Bryant-Kroening
    under-approximation).
  - `sparse_predicates.btor` — two `bad` signals at non-contiguous
    encodings; the predicate-image must *reject* the unreachable one.

## Reproducing the recall measurement

Once step 4.3 lands:

```bash
cargo test --test predicate_image_recall -- --nocapture
```

For a single fixture:

```bash
cargo test --test predicate_image_recall caliptra_pre_fix -- --nocapture
```

## Files

| Path | Purpose |
|---|---|
| [`fixtures.toml`](fixtures.toml) | Canonical 10-entry benchmark manifest |
| [`adversarial/cap_overflow.btor`](adversarial/cap_overflow.btor) | Saturating 8-bit counter, bad@200 |
| [`adversarial/sparse_predicates.btor`](adversarial/sparse_predicates.btor) | Non-contiguous bad encodings under parity constraint |
| `external/` | Reserved for the Pono download (gitignored; not checked in) |

## Elaboration check (step 4.0 verification)

All 9 in-tree / synthetic fixtures elaborate cleanly on the Phase A.3
baseline. Verified 2026-05-20:

| Fixture | Adapter | State count | Status |
|---|---|---|---|
| `caliptra_pre_fix` | sv-yosys | 4 096 | ✅ |
| `caliptra_post_fix` | sv-yosys | 4 096 | ✅ (same source tree) |
| `safety_demo_btor` | btor2 | 16 | ✅ |
| `cwe1245_fsm_bug` | systemverilog (custom) | 4 | ✅ |
| `cwe1260_addr_overlap_bug` | systemverilog (custom) | 5 | ✅ |
| `handshake` | systemverilog (custom) | 4 | ✅ |
| `traffic_light` | sv-yosys | 256 | ✅ (custom SV parser rejects; sv-yosys accepts) |
| `fair_arbiter` | sv-yosys | 32 | ✅ (custom SV parser rejects; sv-yosys accepts) |
| `cap_overflow` (adversarial) | btor2 | 256 | ✅ |
| `sparse_predicates` (adversarial) | btor2 | 8 | ✅ |

The Pono external fixture (`pono_arbitrated_top_n2_w2_d2`, Group D)
is deferred until step 4.3 — recall harness skips fixtures whose
local file is absent so CI stays self-contained.

## Pending step-4.0 follow-ups

- [ ] Fetch + manually inspect the Pono variant (Group D) to fill in
      `significant_values_expected` for at least one named signal.
- [ ] The `significant_values_expected` baseline for `fair_arbiter.sv`
      (Group C) is a placeholder for a single-bit arbiter; refine to
      match the actual fixture's parameterisation once measured (the
      sv-yosys elaboration shows 32 states which suggests > 1 bit of
      state register — the actual baseline will be refined in step
      4.3's harness run).
