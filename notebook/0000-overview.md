# Mununu Optimization Programme — Overview

**Author:** Mariano Cerrutti (vscorza@gmail.com)
**Started:** 2026-05-05
**Plan reference:** `~/.claude/plans/do-a-deep-evaluation-sparkling-origami.md`

## Why

Mununu's verification core (CLTS + composition + minimization + mu-calculus + abstraction) is correct and clean but unoptimized. As we push toward million-state CLTSs, deep alternation, and tight DSL workflows that compose the same automata across many properties, the engineering cost of every per-iteration HashMap clone, every `Vec<Vec<Transition>>` indirection, every full `BitVec ==` becomes load-bearing.

This programme is a deliberate, sequenced run at:

- **Memory layout:** SoA, CSR, interning, alignment.
- **Algorithms:** Paige-Tarjan, chaotic iteration, transposed-CSR pre-image.
- **Parallelism:** rayon-based modal eval, BFS-frontier composition, batch property checks.
- **SIMD:** verifying the autovectorized story; manual SIMD only where measured.

It also doubles as a **publication artifact**: every commit produces a reproducible experiment archive that drops directly into a blog post or a future write-up. The blog series targets practitioners.

## Contract

Every published number satisfies the **Reproducibility Contract** in the plan file (§ "Reproducibility Contract"). Single-command replay (`make replay EXP=NNNN`), pinned toolchain (`rust-toolchain.toml` 1.95.0), pinned dev container (`docker/Dockerfile.dev`), deterministic inputs (`rand_chacha::ChaCha20Rng`), Criterion baselines, archived raw outputs, dated lab-notebook entries. CI enforces the contract via `scripts/check_repro.sh` before any blog post or paper artifact is referenced from outside the repo.

## Layout

- `experiments/EXP-NNNN-<slug>/` — one append-only directory per experiment with eight standard files (README, manifest, command, hw-fingerprint, criterion-archive, dhat-archive, log.md, notes.md).
- `notebook/` — this overview, weekly running entries (`YYYY-WW-day.md`), ADR log (`decisions.md`).
- `publications/blog/` — public-default blog drafts (one per EXP-ID or grouping).
- `publications/paper/outline.md` — paper outline; LaTeX in `mununu-private/paper/`.
- `scripts/{capture_hw,bench_record,repro,bench_diff,check_repro,plot_speedup}.sh|.py` — replay infrastructure.
- `crates/mununu-core/benches/_common.rs` — provenance recorder + fixture loader shared by all benches.
- `crates/mununu-core/src/test_support.rs` — deterministic CLTS generators (gated by `test_support` feature).

## Sequencing

Per the plan:

1. **EXP-0001-prep** — scaffolding (this commit).
2. **EXP-0001** — baseline freeze across all five subsystems.
3. **EXP-0002 → 0008** — memory & layout (blog).
4. **EXP-0009 → 0014** — algorithms (paper headlines).
5. **EXP-0015 → 0017** — parallelism (paper scaling study).
6. **EXP-0013 (gated) + 0013-witness** — chaotic iteration with witness-rank audit.

Total target: ~45 engineer-days. First reproducible result: week 1.

## Soundness posture

Every algorithmic change must:

1. Preserve the documented `// SOUNDNESS:` boundaries (E6 makes them executable).
2. Pass differential tests against the naive oracle (E5).
3. Ship with a soundness argument in its EXP `README.md` citing the relevant theorem (Tarski, Paige-Tarjan Thm 3.3, Hopcroft uniqueness, Bruns-Godefroid OOB).

Witness-extraction paths (controller synthesis, counterstrategy) are higher-bar: any change that touches `iteration_ranks` semantics is gated behind a feature flag until a separate audit experiment closes.

## Conventions

- Lab-notebook entries are append-only. To revise, append a "REVISED YYYY-MM-DD" note with the new claim. To withdraw, append "WITHDRAWN YYYY-MM-DD: <reason>".
- Dead-ends are recorded with as much weight as wins. The blog series and paper §5 explicitly cite rejected alternatives.
- Every artifact carries provenance (commit SHA, container digest, hardware manifest). Numbers without provenance are not citable.
