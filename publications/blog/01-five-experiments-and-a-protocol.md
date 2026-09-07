---
title: "Five experiments and a regression-mitigation protocol — first lessons from optimizing a Rust verifier"
draft: true
target: "Substack — Engineering Notebook"
target_words: 2200
tags: [rust, performance, mu-calculus, formal-methods, benchmarking, reproducibility, falsification]
status: ready_for_review
authors: ["Mariano Cerrutti"]
provenance:
  experiments_referenced: ["EXP-0001-baseline-cliff", "EXP-0002-iter-rank-soa", "EXP-0002a-warmup-rerun", "EXP-0002b-synth-bench", "EXP-0010-fxhash-composition"]
  notebook_entries: ["2026-W19-wed", "2026-W19-thu", "2026-W19-thu-pm", "2026-W19-fri"]
  adrs: ["ADR-0001", "ADR-0006", "ADR-0007", "ADR-0008", "ADR-0009"]
  commit: TBD
---

# Five experiments and a regression-mitigation protocol — first lessons from optimizing a Rust verifier

> Each result in this post is replayable: `make replay EXP=<EXP-ID>`. The numbers cited ship with provenance under [`experiments/`](https://github.com/Mumunu-team/mununu/tree/main/experiments) — commit SHA, container digest, hardware fingerprint, the exact bench command, and the raw Criterion JSON.

This is a notebook entry from two weeks of performance work on [mununu](https://github.com/Mumunu-team/mununu), a formal verifier for compositional labeled transition systems (CLTS). The intended outcome of the work was a set of optimization wins. The actual outcome was mostly methodology: the first candidate appeared to deliver a 5–7× speedup that turned out to be cache contamination, and the rest of the work has been about not repeating that mistake.

This post documents the contamination, the protocol that replaced it, and the results obtained under the protocol — including one confirmed speedup and one falsified hypothesis.

## EXP-0002: an apparent 5–7× speedup

The starting point was a [structured evaluation of mununu's verification engine](https://github.com/Mumunu-team/mununu/blob/main/.claude/plans/do-a-deep-evaluation-sparkling-origami.md) covering CLTS storage, composition, bisimulation minimization, and mu-calculus fixpoint evaluation. It identified roughly twenty candidate optimizations ranked by effort and expected payoff.

The lowest-risk item, §A1, replaced `WitnessMap.iteration_ranks: HashMap<(usize, FormulaVarId), usize>` with a struct-of-arrays `Vec<Vec<u32>>` indexed by `[var.index()][state_idx]`, using `u32::MAX` as the absent sentinel. The motivation was straightforward: contiguous Vec indexing avoids the scatter pattern of HashMap probes, the access pattern is sequential write per fixpoint iteration and sequential read during signature comparison, and the predicted speedup on synthesis-bound benches was at least 2×.

After the change, the existing mu-calculus benches reported the following:

| Bench | EXP-0001 (baseline) | EXP-0002 (SoA) | Apparent ratio |
|-------|--------------------:|---------------:|---------------:|
| `mu_calculus_only/reachability_mu/grid_32x32` | 109 ms | 15.0 ms | 7.3× |
| `mu_calculus_only/invariance_nu/grid_32x32` | 3.1 ms | 506 µs | 6.1× |
| `mu_calculus_only/propositional/grid_32x32` | 78 µs | 12.2 µs | 6.4× |

Two signals indicated the result was wrong before the underlying cause was identified. First, the observed ratios exceeded the pre-registered hypothesis by a factor of three. Second, one of the benches showing a 6× improvement — `propositional` — does not contain a fixpoint, so the changed data structure is not on its hot path.

## What was actually measured

EXP-0001 was recorded during initial scaffolding, the first time the `mu_calculus_only` bench had run on the host. The release binary was fresh, most of its mmap pages were not yet in the page cache, the criterion-managed `target/criterion/` hierarchy did not exist, and the fixtures had not been deserialized.

EXP-0002 ran twenty-four hours later, after several iterative dev cycles had warmed every cache touched by the binary.

The comparison therefore conflated the SoA migration with a transition from cold to warm caches. Most of the apparent speedup was attributable to the second factor. The protocol described below is intended to make this class of confound visible at the point of measurement rather than after publication.

## A four-level regression-mitigation protocol

Cache state, page-fault residency, OS scheduler placement, and thermal state vary between separate `cargo bench` invocations even when the hardware and binary are held constant. Comparing two such runs can produce apparent speedups that disappear once both binaries are warm.

The protocol uses four levels of measurement, each with a defined purpose and a defined limit on how its numbers may be cited.

**Level 1 — smoke.** `cargo bench --quick`. Used during development for directional feedback. Not citable.

**Level 2 — EXP record with warmup.** A `--warmup` flag on the recording script runs the bench once at `--quick` and discards the result before the real recording begins. This pays the page-fault, mmap-load, and binary-cache costs that the first measurement would otherwise absorb. Adds 30–60 seconds per recording. Used when the same binary is benched repeatedly within an EXP.

**Level 3 — same-session A/B.** `bench_compare.sh BASELINE_NAME` saves a Criterion baseline; the patch is applied in the same shell; `bench_compare.sh BASELINE_NAME --baseline-only` measures the candidate against the saved baseline. Same-session execution holds OS scheduler, thermal state, and binary mmap regions approximately constant. This is the minimum level for any externally cited speedup.

**Level 4 — dedicated runner.** Required for paper-grade evidence. Uses the dev container so kernel and glibc match across machines, with Turbo Boost disabled, CPU pinning, and a `--robust` significance gate that runs Mann–Whitney on per-iteration times alongside Criterion's bootstrap.

Each level is recorded as an architectural decision in [ADR-0006](https://github.com/Mumunu-team/mununu/blob/main/notebook/decisions.md). Every published EXP archive states the level it was recorded at; cross-level comparisons carry an explicit caveat.

## EXP-0002a: re-running the original benches under L3

Re-running the `mu_calculus_only` benches under L3 — HashMap baseline against SoA candidate, same shell, same compile cache, n=30 per bench — produced no significant difference. SoA was within ±10% of HashMap on every bench, with no significance under Mann–Whitney. Some benches shifted +25% on Criterion's mean-bootstrap and others shifted −5%; the spread is consistent with LLVM monomorphization noise from the field-type change. The benches do not exercise `iteration_ranks` because the evaluator skips that path when `witness_map = None`.

This was archived as **EXP-0002a-warmup-rerun**. Its README opens by stating that the original ≥2× hypothesis is unaddressed by the archive and that EXP-0002b contains the appropriate test.

## EXP-0002b: testing the workload the change targets

`mu_calculus_only::synthesis_product_game` is a bench function that calls `Context::synthesise_controller_with_options(... ProductGame ...)` against an alternation-2 GR(1)-style formula:

```
nu X. ((mu Y1. (target or <> Y1)) and (mu Y2. (safe or <> Y2)) and [] X)
```

This is the workload that exercises `IterationRanks::record()` during witness-guided fixpoint evaluation and `IterationRanks::get_rank()` during ProductGame controller construction — the access pattern the SoA migration was designed for.

Run under L3:

| Bench | A (HashMap) | B (SoA) | Δ | 95% CI | p |
|-------|------------:|--------:|--:|--------|---:|
| `synthesis_product_game/ring_1k` | 19.1 ms | 17.1 ms | −8.9% | [−25.5%, +6.7%] | 0.44 |
| `synthesis_product_game/grid_32x32` | 2.84 s | 1.21 s | −57.4% | [−67.9%, −39.6%] | 0.00 |

The grid_32x32 case shows a 2.4× speedup (p<0.001), consistent with the original hypothesis. The ring_1k case is directionally favourable but not significant at this sample size.

The differences between EXP-0002 and EXP-0002b are workload selection — EXP-0002b actually exercises the changed data structure — and cache-state control through L3.

## EXP-0010: a falsified hypothesis

A protocol that only confirms expected results provides limited value. The case below is a hypothesis with a strong prior that the protocol rejected.

Plan §B2 specified replacing `HashMap<(StateId, StateId), StateId>` with `FxHashMap` in composition, with an expected 1.5–2× speedup. [`rustc-hash`](https://github.com/rust-lang/rustc-hash) is faster than std's `RandomState`-based HashMap on integer keys, mununu's composition state-pair map is exactly that shape, and SipHash overhead in non-adversarial settings has been documented at length in the literature.

The change passed all 920+ unit tests. Run under L3 with five composition benches and 30 samples per side:

| Bench | std HashMap | FxHashMap | Δ | p |
|-------|------------:|----------:|--:|---:|
| `chain_sync/chain1k_x_ring1k` | 2.61 µs | 3.37 µs | +36.8% | 0.000 |
| `grid_async/grid32_x_grid32` | 1.29 µs | 1.63 µs | +31.1% | 0.000 |
| `mode_compare/sync` | 2.52 µs | 3.49 µs | +40.8% | 0.000 |
| `mode_compare/async` | 2.51 µs | 3.32 µs | +32.7% | 0.000 |
| `mode_compare/superset` | 2.51 µs | 3.61 µs | +59.6% | 0.000 |

Every bench regressed by 30–60%, significant under both Criterion's bootstrap on means and Mann–Whitney on per-iteration times.

The EXP-0010 archive documents three contributing factors. The hot map keys are not all simple integer pairs: `ProductStateBuilder.state_map` is `(StateId, StateId)`, which favours FxHash, but the four arena caches key on `Vec<String>`, `(usize, usize)`, 4-tuples, and pointer pairs, where byte-stream hashing inside `Vec<String>` dominates the per-probe cost. The maps are also small at this scale — composition products typically reach 1000–2000 entries and the arena caches stay smaller — which keeps the std hashbrown table inside L1/L2 and makes the SipHash overhead less significant relative to `RefCell::borrow_mut` and `Rc::clone` costs. Finally, hash quality matters at small sizes: FxHash's multiplicative function can produce more clustering on weak inputs, and at low load factors the additional probe-chain follows cost more than the faster hash function saves.

This is consistent with hashbrown's published benchmarks, which show FxHash beating SipHash on lookups in maps with millions of entries but trading workload-by-workload below a few thousand entries.

The change was reverted. Plan §B2 is recorded as falsified; [ADR-0009](https://github.com/Mumunu-team/mununu/blob/main/notebook/decisions.md) requires future drop-in replacement hypotheses to be benched at L3 before landing.

## A bug in the protocol's robust gate

While running EXP-0010 the two significance estimators disagreed. Criterion's bootstrap reported the regression at p=0.00 across all benches; the `bench_diff.sh --robust` gate, intended to demote false positives from bimodal distributions via Mann–Whitney, reported p=0.7–0.99.

Criterion's `sample.json` stores `times = total wall time for iters iterations`. In linear sampling mode, `iters` ramps up across samples — the first sample runs N iterations, the second 2N, and so on — to produce a long-enough measurement for a stable mean. The Mann–Whitney implementation was comparing raw `times` between baseline and candidate. With matching iter ramps in both arms, it was measuring the ramp shape rather than per-iteration cost, and correctly concluding that the distributions were the same.

The fix is one line: divide each sample by its iteration count before the test. After the fix, Mann–Whitney agrees with Criterion's bootstrap on every bench (p<0.001 for the FxHashMap regressions), and re-running prior EXP archives produces the expected significance levels for the borderline EXP-0002a/b cases.

The bug had been present since the script was written. None of the headline numbers were affected — Criterion's bootstrap is independently correct — but the secondary gate intended to second-guess Criterion was producing the wrong answer in every prior run. The PR landed two changes: the per-iter fix and a code path that prefers Criterion's `change/estimates.json` (mean delta with 95% CI) as the headline number when available.

## Inventory of results

The work to date produced one confirmed speedup, three contamination or methodology archives, and one falsified hypothesis. SoA iteration-ranks delivers 2.4× on grid_32x32 synthesis (EXP-0002b, p<0.001). EXP-0001, EXP-0002, and EXP-0002a remain in the archive as evidence of cold-cache baselining, the resulting phantom speedup, and the workload-mischaracterization rerun. EXP-0010 records the FxHashMap drop-in as a 30–60% regression rather than the expected 1.5–2× win.

What is not yet in place: paper-grade L4 numbers (which require the dedicated runner, dev container, and Turbo-off configuration), memory-axis numbers from dhat instrumentation, and most of the optimization plan's larger items (B1 Paige–Tarjan minimization, B6 modal pre-image CSR, C1 parallel modal evaluation).

## Notes for similar work

Recompile-warm effects are larger than they tend to be assumed. Two `cargo bench` invocations against the same binary in the same shell, separated by half an hour of unrelated work, can differ by 5–7× on the first measurement. A speedup derived from comparing one such measurement to the other is reporting cache state rather than perf state.

Workload selection determines what a bench tests. The original EXP-0002 benches did not exercise the changed data structure on their hot paths; the resulting numbers reflected the migration's effect on unrelated code paths plus cache state. EXP-0002b is less impressive in raw ratio (2.4× rather than 7×) but tests the access pattern the change was designed for.

When two estimators on the same data disagree, the discrepancy is a signal. In the EXP-0010 case the bug was in the secondary gate; the rule generalizes to either direction. Estimators that never disagree are not providing independent information.

The next post in this series covers memory: how dhat-instrumented benches relate to the SoA result, and what role the heap-allocation axis plays in the methodology. Replay commands and provenance archives accompany each post in the public repo.

---

**Reproducibility footer.** This post cites EXP-0001-baseline-cliff, EXP-0002-iter-rank-soa, EXP-0002a-warmup-rerun, EXP-0002b-synth-bench, and EXP-0010-fxhash-composition. Each archive is at `experiments/<EXP-ID>/` in the [mununu](https://github.com/Mumunu-team/mununu) repo and replays via `make replay EXP=<EXP-ID>`. Hardware fingerprints, commit SHAs, container digests, and exact bench commands are committed alongside every result. ADRs 0006–0009 in [`notebook/decisions.md`](https://github.com/Mumunu-team/mununu/blob/main/notebook/decisions.md) record the protocol decisions referenced in this post.

**Tooling.** The L3 protocol is implemented in [`scripts/bench_compare.sh`](https://github.com/Mumunu-team/mununu/blob/main/scripts/bench_compare.sh) and [`scripts/bench_diff.sh`](https://github.com/Mumunu-team/mununu/blob/main/scripts/bench_diff.sh). Both are under 200 lines of shell and Python and depend only on Criterion 0.8 and standard tools (no scipy dependency for Mann–Whitney). The dev container, hardware fingerprint capture, and EXP archive scaffolding are at [`scripts/`](https://github.com/Mumunu-team/mununu/tree/main/scripts) and [`docker/Dockerfile.dev`](https://github.com/Mumunu-team/mununu/blob/main/docker/Dockerfile.dev).
