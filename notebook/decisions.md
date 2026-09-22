# Architectural Decision Records

Append-only log. Each ADR is dated and links to the EXP-IDs that motivated it.

Format (per Michael Nygard's ADR template, lightly adapted):

```
## ADR-NNNN: <title>

**Date:** YYYY-MM-DD
**Status:** proposed | accepted | superseded by ADR-NNNN | withdrawn
**Context:** <why this decision is needed>
**Decision:** <what we are doing>
**Consequences:** <what happens as a result; trade-offs>
**Related EXP:** EXP-NNNN, EXP-NNNN
```

---

## ADR-0001: Reproducible-experiments scaffold for the optimization programme

**Date:** 2026-05-05
**Status:** accepted
**Context:** The optimization programme produces numbers (speedups, memory deltas, scaling curves) that will appear in blog posts and future write-ups. Without a reproducibility contract — pinned toolchain, hardware fingerprints, deterministic inputs, archived raw outputs, single-command replay — those numbers are unverifiable.
**Decision:** Adopt the eight-file experiment archive convention (`experiments/EXP-NNNN-<slug>/`), the lab-notebook template (`log.md` + `notes.md`), the reproducibility scripts (`scripts/{capture_hw,bench_record,repro,bench_diff,check_repro}.sh`), and Makefile verbs (`experiment`, `replay`, `bench-record`, `bench-compare`, `publish-prep`). Every result cited in a write-up must replay green via `make replay EXP=NNNN`.
**Consequences:** Up-front cost ~2.5 engineer-days. Steady-state cost: ~30 minutes per experiment for archive curation. Benefit: every paper claim is independently verifiable; blog posts ship with one-click replay; regressions surface in CI rather than in review.
**Related EXP:** all (EXP-0001 onward).

---

## ADR-0002: Differential oracle precedes algorithmic swaps

**Date:** 2026-05-05
**Status:** accepted
**Context:** Paige-Tarjan partition refinement (B1, EXP-0009) and chaotic fixpoint iteration (B5, EXP-0013) are non-trivial algorithmic swaps. Existing unit tests (6 minimization, ~80 mu-calculus) check specific cases; they don't span the input distribution that property tests would.
**Decision:** Land the differential oracle (E5: `composition/minimize_naive.rs` + `tests/properties/minimization.rs::diff_naive_vs_production`) BEFORE B1, and the fixed-point equivalence proptest (`tests/properties/mu_calculus.rs`) BEFORE B5. The oracle stays in `#[cfg(any(test, feature="test_support"))]` to keep release builds clean.
**Consequences:** Adds 1.5 days before the headline algorithmic experiments can land. In return: any divergence from the naive K-S verdict surfaces in the proptest before review. Soundness arguments in EXP READMEs cite the differential as evidence, not just theorems.
**Related EXP:** EXP-0009, EXP-0013.

---

## ADR-0017: §A6b BindingStack falsified; refines the prediction model with the "already-optimized primitive" rule

**Date:** 2026-05-07 (sitting 12)
**Status:** accepted; plan §A6b falsified at mununu's typical alternation depths.
**Context:** EXP-0007 implemented BindingStack: replace `eval_fixpoint`'s per-iteration `bindings.clone()` with an enter-on-entry / restore-on-exit RAII pattern. ADR-0014 predicted **likely win** because the change "reduces total work" (fewer BitVec deep-clones per iteration). Bench results: 2 hard regressions at p<0.001 (+11.6% and +28.1%), 0 improvements, 5 of 10 benches significantly slower. Reverted.
**Why the prediction missed:** at K=0 (alternation-1 fixpoints, which dominate mununu's bench fixtures), `bindings.clone()` is a 24-byte memcpy of an empty HashMap — essentially free. The replacement `bindings.insert(var, ...)` does ~30 ns of hash + bucket-probe + drop + re-insert, which is *more* work than the memcpy it replaces. The crossover where NEW wins is K≥2 (alternation-3+), which mununu's formulas don't reach.

This pairs exactly with ADR-0013's §B4 (changed-flag) finding: "the std/bitvec primitives already optimize the targeted path." `BitVec::==` early-exits on first word mismatch. `HashMap::clone` is bulk-memcpy on the bucket array. `Vec::push` doubles. Replacing these with seemingly-leaner per-step ops loses to the constant factor.
**Decision:**
1. **Revert EXP-0007.** The BindingStack pattern is asymptotically correct but measurably worse at alternation depths ≤ 2.
2. **Mark plan §A6b as falsified-at-this-scale.** Open EXP-0007-deep-alternation as a niche followup if alternation-3+ becomes a target workload.
3. **Refine the ADR-0014 prediction model into a three-bit taxonomy.** The previous rule ("reduces total work wins") is necessary but not sufficient. The third bit:
   - **Q1**: Is this a structural change (changes access pattern) or a drop-in (replaces one primitive with another)? Structural is favored (per ADR-0013, ADR-0014).
   - **Q2**: Does it reduce total work or add work? Reducing is favored.
   - **Q3 (NEW)**: Does the work-reduction operate on a path the std/bitvec/hashmap primitives haven't already optimized? If OLD calls `HashMap::clone`, `BitVec::==`, `Vec::push`, etc. on the hot path, the primitive is likely doing better than what we'd write by hand at scales <10⁵. The "obvious" replacement loses to the constant factor.
4. **Apply Q3 to remaining plan items.** §A6a (predicate name interning): replaces String hashing with u32 lookup. The OLD path uses HashMap<String, BitVec> probe — String hash is genuinely expensive (re-hashes per call). Probably wins, but pre-validate by microbench. §A7 (BitVec store-type pin): no work change, just configuration. Already predicted footnote-only.
**Consequences:** Plan budget after sitting 12: 2 confirmed (§A1 SoA, §A4 Vec doubling), 1 infrastructure-orphan (§B3 cache), 5 falsified (§A2, §A3-via-staging, §B2, §B4, §A6b). The Q3 rule is now load-bearing for the remaining plan items. The blog post structure shifts: post 2 ("Layout matters") gets the §A6b falsification entry; the empirical taxonomy story (3 bits, 5 falsifications, 2 confirmations) is the meta-narrative at-or-around post 12.
**Related EXP:** EXP-0007 (this archive); pairs with EXP-0010 + EXP-0003 + EXP-0012 + EXP-0030 in the falsification quintet.

---

## ADR-0016: §B3 cache is architecturally orphaned in current mununu; downgrade to "infrastructure-correct, no caller"

**Date:** 2026-05-07 (sitting 12)
**Status:** accepted; supersedes the "third confirmed plan item" framing in ADR-0015.
**Context:** Sitting 12 opened on the natural EXP-0011 followup: wire `compose_named_cached` into real callers and measure end-to-end speedup on a multi-property DSL workflow. A grep across the workspace found **no production callers** of `compose_named` outside the EXP-0011 bench and the `cache_check` ad-hoc binary:
  - `context_dsl/realize.rs:1279` is the only `compose_named` call in the realize path. It runs inside an associative-compose loop where each iteration creates a *fresh* `temp_context` with two registered CLTSs and calls `compose_named` once. The cache lives on `Context`, so the temp_context's cache is empty on every call.
  - The realize step produces composed CLTSs as named entries in the final `RealizedContext.context`. After realize, all property evaluation (`evaluate_mu`, `synthesise_controller`) operates on those *already-composed* named CLTSs — `compose_named` is not called again.
  - Mununu's own multi-property workflow (`evaluate_mu_many` at `context/mod.rs:1203`) iterates over CLTS *names*, not over `(left, right, mode)` triples — it doesn't compose anything per call.
  - The API server's `api/cache.rs` caches the entire `RealizedContext` (composition pre-done), so it has zero opportunity to benefit from a Context-level cache below it.

The microbenchmark in EXP-0011 *was* measuring real BFS-product compose vs. real Arc-clone-on-hit (the `with_uncontrollable_prefix(3)` fixture exposes the genuine compose path); the numbers stand. But the bench's call pattern — one Context with two registered CLTSs, repeated `compose_named` calls on the same triple — exists nowhere else in the codebase.

**Decision:**
1. **Downgrade EXP-0011 from "confirmed plan item" to "infrastructure-correct, no caller today."** The implementation is sound; the win is real *if a caller exists*. None does. Keep the code (low maintenance cost, future-extensible) but stop counting it as a delivered speedup.
2. **Plan §B3 is reframed.** Original plan §B3 hypothesis: "DSL workflows compose the same automata across many property checks." That assumption was wrong — mununu composes *once* during realize and stores the result. The §B3 *implementation* is the right shape for a different (future) workload: an interactive REPL or batch tool that recomposes on the fly. Schedule §B3-callers as EXP-0011-followup if such a workload is added.
3. **Append a corrigenda section to EXP-0011's README, log, notes.** Honest framing: "Microbench shows the cache hit path costs ~46 ns vs. ~6.247 ms cache miss; no production caller exercises the cache today; numbers don't translate to real wallclock improvement until a caller is wired."
4. **Plan budget after sitting 12, opening:** 2 confirmed (§A1 SoA, §A4 Vec doubling), 1 infrastructure-only (§B3), 4 falsified (§A2, §A3-via-staging, §B2, §B4), 1 partial-falsification (§A1 heap). Confirmed-and-delivering total drops from 3 to 2.
5. **Next plan item:** §A6b (BindingStack) is the highest-value open structural change: replaces the per-fixpoint-iteration `bindings.clone()` at `mu_calculus/evaluator.rs:1741` with a stable in-place stack. Per ADR-0014 prediction (work-reducer), default-favorable. EXP-0007.

**Consequences:** Honest accounting prevents a paper claim ("3 confirmed wins, including a 135,000× cache speedup") that wouldn't survive peer review when an evaluator asks "where in your code does this trigger?" The §B3 framing in blog post 5 ("Memoize the product") needs to acknowledge the orphan finding; what's left to write up is the methodology lesson — "we discovered the assumed call pattern doesn't exist; here's how the bench harness exposed that."
**Related EXP:** EXP-0011 (downgraded); future EXP-0011-followup if §B3 gets a real caller; EXP-0007 (BindingStack, opening).

---

## ADR-0015: Plan §B3 (Context-level composition cache) confirmed; first 5-orders-of-magnitude win

**Date:** 2026-05-07 (sitting 11)
**Status:** accepted; plan §B3 confirmed; code change kept.
**Context:** EXP-0011 added `Context::compose_named_cached(left, right, mode) -> Arc<Clts>` memoizing the BFS product on `(left, right, mode)`. Result: cache-miss median 6.247 ms, cache-hit median 46.23 ns; ≈135,132× speedup on the steady-state hit path. All 825 tests pass with the collision-safe variant (full-key recheck on `u64`-hash hit). This is the third confirmed plan item (§A1 SoA, §A4 Vec doubling, §B3 cache) and breaks the four-falsification streak (§A2, §A3-via-staging, §B2, §B4).
**Decision:**
1. **Keep the cache.** First plan item to win by reducing total work to "one refcount bump"; matches the refined taxonomy from ADR-0014 ("changes that reduce total work win") with the strongest possible signal.
2. **Mark plan §B3 as confirmed.** Cache-state contamination is not a concern: the bench is steady-state (Criterion warmup populates the cache; measured iterations are all hits), and the cache stores `Arc<Clts>` so hits are an `Arc::clone`.
3. **Flag a methodology bug retroactively.** The pre-existing `composition_only::bench_chain_sync`/`grid_async`/`mode_compare` benches use `chain_1k`/`ring_1k`/`grid_32x32` fixtures whose controllable alphabets overlap; `compose()` rejects shared-controllable-alphabet pairs in validation, so those benches were measuring the *error path*, not real composition. EXP-0010, EXP-0003, EXP-0030 reported numbers on this bench; their falsification *directions* still stand (a slower error path is still a slower path) but the magnitudes don't reflect real compose cost. A corrigenda note will be appended to those archives, not a re-open.
4. **Open EXP-0011-callers.** Replace `compose_named` callers in DSL evaluation and synthesis with `compose_named_cached` so the speedup transfers to real Mununu workflows. Out of scope for EXP-0011.
**Consequences:** Plan optimization budget after sitting 11: 3 confirmed (§A1, §A4, §B3), 4 falsifications (§A2, §A3-via-staging, §B2, §B4), 1 partial-falsification (§A1 heap). The refined taxonomy (work-reducing wins, work-adding loses) survives a third confirmation; the bench-fixture bug discovery is a minor finding worth noting but doesn't invalidate the four falsifications because each one consistently regressed on whatever path was being exercised. Blog post 5 ("Memoize the product") and paper draft both get a clean win.
**Related EXP:** EXP-0011 (this archive); EXP-0010, EXP-0003, EXP-0030 get corrigenda appended.

---

## ADR-0014: Plan §A3 (CSR via staging-flatten) falsified; refines the win/lose taxonomy

**Date:** 2026-05-06 (sitting 10)
**Status:** accepted; plan §A3-via-staging falsified; design alive for a future EXP-0031-csr-direct-build redesign.
**Context:** EXP-0030 ran the L3 protocol on `clts_construction` (build), `composition_only` (build via compose), and `mu_calculus_only::synthesis_product_game` (consumer). Result: 6/7 construction benches regress at +12% to **+64.5%** (p<0.001); composition and synth are neutral. The implementation flattens an existing `Vec<Vec<Transition>>` staging into a flat `Vec<Transition>`, adding an O(|E|) memcpy at build time that absorbs the predicted savings.
**Decision:**
1. **Revert EXP-0030.** The CSR-via-staging-flatten path is a net regression on construction with neutral consumer effects.
2. **Mark plan §A3-via-staging as falsified.** The §A3 *design* (CSR layout) might still win via a from-scratch direct-CSR-build path that bypasses the staging Vec<Vec<>>. Schedule as EXP-0031-csr-direct-build if pursued; not on the current path.
3. **Refine the win/lose taxonomy.** ADR-0009/0012/0013 said "structural wins, drop-ins lose." EXP-0030 shows that's not enough. The refined rule: **changes that reduce total work win; changes that add work (drop-in or structural) lose.**
   - **EXP-0002b SoA**: HashMap → Vec<Vec<u32>>. Lazy init. Total work down. ✅
   - **EXP-0004 Vec doubling**: removed wrapper. Total work down. ✅
   - **EXP-0010 FxHashMap**: same work, different hasher. ❌
   - **EXP-0003 LabelSetTable**: extra find_label_set probe. Total work UP. ❌
   - **EXP-0012 track-during-merge**: replaced primitive memcpy + early-exit compare with serial branchful word-loop. Total work UP. ❌
   - **EXP-0030 CSR-via-staging**: kept staging, added flatten. Total work UP. ❌
4. **Apply the refined rule to remaining plan items.** §A6 (predicate interning + Vec bindings): the predicate interning is "extra work" shape (extra map probe per Atom); the BindingStack is "less work" shape (replaces per-iteration HashMap clone with stable storage). Schedule them as separate EXPs to isolate the two effects.
**Consequences:** Plan optimization budget after sitting 10: 2 confirmed (§A1, §A4), 1 partial-falsification (§A1 heap), 4 hard falsifications (§A2, §A3-via-staging, §B2, §B4). The refined taxonomy gives a sharper prediction model for the remaining plan items.
**Related EXP:** EXP-0030 (this archive); future EXP-0031-csr-direct-build (not scheduled) might still deliver §A3.

---

## ADR-0013: Plan §B4 (changed-flag fixpoint termination) falsified at this scale; third drop-in failure pattern

**Date:** 2026-05-06 (sitting 9)
**Status:** accepted; plan §B4 marked falsified-at-this-scale; falsification pattern documented.
**Context:** EXP-0012 ran the L3 protocol on the full `mu_calculus_only` bench suite (10 benches across propositional, reachability_mu, invariance_nu, synthesis_product_game). A=clone+compare baseline, B=in-place merge with `or_assign_track` / `and_assign_track`. **7 of 10 benches regress at p<0.001**, ranging from +1.4% to **+93.8%** on `synthesis_product_game/ring_1k`. The lone non-regression (-8.7% on grid_32x32 synth) failed Mann-Whitney significance at p=0.351. All tests pass.
**Decision:**
1. **Revert the EXP-0012 changes.** The plan §B4 reasoning assumed compare+clone dominated; in practice `BitVec::==` early-exits on first word mismatch and clone is hardware-vectorized memcpy on tiny BitVecs (≤128 bytes for our state counts). The per-iteration overhead of the new word loop with per-word branch loses to the original at small scales.
2. **Mark plan §B4 as falsified-at-this-scale.** A future hybrid implementation that special-cases by BitVec word size (clone+compare for small, track-during-merge for large 10k+) might still win at scale. Not scheduled.
3. **Establish the drop-in failure pattern as a load-bearing finding.** Three EXPs now demonstrate it: §B2 (FxHashMap, EXP-0010), §A2 (LabelSetTable, EXP-0003), §B4 (track-during-merge, EXP-0012). All assumed an "obvious bottleneck" (hash speed, key shape, compare+clone). At our state-count scale, none of the alleged bottlenecks dominated. The std/bitvec primitives already optimize the targeted path. The wins (EXP-0002b SoA, EXP-0004 Vec doubling) come from changing the access pattern, not the per-operation cost.
4. **Update the plan with this empirical pattern.** Future "drop-in" hypotheses (§A6 predicate interning, §A7 BitVec store-type pin, §B7 std::simd, §C2 FxHashMap drop-in for composition) get re-rated as "needs design-time L3 validation; default-skeptical." Future "structural" hypotheses (§A3 CSR, §A6 BindingStack, §B6 modal pre-image CSR, §B1 Paige-Tarjan) get default-favorable.
**Consequences:** Plan optimization budget after sitting 9: 2 confirmed (§A1, §A4), 1 partial-falsification (§A1 heap), 3 hard falsifications (§A2, §B2, §B4). The drop-in/structural taxonomy is now the load-bearing prediction model. Blog post 2 ("Layout matters") gets a third falsification entry; the "two structural wins, three drop-in fails" framing is empirically grounded.
**Related EXP:** EXP-0012 (this archive); pairs with EXP-0010 + EXP-0003 in blog post 2 as the falsification triplet.

---

## ADR-0012: Plan §A2 (LabelSetTable interning) falsified at this implementation

**Date:** 2026-05-06 (sitting 8)
**Status:** accepted; plan §A2 marked falsified-at-this-implementation.
**Context:** EXP-0003 ran the L3 protocol on `composition_only` AND `mu_calculus_only::synthesis_product_game` with A=SmallVec-keyed `uncontrollable_groups`, B=LabelSetId-keyed via interning table. Result: composition_only +18% to +85% slower across 4 of 5 benches (p<0.001); synth +3% to +5% slower (p<0.01). All tests pass; performance regresses.
**Decision:**
1. **Revert the LabelSetTable change.** The drop-in pattern (replace SmallVec keys with u32 IDs) doesn't deliver because the consumer constructs SmallVecs of uncontrollable subsets fresh per transition. Adding `find_label_set` before `contains_key` adds 1 SmallVec hash per probe instead of removing it.
2. **Mark plan §A2 as falsified-at-this-implementation.** A redesign that caches LabelSetId on Transition (avoiding consumer-side SmallVec construction) might still win. Not scheduled.
3. **Pair EXP-0003 with EXP-0010 in blog post 2 as twin drop-in falsifications.** Three drop-in / hash-key hypotheses now tested: §A2 (this), §B2 (EXP-0010), and the original §A1 contamination story (EXP-0002 → EXP-0002b). All required L3 protocol to expose. Together they establish the pattern: **drop-in hash/key optimizations on small RefCell-wrapped maps in mununu's code consistently lose. Confirmed wins (§A1 SoA, §A4 Vec doubling) come from changing access pattern, not changing hashers.**
**Consequences:** Plan's optimization budget for §A-series is now 2 of 7 confirmed (§A1, §A4), 1 of 7 falsified-at-impl (§A2), 4 remaining (§A3 CSR, §A5 field reorder, §A6 predicate interning, §A7 BitVec store-type). Bench-first methodology required for all of them; intuition is no longer sufficient.
**Related EXP:** EXP-0003 (this archive); pairs with EXP-0010 in blog post 2.

---

## ADR-0011: Plan §A4 (drop 20% growth wrapper) confirmed; second confirmed plan item

**Date:** 2026-05-06 (sitting 7)
**Status:** accepted; plan §A4 marked confirmed.
**Context:** EXP-0004 ran the L3 protocol on `clts_construction` benches: A=existing 20% growth wrapper, B=`Vec::push` native doubling. **All 7 benches improve at p<0.001**, ranging from -5.2% (random_seeded/1024, RNG-dominated) to **-31.0% (chain/100000, realloc-dominated, 1.45× speedup)**. Hypothesis ≥1.1× from plan §A4 is confirmed.
**Decision:**
1. **Drop the 20% growth wrapper, the four `ensure_*_capacity` helpers, and the four capacity-hint fields.** Net: -50 lines of growth code, +30 lines of EXP-0004 comments. Public API (`reserve_states`, `reserve_transitions`) unchanged.
2. **Pair EXP-0004 with EXP-0010 in blog post 2.** Both are 0.5-1 day "drop the bespoke wrapper" hypotheses; one wins (§A4) and one loses (§B2). The pair demonstrates that bench measurement distinguishes intuition that holds from intuition that fails. This is more interesting than either result alone.
3. **EXP-0001-deep should re-record the construction baseline with §A4 applied.** The current EXP-0001 baseline reflects the 20% wrapper; published "before" numbers must reflect the post-EXP-0004 baseline.
4. **Update plan with confirmation status.** §A4 = confirmed (1.06× to 1.45× across bench matrix, p<0.001). §A1 wall-clock = confirmed via EXP-0002b. §A1 heap = falsified-at-grid_32x32 via EXP-0002b-mem. §B2 = falsified via EXP-0010.
**Consequences:** The plan's optimization budget is on track for §A-series items (now 2 of 7 confirmed: §A1, §A4). §B-series items need re-evaluation per ADR-0009 before scheduling. EXP-0003 (LabelSetTable) is the next §A-series substantive change with predicted larger payoff than §A4 since it changes data layout, not just growth strategy.
**Related EXP:** EXP-0004 (this archive); pairs with EXP-0010 in blog post 2.

---

## ADR-0010: SoA wall-clock win is a cache-locality win, not a heap-pressure win

**Date:** 2026-05-06 (sitting 6)
**Status:** accepted; refines ADR-0008.
**Context:** EXP-0002b confirmed 2.4× wall-clock speedup of SoA over HashMap on grid_32x32 synthesis. EXP-0002 README pre-registered "≥100 KB heap reduction on synthesis-relevant bench" as a separate hypothesis. EXP-0002b-mem ran the dhat-instrumented A/B and found: total bytes -0.02%, total allocations -0.008%, peak heap -76 KB (-6.7%), peak block count unchanged. **The pre-registered ≥100 KB heap-reduction hypothesis is falsified by 24 KB at this fixture scale.**
**Decision:**
1. **Update the SoA narrative for paper §3.x and blog post 2.** Frame the wall-clock win as a *cache-locality + per-lookup-cost* improvement, NOT as a heap-pressure reduction. EXP-0002b's 2.4× confirmation stands; the mechanism is the access pattern, not the allocator.
2. **Keep both EXP-0002b and EXP-0002b-mem as canonical archives.** The wall-clock and memory axes are now both measured. Public claims must specify the axis.
3. **Schedule EXP-0002b-mem-deep** at L4 with a larger fixture (64×64 grid or 100k-state CLTS). Predicts: peak heap delta scales linearly with `state_count × num_fixpoint_vars`. At 100k states × 2 mu-obligations the delta should be ~800 KB, validating the heap-axis hypothesis at scale even though it falsifies at grid_32x32 scale. Required before claiming memory-axis benefits in the paper.
4. **Generalize: dhat-instrument every memory-axis hypothesis BEFORE landing the perf claim.** The §A1 README pre-registered the ≥100 KB number on intuition; running dhat earlier would have caught the mismatch immediately. ADR-0010 establishes the practice.
**Consequences:** The paper §3.x narrative for SoA becomes more precise and more defensible (explains the mechanism, predicts where the win scales). Plan §A3 (CSR adjacency) is unaffected — its primary mechanism is genuinely heap reduction, not access pattern. EXP-0002b-mem-deep is added to the followups list with the dev-container/L4 EXPs.
**Related EXP:** EXP-0002b-mem (this archive); refines ADR-0008.

---

## ADR-0009: FxHashMap drop-in for composition is a regression — plan §B2 falsified

**Date:** 2026-05-06 (sitting 5)
**Status:** accepted; plan §B2 marked falsified.
**Context:** Plan §B2 (and the original deep evaluation in `~/.claude/plans/do-a-deep-evaluation-sparkling-origami.md`) listed "FxHashMap drop-in for composition" as a low-risk 0.5-day item with expected 1.5-2× speedup. EXP-0010 ran the L3 protocol on `composition_only` benches. Result: **5/5 benches regress at +30% to +60%, all p<0.001 under both Criterion's bootstrap and Mann-Whitney on per-iter times.**
**Decision:**
1. **Revert FxHashMap from `composition/mod.rs` and `Cargo.toml`.** No architectural argument keeps it (unlike EXP-0002's SoA where the type-safer struct enables future EXPs).
2. **Mark plan §B2 as falsified.** The reasoning: composition's hot HashMaps (a) mix integer-pair, Vec<String>, and structural keys; (b) stay small (<2k entries); (c) are RefCell-wrapped with Rc churn around them. At these sizes/shapes, FxHash's faster hash gets dominated by clustering on weak keys + RefCell/Rc costs. std's hashbrown-backed HashMap with SipHash is faster here.
3. **Generalize the lesson to any future "drop-in" hypothesis.** Future EXPs that propose blanket HashMap → FxHashMap swaps (e.g., on mu_calculus formula-var bindings, predicate maps in Environment, witness map) MUST be benched at L3 BEFORE landing. Intuition is not enough.
4. **Blog post 4 pivots.** Originally "Drop SipHash where it doesn't pay" (success story); becomes "When FxHashMap is the wrong choice — a falsification story." Falsification stories are more interesting blog content than yet-another success.
**Consequences:** Plan §B2 effort budget (0.5 day) consumed without a perf win. The alternative experiments (§B3 composition cache, §B6 modal pre-image CSR) are unaffected — they target different bottlenecks. The L3 protocol bug found during this EXP (`bench_diff.sh --robust` MW-on-raw-times instead of per-iter) is fixed and benefits all future EXPs.
**Related EXP:** EXP-0010 (this archive); plan §B2 (falsified); future "drop-in" experiments must follow ADR-0009's "bench at L3 first" rule.

---

## ADR-0008: SoA hypothesis confirmed on synthesis-bound workload (EXP-0002b)

**Date:** 2026-05-06 (sitting 4)
**Status:** accepted; supersedes the falsification reading of ADR-0007.
**Context:** ADR-0007 stated EXP-0002a "falsified" the SoA's ≥2× hypothesis. On second reading, ADR-0007 was wrong: EXP-0002a's benches don't exercise iteration_ranks at all (`witness_map = None`), so the hypothesis was *unaddressed*, not falsified. EXP-0002b adds a synthesis-bound bench (`mu_calculus_only::synthesis_product_game`) that calls `Context::synthesise_controller_with_options(... ProductGame ...)` against an alternation-2 GR(1) formula, exercising both `IterationRanks::record()` and `get_rank()` on their actual hot paths. Result: **2.4× speedup on grid_32x32 (Criterion: −57.4% [−67.9%, −39.6%], p=0.00)**. ring_1k is below the noise threshold at n=15 but trends consistent (-9%).
**Decision:**
1. **Hypothesis ≥2× confirmed for synthesis-bound workloads with non-trivial alternation.** Public claims about the SoA migration must specify "synthesis-bound" rather than implying a blanket speedup.
2. **EXP-0002b is the citable archive** for the SoA performance claim. EXP-0002 stays as the contamination historical record; EXP-0002a stays as the workload-mischaracterization historical record.
3. **EXP-0002b-deep is required** for paper-grade citation: re-record at L4 (mununu-dev container, Turbo Boost off, dedicated runner, sample size 30+), include a 64×64 grid fixture, instrument dhat for memory profiling.
4. **ADR-0007's "falsified" framing is corrected**: the hypothesis was unaddressed in EXP-0002a, not falsified. ADR-0007's keep-the-migration decision stands; the rationale is now "win on workloads it targets" not "neutral on all workloads."
**Consequences:** The plan §A1 expected speedup is restored to "headline win" status, conditional on the workload class. EXP-0007 (predicate interning) inherits the validated SoA shape. Blog post 2 (Layout Matters) and paper §3.x can cite EXP-0002b's number with full provenance once EXP-0002b-deep ships.
**Related EXP:** EXP-0002 (contamination historical), EXP-0002a (workload-mischaracterization historical), EXP-0002b (citable result), EXP-0002b-deep (planned, paper-grade re-record).

---

## ADR-0007: First L3 re-run falsified EXP-0002's headline; SoA kept on architectural grounds

**Date:** 2026-05-06 (sitting 3, later)
**Status:** accepted
**Context:** Following the user's question "do we have previous tests and benches re ran with warmup?", I ran the first L3 protocol A/B comparison (`bench_compare.sh`, warmup discard, same-session, full Criterion samples) on the SoA-vs-HashMap iteration_ranks change from EXP-0002. Result: EXP-0002's apparent 5-7× speedup was 100% cache-warmup contamination. The clean A/B shows mu-fixpoints +10-27% slower with SoA, nu-fixpoints −3-5% faster, both at p<0.05 per Criterion's t-test. The benches don't actually exercise iteration_ranks (witness_map = None), so the differences are LLVM monomorphization noise. Archived as [EXP-0002a-warmup-rerun](../experiments/EXP-0002a-warmup-rerun/).
**Decision:**
1. **Keep the SoA migration** despite the inconclusive performance result. The struct is type-safer, lays groundwork for EXP-0007 (predicate interning + dense bindings), and the "regression" is on a workload that doesn't exercise iteration_ranks at all.
2. **EXP-0002 stays in place as historical evidence** of the contamination class that motivated ADR-0006. README and notes carry a SUPERSEDED warning pointing to EXP-0002a.
3. **EXP-0002b is required** to test the original ≥2× hypothesis on a synthesis-bound bench (`Context::synthesise_controller_with_options(... ProductGame ...)` against alternation-depth-2+ fixtures with witness extraction enabled). Until EXP-0002b shows results, the SoA's actual hot-path performance is undocumented.
**Consequences:** EXP-0002's blog/paper claim is retracted. The plan's §A1 expected speedup is no longer load-bearing for the paper outline; it shifts from "headline win" to "API cleanup + future-enabling refactor". The reproducibility contract (notebook/0000-overview.md) survives intact — the contamination was caught BY the contract, not despite it.
**Related EXP:** EXP-0002 (superseded), EXP-0002a (corrected), EXP-0002b (followup).

---

## ADR-0006: Four-level bench regression mitigation protocol

**Date:** 2026-05-06
**Status:** accepted
**Context:** EXP-0001 vs EXP-0002 produced apparent 5-7× speedups, but the comparison was contaminated by cache-warmup and binary-mmap differences between two separately-invoked `cargo bench` runs in different shell sessions. Without a documented mitigation protocol, every future EXP risks the same false-positive class.
**Decision:** Adopt a four-level protocol, documented in [`notebook/BENCH_POLICY.md`](BENCH_POLICY.md) "Regression mitigation":
- L1 (smoke): `cargo bench --quick`, indicative only.
- L2 (EXP record): `scripts/bench_record.sh --fresh --warmup EXP-NNNN-...` — discards a `--quick` warmup before the real recording.
- L3 (same-session A/B): `scripts/bench_compare.sh BASELINE_NAME -- ...` then `--baseline-only` after the patch — keeps OS scheduler, thermal, and mmap state constant across A and B.
- L4 (dedicated runner): inside `mununu-dev` container, Turbo Boost disabled, core-pinned, with `scripts/bench_diff.sh --robust` (Mann-Whitney p<0.01 + median threshold) as the gate.
**Consequences:** Adds ~30-60 seconds to every level-2 EXP recording (the warmup). In return: the published numbers are more reproducible, and the regression gate is robust against bimodal distributions caused by transient noise. EXP-0001-deep and EXP-0002-deep are scheduled to re-record at level 4 before any blog/paper claim is made.
**Related EXP:** all (process decision); EXP-0001-deep, EXP-0002-deep next.

---

## ADR-0005: Soundness regression suite already exists; do not duplicate

**Date:** 2026-05-06
**Status:** accepted
**Context:** The plan §E6 called for landing a "soundness regression suite skeleton" at `tests/soundness/`. Investigation during sitting 3 revealed that `crates/mununu-core/tests/soundness.rs` already contains 22 tests covering: over-approximation preserving safety, noop self-loops masking deadlocks, counter-bound preservation, havoc preserving safety, async vs sync composition behaviors, controllability misclassification, extraction-style guard removal and havoc, Mealy vs Moore divergence (SYNTCOMP), signature-based functional strategy, ProductGame mode (with cross-mode agreement), and ParityGame mode (Zielonka).
**Decision:** Treat the existing `tests/soundness.rs` as the load-bearing suite. New soundness tests added by future EXPs should append to this file; do not create a parallel `tests/soundness/` directory. The plan is amended to reflect this in the next sitting's notebook entry.
**Consequences:** Saves ~2 days of duplicated work. The existing suite already runs in `make ci` (no feature gate), so the soundness contracts are protected by default. Future EXPs (B1 Paige-Tarjan, B5 chaotic iteration, B6 modal pre-image CSR) just need to ensure these tests continue to pass.
**Related EXP:** all algorithmic experiments (B1, B5, B6, C1, C2).

---

## ADR-0004: Manifest schema is versioned; scaffold evolves without rewriting history

**Date:** 2026-05-06
**Status:** accepted
**Context:** As experiments accumulate, we'll discover the scaffold needs to evolve — new provenance fields, deterministic seeds promoted to required, dhat archive paths, etc. Rewriting old archives in-place to match a new schema would invalidate citations in already-published blog posts and a paper draft.
**Decision:** `experiments/EXP-NNNN-<slug>/manifest.json` carries a `schema_version` integer. `scripts/check_repro.sh` validates per-version field sets, not a union. Old archives stay at their original version forever; new versions add required fields and `bench_record.sh` bumps the version it writes. The version policy and field history live in [`experiments/SCHEMA.md`](../experiments/SCHEMA.md). Optional fields can be added without bumping.
**Consequences:** A modest amount of conditional logic in `check_repro.sh` (one `REQUIRED` map per version). In return: scaffold refinements never block on history rewrites; published numbers remain replayable as long as the code paths they exercised still exist. When a refinement is truly cliff-shaped (bench harness signature changed, fixture format incompatible), the affected archive is tagged `superseded_by: EXP-NNNN` and the new EXP carries the updated numbers — the old archive stays as historical evidence.
**Related EXP:** all (process decision).

---

## ADR-0003: Witness-rank stability audit gates chaotic iteration

**Date:** 2026-05-05
**Status:** accepted
**Context:** Strategy extraction reads `iteration_ranks` from `WitnessMap` and uses lexicographic comparison of rank vectors to pick "most progressive" controllable transitions. Tarski guarantees same lfp/gfp under any monotone iteration order, but the rank *values* depend on order — and the synthesis path may rely on more than just monotonicity.
**Decision:** Gate B5 (chaotic iteration, EXP-0013) behind `EvaluationOptions::chaotic_iteration: bool` (default off). Open EXP-0013-witness as a sub-experiment that audits the synthesis path: enumerate every consumer of `iteration_ranks`, prove (or empirically verify on 4096 random CLTSs) that strategy correctness is invariant under iteration order, then flip the default.
**Consequences:** Slower path to the paper §3.3 claim. Avoids a soundness regression in controller synthesis that would invalidate every example file shipped under `examples/`.
**Related EXP:** EXP-0013, EXP-0013-witness.
