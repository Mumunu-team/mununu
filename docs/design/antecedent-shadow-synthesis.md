# Antecedent shadow-register synthesis in the exact-symbolic engine

> **Status: SUPERSEDED and removed (2026-10-14, mununu#677).** Inputs-in-the-state (mununu#656, `adapter/btor2/input_state.rs`) gives an input-derived antecedent its same-cycle value as a state predicate, which is the semantics this design approximated with a sampled register — and the implementation of this design was wrong by one cycle: the argument below is for `AG(S → C)`, the code renamed the atom and KEPT the `[] C`, so the engine checked the consequent one cycle late (`A@N → C@N+2`). Invisible on sticky consequents, refuted every one-shot one (monono `dram_preload` `waddr_q == 0 |=> waddr_q == 1`), and masked until #656 removed the refusal that had routed those properties to the cube engine. The module, its tests and the `prepare` step are gone; `--no-antecedent-shadow` / `no_antecedent_shadow` / `MUNUNU_NO_ANTECEDENT_SHADOW` are accepted and ignored. Kept as history.
>
> **Status (original):** design (2026-08-22). Supersedes the Phase A refusal for `sv verify-auto`. Refusal remains the fallback when a shadow cannot be synthesized soundly.
> **Owner:** exact-symbolic engine (`crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs`) + SVA lift (`crates/mununu-core/src/adapter/slang/verify_auto.rs`).
> **Fixes:** mununu#476 (the deeper form — the transitive refusal from Phase A becomes fallback-only).

## Context

The exact-symbolic engine leaves primary inputs FREE — they are quantified out per modality step. This is correct for state-only atoms, but it decouples input references across time: `A@N` (the antecedent evaluation) and `A@N+1` (the transition read of the same physical signal) share no constraint. For SVA `A |=> C` where `A` reads inputs — directly OR through combinational logic — the model checker returns a spurious verdict (`Violated (1 cell)` on correct RTL was the monono#476 report; the Phase A transitive refusal made this a `Skipped` instead).

Combinational-of-input antecedents dominate real SVA — bus/handshake gating (`valid_and_ready`), address decoding (`sel_a = (addr == BASE_A)`), enable stacks (`write_en = wr && cs && !halt`), cross-module chains (`rvalid_mine = rvalid && (rid == MY_ID)`). Refusal-only ships a strict Pareto improvement over the previous unsound `Violated`, but refuses a large fraction of real assertions. The proper fix — synthesizing an antecedent shadow register at SVA lift time — is a well-established technique (SymbiYosys, JasperGold, Vardi's SVA-to-BMC compilation) and is what this document specifies.

## Approach

At SVA lift time, when `A |=> C` is being turned into a mu-calculus formula, analyze `A`. If `A`'s combinational cone reaches primary inputs (transitively, stopping at register boundaries — same walker as Phase A: `parser::cone_inputs`), synthesize an **antecedent shadow register** in the BTOR2:

- New state cell `_mununu_antshadow_<N>` (fresh unique per synthesised atom).
- Sort = `A`'s sort (typically 1-bit Boolean).
- `init(_mununu_antshadow_<N>) = 0` — the SVA `|=>` semantics say cycle 0 has no prior antecedent, so the obligation is trivially satisfied when the shadow reads false.
- `next(_mununu_antshadow_<N>) = A_nid` — the shadow *samples* A each cycle.

Rewrite the lifted mu-calculus formula so the antecedent atom references `_mununu_antshadow_<N>` instead of `A`. Feed the augmented BTOR2 + rewritten formula to the exact engine. The engine treats the shadow as ordinary state — no more input decoupling.

If the shadow cannot be synthesized (fallback conditions below), fall through to the Phase A refusal.

## Soundness argument

Standard SVA-to-BMC reduction. The SVA fragment `A |=> C` at cycle N means "if A held at cycle N, C must hold at cycle N+1". Introducing shadow `S` with `next(S) = A, init(S) = 0` gives `S@N+1 = A@N` and `S@0 = 0`. The obligation `AG(S → C)` evaluated at cycle N+1 is equivalent to `AG(A@N → C@N+1)` for N ≥ 0, plus a trivially satisfied `AG(0 → C)` at cycle 0.

**Key point** — the shadow rewrite is **not sound as a general engine-internal transformation**. Applying it to `mu Y. (A or <> Y)` (bare `EF A`) would shift the semantics by one cycle. The shadow is safe only for the `|=>` shape, which the SVA lift knows about but the engine does not. The rewrite MUST happen at SVA lift time, with an explicit hint to the engine that the augmented model + rewritten formula are jointly one rewrite.

For non-SVA-lift entry points (direct `btor2 verify` with a user-authored mu-calc formula containing an input-derived atom), the engine has no shape hint and cannot safely shadow. The Phase A refusal remains the only sound answer there.

**Auxiliary sanity check** — the shadow rewrite preserves controllability, resettability, and reachability of every EXISTING state cell (it only ADDS a state cell with a well-defined init and next). No existing atom's semantics change. No existing verdict changes. All previously-decided properties on the augmented model produce the same verdict, and the newly-decidable properties (previously refused / previously unsound Violated) now decide with a verdict the SVA semantics validate.

## Boundaries — when the shadow is NOT synthesized

Fall back to the Phase A refusal when any of these hold:

- **Non-Boolean antecedent.** A wider-than-1-bit antecedent used in `|=>` is either a language misuse (SVA `|=>` is Boolean) or a multi-value pattern the shadow would misrepresent. Refuse.
- **Array / memory in A's cone.** BTOR2 memory reads model as inputs after havoc; a shadow over an array-dependent A would sample the free-input-model of the array, defeating the shadow. Refuse.
- **A is itself a primary input** (the Phase A hard-refusal case). Introducing a shadow that samples a single input is technically sound, but the resulting property is `AG(prev(A) → C)` which is rarely what the author meant. Refuse and let the author confirm the shape.
- **A's cone reaches an anonymous free input** (the `signal_reaches_anonymous_input` case at symbolic_bitblast.rs:3410). The partial-write havoc pattern is a different soundness posture; shadow-synth doesn't fix it. Refuse.
- **`--no-antecedent-shadow` flag on the CLI / `antecedent_shadow: false` in the API.** Explicit user opt-out for debugging or differential-oracle purposes. Refuse and cite the flag in the diagnostic.

**Multi-atom antecedents (extended 2026-08 post-mununu#476 shipping).** The detector was originally single-atom-only: `(a && b) |=> c` fell through to the Phase A refusal. It now walks any Boolean subtree under the antecedent-side `Not` — `And`, `Or`, nested `Not` — and collects every `Predicate` leaf as an independent shadow target. Soundness: independent shadows compose correctly for `And` (`shadow(A) ∧ shadow(B) = A@N ∧ B@N`), `Or` (`shadow(A) ∨ shadow(B) = A@N ∨ B@N`), and negation (`!shadow(A) = !A@N`). The five fallback conditions above still apply per-leaf — if any leaf is a bare primary input, that leaf falls through to the Phase A refusal even in a multi-atom context; the other leaves still get shadows. See `detect_pipeimplies_antecedent_atoms` in `mu_calculus/mod.rs`.

## Implementation shape

### New module — `crates/mununu-core/src/adapter/btor2/antecedent_shadow.rs`

```rust
pub struct ShadowSynthResult {
    /// The BTOR2 file with shadow registers appended.
    pub augmented: Btor2File,
    /// Original-atom-name → synthesized shadow name.
    pub renames: BTreeMap<String, String>,
    /// The atoms that fell through and require refusal.
    pub refused: Vec<RefusalReason>,
}

pub enum RefusalReason {
    NonBoolean { atom: String, width: usize },
    ArrayInCone { atom: String, mem_nid: Nid },
    IsPrimaryInput { atom: String },
    ReachesAnonymousInput { atom: String },
    UserOptedOut,
}

/// For each atom in `antecedent_atoms` whose cone reaches primary inputs,
/// synthesize an antecedent shadow register in the BTOR2 and record the rename.
/// Returns the augmented file and a rename map for the SVA-lift caller to apply
/// to the mu-calculus formula.
pub fn synthesize_shadows(
    file: &Btor2File,
    antecedent_atoms: &[String],
    opts: ShadowSynthOpts,
) -> ShadowSynthResult { ... }
```

### SVA lift integration — `crates/mununu-core/src/adapter/slang/verify_auto.rs`

The `|=>` shape is already detected by the SVA translator (per docs/design/native-sv-abstraction.md). At the point where the lifted mu-calc formula is being assembled, before handing to the engine:

1. Collect the antecedent atoms (already known from the parse tree).
2. Call `antecedent_shadow::synthesize_shadows(&file, &antecedent_atoms, opts)`.
3. If any shadows were synthesized, apply the rename map to the mu-calc formula (a straightforward AST walk).
4. Record the synthesis in the report (see interface).
5. Any `refused` entries fall through to the engine, which then hits Phase A's refusal path for those specific atoms.

### Engine integration — `crates/mununu-core/src/adapter/btor2/symbolic_bitblast.rs`

The Phase A refusal already lives here. It becomes the fallback path. No change to the refusal logic; the shadow-synth simply reduces the number of atoms that reach it.

Optionally add a diagnostic to the refusal message when a shadow was ATTEMPTED but fell back — so the user sees the specific reason (e.g. "shadow skipped: array in cone").

## CLI / API interface

**New CLI flag** on `sv verify-auto` and any command that invokes the SVA lift:

```
--no-antecedent-shadow    Disable auto-synthesis of antecedent shadow registers
                          for SVA `|=>` properties with input-derived antecedents.
                          Instead, refuse those properties with the Phase A message.
                          Debug / differential-oracle use only.
```

**New API field** on the `sv verify-auto` request:

```json
{ "antecedent_shadow": true }   // default; false to opt out
```

**New report fields** on `AutoVerifyReport`:

```json
{
  "antecedent_shadows": [
    {
      "atom": "mem_rvalid_mine",
      "shadow_name": "_mununu_antshadow_0",
      "source_inputs": ["mem_rvalid", "mem_rid"]
    }
  ],
  "antecedent_shadow_refusals": [
    { "atom": "arr_gt_thresh", "reason": "array_in_cone", "mem_nid": 47 }
  ]
}
```

Both arrays are empty in the common case (no input-derived antecedents). Downstream consumers (ROSF, monono) do not need to change their parser — the fields are additive; unknown fields must be ignored per the standard verdict-schema stability contract (`docs/api-schemas/verdict.md`).

**Verdict stability.** `PropertyVerdict` values do not change semantics. A property that previously returned `Skipped` under the Phase A refusal may now return `Holds` / `Violated` / `Unknown` — this is the intended behavior improvement. A property that previously returned an unsound `Violated` may now return the correct verdict.

## Test strategy

Four layers, all under `crates/mununu-core/`:

### Unit — `adapter/btor2/antecedent_shadow.rs::tests`

- **Detection**: an atom whose cone reaches inputs is flagged; a state-only atom is not.
- **Rewrite correctness**: the augmented BTOR2 has exactly one new state per input-derived atom; its init is 0; its next is the atom's NID.
- **Rename map**: the map covers every synthesized atom and no others.
- **Fallback**: array-in-cone / non-boolean / bare-primary-input all produce refusal entries without mutating the file.

### Integration — `adapter/btor2/symbolic_bitblast.rs::tests`

- **Monono `wb_mem_client` shape** (positive): the fixture that previously refused (Phase A) now DECIDES with a Holds verdict, and the report lists a shadow entry.
- **Bare `EF (input_derived)` regression**: engine-internal callers with no SVA-lift hint still refuse (soundness preservation).
- **Combinational-of-state atom** (regression): unchanged — no shadow synthesized, no refusal.
- **`--no-antecedent-shadow` opt-out**: with the flag, the same monono fixture refuses again with the Phase A message.

### Differential oracle — new test file `crates/mununu-core/tests/antecedent_shadow_differential.rs`

For a curated set of SVA `|=>` fixtures with input-derived antecedents:
- Verdict from exact-symbolic engine WITH shadow-synth
- Verdict from predicate-cube engine (which shadow-registers antecedents natively per the current refusal message's claim)
- Assert: definite verdicts agree; if one is ⊥, the other may be definite (portfolio coverage difference); a definite-vs-definite disagreement raises a soundness alarm.

The differential test is the key soundness argument — it independently verifies the shadow-synth against a peer engine using a different technique for the same problem. Run under `mununu-sva` per CLAUDE.md §"SVA-verification e2e validation (slang)".

### End-to-end — extends `crates/mununu-core/tests/verify_auto_sweep.rs` (or equivalent)

Full pipeline `slang → BTOR2 → shadow-synth → exact engine → verdict`, on:
- The monono `wb_mem_client` reduced fixture (add to `examples/verify/`)
- A ready-valid handshake with `assert property (valid && ready |=> next_state == X)`
- An address-decoder with `assert property ((addr == BASE) && wr |=> sel_a)`

Each asserts the expected verdict AND the presence of a shadow entry in the report.

## Cross-engine implications

- **Predicate-cube engine.** No change. It already shadow-registers antecedents (per the current refusal message). The differential oracle test exercises it as-is.
- **`btor2 verify` direct path.** No change. Users writing hand-authored mu-calc formulas over BTOR2 files bypass the SVA lift → no shadow-synth → Phase A refusal still applies to input-derived atoms.
- **Synthesis (`context synth`).** No change. Synthesis reads mu-calc formulas the user writes; no SVA lift; no shadow-synth. If a user writes a formula with an input-derived atom for synthesis, the Phase A refusal (from the exact-symbolic engine when it's used as an oracle in the synthesis pipeline) is the right answer.
- **Recoverability (`AG EF good`) path** at `adapter/recoverability.rs`. Uses the exact engine; recoverability formulas typically have no input-derived antecedents. If they do, refusal remains the answer — no shadow-synth for non-`|=>` shapes.

## Cross-repo impact

- **ROSF** (subprocess `--profile industrial`). Verdict quality improves — previously `Skipped` properties may now decide. Report parsing is additive-safe. No Docker rebuild required unless a specific mununu version is pinned.
- **Monono** (direct `sv verify-auto` calls). Same story. The `wb_mem_client` case + every combinational-of-input antecedent in monono's tree will now decide instead of refuse. Any code that treated `Skipped` as "we can't help you" should now expect definite verdicts on that class.

## Docker rebuild determination

| Image | Impact | Rebuild required? |
|-------|--------|-------------------|
| mununu `Dockerfile` (prod) | binary carries new engine behavior + new report fields | Yes for consumers that pin a version tag |
| mununu `Dockerfile.dev` | binary bump | Only for the dev workflow that requires the new binary |
| mununu `Dockerfile.sva` | binary bump; e2e tests exercise the differential oracle (mandatory under this image per CLAUDE.md §"SVA-verification e2e validation") | **Yes** — mandatory rebuild + full slang-gated e2e test run under this image before merge |
| mununu `Dockerfile.extract`, `.extract-*` | no impact | No |
| rosf `Dockerfile` | consumes mununu subprocess; verdict quality changes on input-derived antecedents | Only if pinned to a version tag; behavior updates on next binary pull otherwise |
| rosf `Dockerfile.dev`, `.hw` | no direct dependence on the changed paths | No |

## Rollout plan

1. Land the shadow-synth module + SVA lift integration + refusal-as-fallback + unit tests on `fix/476-antecedent-shadow-synth` (rename of Phase A branch).
2. Run the differential oracle under `mununu-sva` image; require green before merge.
3. Update `docs/verifying-rtl.md` and `docs/api-schemas/verdict.md` (from the Phase C wide-doc pass) to describe shadow-synth and the new report fields.
4. Update `wiki/RTL-Verification-Pipeline.md` with the new pipeline diagram (SVA lift → cone analysis → shadow-synth → exact engine).
5. Update the cross-repo prompt (`docs/consumer-briefings/2026-09-open-issue-sweep.md`) to say: refusal is now the fallback, not the default; shadow-synth is the primary path.
6. New policy file `docs/policies/cross-repo-impact.md` — enforce the Docker-rebuild-decision table format on every PR that changes engine behavior. Referenced from CLAUDE.md.
7. File the follow-up mununu#477 (verify cube engine also handles input antecedents correctly — differential oracle for the differential oracle). Not blocking this PR.

## Non-goals

- No change to the SVA fragment supported.
- No change to `PropertyVerdict` values or their `as_str()` shape.
- No change to the predicate-cube engine.
- No change to the recoverability, synthesis, or KMTS-3-valued paths.
- No implementation of shadow-synth for `A |-> C` (immediate-implication SVA) in this PR — same-cycle sampling is a different shape; separate design.
