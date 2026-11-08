# STS-IR — the frontend-agnostic abstraction seam

> Status: **IR-track shipped + CLOSED at P3 (2026-06-24).** DR0→P3 shipped: the
> STS-IR seam ([`crates/mununu-core/src/adapter/sts_ir.rs`](../../crates/mununu-core/src/adapter/sts_ir.rs))
> + both lift engines consuming it + the unified `eval_node_generic` evaluator +
> `mununu verify` routing through the predicate-cube / 3-valued path. **P4/P5
> were evaluated and NOT pursued** — see §8 (the load-bearing verification-path
> unification was the goal, and it shipped at P1–P3). Roadmap context (local):
> [`ir-track-p4-p5-scoping-2026-06-23.md`](../archive/plans/ir-track-p4-p5-scoping-2026-06-23.md) (a planning note, published under `docs/archive/`).

## 1. Why

Today mununu has two abstraction/lift engines that both consume BTOR2 but share no interface:

- **`bit_blast`** — explicit register-cross-product enumeration → Sharp `Clts`
  (`crates/mununu-core/src/adapter/btor2/bit_blast.rs`).
- **`predicate_cube_lift`** — predicate-cube → 3-valued may/must KMTS
  (`crates/mununu-core/src/adapter/btor2/kmts_lift.rs:1427`), reachable today only via
  `btor2 cegar` / `/btor2/cegar`.

`predicate_cube_lift` `parser::parse`s BTOR2 *internally*, so nothing but BTOR2 can feed it. The
STS-IR is the **narrow waist** that decouples the abstraction engines from the BTOR2 frontend:

```
frontend ──lower──▶ STS-IR ──┬─ Enumerate strategy ─▶ Sharp CLTS ─┐
 (BTOR2 ~1:1)                 └─ SmtImage strategy ──▶ KMTS        ─┴▶ one Clts ─▶ eval_node_generic<D: EvalDomain>
```

A frontend that can present the two semantics below inherits *both* abstraction policies + the
KMTS evaluator + CEGAR with no new abstraction code. BTOR2 maps to the IR ~1:1 (it already *is* a
symbolic transition system). Value-rich non-RTL frontends (Promela, a C subset) *could* lower to
it — that was the planned P4/P5 — but P4/P5 were evaluated and **not pursued** (§8): those
frontends are already fully enumerable, so lowering them yields no verdict gain, and
`AbstractionType`/`FieldDomain` stay regardless (they are load-bearing for the BTOR2/SV bit-blast
sidecar path). Discrete frontends keep their direct-CLTS path and never touch this seam.

## 2. The interface (two semantics over shared metadata)

Defined in [`adapter/sts_ir.rs`](../../crates/mununu-core/src/adapter/sts_ir.rs):

```rust
pub struct StsVar { pub name: String, pub width: u32 }

pub trait SymbolicTransitionSystem {
    fn state_vars(&self) -> Vec<StsVar>;
    fn input_vars(&self) -> Vec<StsVar>;
}

// Concrete one-step semantics — the Enumerate (explicit) strategy needs only this.
pub trait StepEval: SymbolicTransitionSystem {
    fn step(&self, state: &HashMap<String,u128>, inputs: &HashMap<String,u128>)
        -> Result<HashMap<String,u128>, AdapterError>;
}

// SMT predicate-image — the SmtImage (predicate-cube) strategy needs this.
pub trait SmtEncode: SymbolicTransitionSystem {
    fn may_edges(&self, predicates: &[PredicateSpec], timeout_ms: u32) -> Vec<(usize,usize)>;
    // must-relation (∀∀ / ∀∃ / hyper) follows the same shape — see §5.
}
```

### No BTOR2 / Z3 leakage — the load-bearing property

The whole seam is expressible without naming a BTOR2 or Z3 type:

- structure is `StsVar` (name + width);
- the concrete step is name-keyed `HashMap<String,u128>` in and out;
- the SMT predicate-image is expressed over `PredicateSpec` (`{name, register, value}`, where
  `register` is a *symbol name*, not a NID) and returns plain `(usize, usize)` cube-index pairs.

`Btor2File`, `Btor2SmtView`, `Nid`, and the `z3::*` types stay entirely behind `BtorSts`. This is
the DR0 gate: if the seam could not be stated without leaking BTOR2, the IR would be a fiction.

## 3. The BTOR2 implementation (`BtorSts`) — pure delegation

`BtorSts<'a>(&'a Btor2File)` implements all three traits by delegating to already-shipped, public
functions — **DR0 changes no behaviour and rewires no call site**:

| Trait method | Delegates to |
|---|---|
| `state_vars` / `input_vars` | `parser::collect_symbols` + `parser::bv_width` over `Node::State`/`Node::Input` |
| `StepEval::step` | `bit_blast::simulate_one_step` (name-keyed, unchanged) |
| `SmtEncode::may_edges` | `with_z3_config` → `btor2_encode::encode_design` → `smt_must_edge::build_register_nid_map` → `smt_per_target_may_check` (the exact batched pattern at `kmts_lift.rs:1634`) |

## 4. How each engine consumes the seam (the consumption sketch)

- **`bit_blast` → `StepEval` (Enumerate strategy).** Explicit enumeration is "for each reachable
  state assignment, for each input combination, `step()` to the next assignment, add a Sharp
  edge." That is exactly `StepEval::step` in a loop bounded by `state_vars()` widths. `bit_blast`
  becomes the *Enumerate edge-strategy* over any `StepEval`, not a BTOR2-specific engine.
- **`predicate_cube_lift` → `SmtEncode` (SmtImage strategy).** The `MayEdgeInference::SmtAllPairs`
  block at `kmts_lift.rs:2119` *is* `SmtEncode::may_edges`. Behind the trait it becomes
  `for (i,j) in sts.may_edges(&predicates, t) { builder.transition(..MayOnly..) }` — the lift no
  longer parses BTOR2 or touches Z3; it consumes an `&dyn SmtEncode`.

Both consumption paths produce the *same* `Clts` (a Sharp CLTS is a degenerate KMTS), fed to the
one evaluator family (`BoolDom` cheap path / `KleeneDom` sound-abstraction path).

### 4.1 The unified predicate-cube lift entry (U.1–U.4, 2026-06-26)

The predicate-cube lift had **two** implementations with verbatim-duplicated
per-cube logic — the eager `predicate_cube_lift` and the lazy `LazyLift` +
`materialize_clts_from_lazy`. The lift-unification refactor consolidated them so
they share one core and dispatch through one gated entry:

| Concern | Where it lives now |
|---|---|
| eager-vs-lazy dispatch + compound gate | one `lift_predicate_cube(predicates, btor2, opts, lift_opts, strategy)` entry |
| per-cube sampling (representative → simulate → re-evaluate → target cube) | one `cube_sampling_edges()`, shared by the eager loop + lazy `compute_cube_outgoing_edges` |
| sampled-target must post-pass (4 strategies) | one `apply_sampled_must_inference()`, shared by both |
| `SmtAllPairs` may + SMT must (compound-aware) | **eager-only** — a global Z3 query, not per-cube |

**Strategy decision.** The may-relation is one of:

- `SmtAllPairs` — sound global all-pairs SMT may-relation (`SmtEncode::may_edges`).
  Eager-only (global query); the only path that honours **compound predicates**
  (via `PredicateLike::expr`).
- `Off` (sampling) — the per-cube `cube_sampling_edges` body; simple
  `register==value` atoms only; runs on both Eager + Lazy.

**Eager ≡ Lazy guarantee.** On the sampling path the eager and lazy lifts produce
byte-identical CLTSes — asserted by the `u1_eager_lazy_differential_equivalence`
test (the permanent anti-divergence guard). `SmtAllPairs` is outside this
equivalence (it is eager-only-global).

**Compound rule.** Compound predicates require `Eager` + `SmtAllPairs`. The
sampling representative inverse can't realise `a==0 && b==0`, and the lazy body
never consults `compound_exprs`; `ensure_compound_lift_supported()` enforces this
at the entry (and via thin defensive copies in each impl for direct callers).

### 4.2 The third consumer — symbolic (BDD) engine (R-F5, planned)

The seam admits a **third** strategy beyond `StepEval` (Enumerate) and `SmtEncode`
(explicit predicate-cube): a **symbolic** engine that builds the may/must transition
relation as BDDs and evaluates the mu-calculus by fixpoint image/preimage, never
materialising the `2^|P|` cube states. It is the R-F5 track: R-F5.0 (the OxiDD spike,
`mu_calculus/symbolic.rs`) is shipped and validated cell-for-cell against
`evaluate_tri`; R-F5.1–.4 (BDD `TritSet`, symbolic modal step, symbolic relation
construction from BTOR2, verify-auto wiring) are planned. It plugs into the *same*
seam — the symbolic relation-builder consumes the transition-system view, not BTOR2
directly. Full picture:
[`docs/design/post-rf5-architecture.md`](post-rf5-architecture.md).

## 5. Z3 scope & batching (why `may_edges` is batched, not per-pair)

Z3 calls must run inside a `z3::with_z3_config` scope, and the efficient pattern builds the
`encode_design` view **once** and reuses it across all cube-pair queries (the `kmts_lift.rs:1634`
loop). So the trait exposes a **batched** `may_edges(predicates) -> Vec<(src,tgt)>` rather than a
per-pair `may_edge(src,tgt)` — the impl owns the scope and the view lifetime, and the caller never
sees Z3. The must-relation methods (P1) take the same batched shape over
`smt_must_edge::smt_per_target_must_check{,_standard}` / `smt_hyper_must_check`.

DR0's `may_edges` uses the BvOnly `encode_design`; **P1 swaps in the memory-aware
`encode_design_for_lift`** (array theory for `$mem` cells) — a one-line change behind the trait,
invisible to callers.

## 6. What DR0 ships vs what the P-phases do

- **DR0 (this):** the traits + `BtorSts` (delegating) + tests proving `step` and `may_edges` work
  through the seam, with no leakage and no call-site changes. Plus this note.
- **P0:** promote the stub to the real seam (move the SMT/step helpers behind the trait as the
  canonical interface); BTOR2 stays the only impl; behaviour-preserving.
- **P1:** reroute `bit_blast` (Enumerate) and `predicate_cube_lift` (SmtImage) to consume
  `&dyn StepEval` / `&dyn SmtEncode` instead of `Btor2File` directly; add the batched must methods.
  - **P1 #1 (shipped):** `predicate_cube_lift` + `LazyLift::from_btor2` now resolve every
    predicate's `register` through `SymbolicTransitionSystem::resolve_register` (via a shared
    `resolve_predicate_registers` helper) *before* binding — a predicate over a symbol-stripped
    alias (the uart_tx `bit_cnt_q` → canonical state cell `bit_cnt_d`, the DR1 #1 blocker) now
    binds to the real register across the `simulate_one_step` map, the `next_registers` readback,
    `pred_register_widths`, and the SMT `build_register_nid_map`. Direct hits are kept; aliases
    are rewritten; unresolvable names still error. This is the first call site to consume the seam.
  - **P1 #2 (shipped):** `predicate_cube_lift`'s `SmtAllPairs` may-block consumes
    `SmtEncode::may_edges` instead of re-inlining the encode → nid-map → all-pairs Z3 loop —
    de-dups the two copies onto the single seam implementation.
  - **P1 #3 (shipped):** added `SmtEncode::must_edges` (the canonical ∀∃ KMTS must-relation,
    all-pairs, sound under-approximation, `R_must ⊆ R_may`). The eager `predicate_cube_lift`
    composes it: `SmtAllPairs` may + a non-Off must inference now promote each must-edge
    `MayOnly` → `Sharp`, yielding a KMTS with both relations — the prerequisite for sound DEFINITE
    3-valued verdicts (closes DR1 F5; previously the must post-pass only consumed the sampling
    pass's candidate set, which `SmtAllPairs` bypassed).
  - **P1 #4 Phase 1 (shipped):** widened `StepEval` for the `bit_blast` (Enumerate) reroute, per
    the **Q2 = B** decision (observability-rich contract, clamping stays policy). Added
    `StepOutcome { next_state, observed, admissible }` + `StepEval::step_observe(state, inputs,
    observe)`; `step` is now a provided default over it (next-state only, discards
    observability/admissibility — the pre-Phase-1 contract). `BtorSts::step_observe` delegates to
    the new `bit_blast::simulate_one_step_observe`, which reports the requested combinational
    signals' current-cycle values + whether the BTOR2 `constraint` lines hold. The trait never
    mentions `FieldDomain` — domain-encoding / OOB-sink / state-splitting stay caller-side policy.
  - **P1 #4 Phase 2a (shipped):** aligned `simulate_one_step_observe`'s observe resolution with the
    Enumerate strategy's combinational-signal resolution — `Op` → own value
    (`combinational_signal_nids`), else `Output` → referenced-signal value (`output_port_nids`),
    else `State`/`Input` → own value; precedence `Op > Output > State/Input`. Makes a future Pass-1
    reroute *value-faithful* (the `cv` mask reads the same signals it does today) and is a
    correctness fix on its own (output-port observation now follows the signal).
  - **P1 #4 Phase 2b+2c (shipped):** `enumerate_and_blast`'s Pass 1 now consumes the `StepEval`
    primitive. **2b** a `PreparedStep` (builds `collect_symbols` + the state/input lists + the
    observe-resolution maps ONCE) so the hot loop carries no per-step setup cost; `simulate_one_step_observe`
    is now a build-once-step-once wrapper over it. **2c** Pass 1 routes each `(cube, input)` through
    `PreparedStep::step` (admissibility + the `cv` mask + the next register-state from one outcome);
    the domain-encoding / OOB-sink / state-splitting stay enumerator-side policy (`cells.encode`,
    `__mununu_oob__`). Correctness-by-construction: `PreparedStep`'s state/input lists are
    `file.states()` / `file.inputs()` order — the same as `state_meta` / `input_meta` — so position
    `i` aligns and the seeding is identical to the old `make_step_env`. Gates: the full
    `cargo test -p mununu-core` sweep is verdict-equivalent (exit 0); no perf regression — the
    reroute *removes* the per-step full-env `clone()` and adds only bounded O(states+inputs) maps.
    Phase 3 = a non-RTL frontend inherits Enumerate (the P4/P5 payoff).
- **P2:** unify the two evaluator bodies (`eval_node` BitVec / `eval_node_tri` TritSet) over one
  generic body (orthogonal to the IR, same track). NOTE: the per-element `truth_domain` trait is a
  dead R.1 artifact the R.3 evaluator bypassed; P2 builds a NEW **bulk** `EvalDomain` trait
  (associated `Valuation` = `BitVec` | `TritSet`, whole-set ops) instead. See
  `docs/design/evaluator-domain-unification.md`.
  - **P2.1 (shipped, #111):** design note.
  - **P2.2 (shipped, #112):** `EvalDomain` + `BoolDom` + `eval_node_generic`/`eval_fixpoint_generic`;
    the 2v `eval_node`/`eval_fixpoint` now delegate to `::<BoolDom>`. HARD gate met (2083/2083
    crate tests; criterion "no change" on `mu_calculus_evaluate` |S|=2048/8192).
  - **P2.3 (shipped):** `KleeneDom` (TritSet); `eval_node_tri` now delegates to `::<KleeneDom>`;
    the hand-written 3v body + `eval_fixpoint_tri` are deleted. Both 2v + 3v now run ONE generic
    body. Gate met (2083/2083 incl. `r3_kleene_baseline`; trit benches no regression). P2.4 next:
    retire the dead per-element `truth_domain`.
- **P3 (shipped):** `mununu verify` SV/BTOR2 routes through the IR + chosen strategy (the cube /
  3-valued path when the sidecar declares `predicates`); 3-valued verdicts carried into
  `PropertyVerdict` (#123–#125). This is the IR-track's load-bearing payoff — the *verification*
  path now runs on one IR + one evaluator.
- **P4/P5 — EVALUATED, NOT PURSUED (2026-06-24).** See §8.

## 7. Open design questions for the Go/No-Go review

1. **Must-relation surface.** DR0 ships `may_edges` only; confirm the three must variants
   (`SmtPerTarget` ∀∀, `SmtPerTargetStandard` ∀∃, `SmtHyperMust`) all fit the same batched shape
   without a richer return than `Vec<(usize,usize)>` (hyper-must needs target *sets*).
2. **Enumerate strategy via `StepEval` only.** Confirm `bit_blast`'s sidecar-driven field
   clamping (Ignored / width bounds / OOB sink) is expressible as a policy *over* `StepEval`
   rather than needing more of BTOR2 — or whether the IR must also carry an init/constraint
   accessor. (DR0 omits `init`/constraints; P1 decides if they're needed at the seam.)
3. **Lowering cost.** *(Resolved 2026-06-24 — see §8.)* The measurement gate found the cost not
   worth the (nil) verdict benefit; P4/P5 were not pursued.

## 8. Why the IR-track closed at P3 (P4/P5 not pursued, 2026-06-24)

The IR-track's load-bearing goal was to unify the **verification** path onto one
IR + one evaluator. That shipped: DR0→P3 gave the STS-IR seam, both lift engines
consuming it, the single generic `eval_node_generic` evaluator, and `mununu verify`
routing through the predicate-cube / 3-valued path. P4/P5 (lower a value-rich
non-RTL frontend onto the seam, then retire the legacy abstraction types) were
the *flexibility-proof* tail. A code-grounded measurement (`docs/design/sts-ir.md`
§7.3's gate, recorded in [`ir-track-p4-p5-scoping-2026-06-23.md`](../archive/plans/ir-track-p4-p5-scoping-2026-06-23.md) (a planning note, published under `docs/archive/`))
found them not worth building:

1. **No verdict gain.** Promela (and the C-extraction subset) are already fully
   enumerable — bounded vars + control states form a finite cross-product
   (`promela_var_to_domain` → `FieldDomain::with_range`). Lowering them to
   `StepEval` (the Enumerate strategy) reproduces the *same* explicit enumeration
   through a different code path; no property verdict changes. The `SmtEncode`
   (predicate-image) half is inapplicable — these frontends have no bit-vector SMT
   encoding of their transition relation, and building one is a large lift with
   marginal real-world need.

2. **P5's deletion is blocked.** `AbstractionType` and `FieldDomain` are **not**
   dead code awaiting retirement — they are load-bearing for the **BTOR2/SV
   bit-blast sidecar** bounded-counter path (`bit_blast.rs` `resolve_to_field_domain`,
   `adapter/sidecar/`, `AbstractionType::Ignored` cone-drop), which the M.1 / M.4 /
   R46 fixture fleet depends on and which does *not* lower through the seam (BTOR2
   *is* the IR ~1:1). They stay. Only `state_enum` (one cross-product utility) would
   be deletable, and only after *both* Promela and C-extraction migrate — a marginal
   prize for a multi-session, two-frontend migration with no verdict gain.

Net: P4/P5 were a marginal-payoff, partly-blocked architectural exercise that the
plan's own measurement gate (and the §6.7 anti-gold-plating discipline) rules out.
The IR-track is **complete at P3**; `AbstractionType` / `FieldDomain` are permanent.
